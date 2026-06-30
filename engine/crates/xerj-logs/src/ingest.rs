//! Log ingestion pipeline.
//!
//! Accepts structured (JSON) log records, routes them to time-partitioned
//! columnar memtables, and extracts message templates for pattern analytics.

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};
use xerj_common::XerjError;

use crate::columnar::{Column, ColumnType, ColumnValue};

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// LogRecord
// ─────────────────────────────────────────────────────────────────────────────

/// A structured log record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    /// Unix epoch nanoseconds. Required.
    pub timestamp: i64,
    /// Log level (INFO, WARN, ERROR, DEBUG, TRACE, FATAL).
    pub level: Option<String>,
    /// Human-readable message body.
    pub message: Option<String>,
    /// Additional structured fields.
    #[serde(flatten)]
    pub fields: HashMap<String, Value>,
}

impl LogRecord {
    /// Parse from a JSON byte slice.
    pub fn from_json(data: &[u8]) -> Result<Self> {
        serde_json::from_slice(data).map_err(|e| {
            XerjError::serialization(format!("log record parse: {e}"))
        })
    }

    /// Parse from a JSON string.
    pub fn from_json_str(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|e| {
            XerjError::serialization(format!("log record parse: {e}"))
        })
    }

    /// Create a simple record with the current time.
    pub fn now(level: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now().timestamp_nanos_opt().unwrap_or(0),
            level: Some(level.into()),
            message: Some(message.into()),
            fields: HashMap::new(),
        }
    }

    /// The UTC hour bucket this record belongs to (for partitioning).
    pub fn hour_bucket(&self) -> HourBucket {
        let dt = DateTime::from_timestamp(
            self.timestamp / 1_000_000_000,
            (self.timestamp % 1_000_000_000) as u32,
        )
        .unwrap_or_default();
        HourBucket {
            year: dt.year() as u16,
            month: dt.month() as u8,
            day: dt.day() as u8,
            hour: dt.hour() as u8,
        }
    }
}

/// Identifies a 1-hour time partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HourBucket {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
}

impl HourBucket {
    pub fn from_timestamp_secs(ts: i64) -> Self {
        let dt = DateTime::from_timestamp(ts, 0).unwrap_or_default();
        Self {
            year: dt.year() as u16,
            month: dt.month() as u8,
            day: dt.day() as u8,
            hour: dt.hour() as u8,
        }
    }
}

impl std::fmt::Display for HourBucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}",
            self.year, self.month, self.day, self.hour
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Template extractor
// ─────────────────────────────────────────────────────────────────────────────

/// Extracts parameterized templates from log messages.
///
/// Example: `"ERROR [myservice] db_write failed for user_42"`
///          → `"ERROR [{}] {} failed for {}"`
///
/// Templates enable grouping similar log lines for pattern analytics.
pub fn extract_template(message: &str) -> String {
    // Simple heuristic: replace tokens that look like variables
    // (numbers, UUIDs, file paths, bracketed identifiers) with `{}`
    let mut parts: Vec<&str> = message.split_whitespace().collect();
    for part in &mut parts {
        if looks_like_variable(part) {
            *part = "{}";
        }
    }
    parts.join(" ")
}

fn looks_like_variable(token: &str) -> bool {
    // Pure number
    if token.parse::<f64>().is_ok() {
        return true;
    }
    // Hex-like (e.g., 0x1a2b, memory addresses)
    if token.starts_with("0x") || token.starts_with("0X") {
        return true;
    }
    // UUID-shaped: 8-4-4-4-12
    let stripped = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
    let parts: Vec<&str> = stripped.split('-').collect();
    if parts.len() == 5 {
        let lens: Vec<usize> = parts.iter().map(|p| p.len()).collect();
        if lens == [8, 4, 4, 4, 12] {
            return true;
        }
    }
    // Bracketed tokens: [user_42], [myhost.example.com]
    if token.starts_with('[') && token.ends_with(']') {
        return true;
    }
    // Contains path separator
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Memtable
// ─────────────────────────────────────────────────────────────────────────────

/// A mutable in-memory partition for one time bucket.
struct Memtable {
    bucket: HourBucket,
    /// Core columns always present.
    timestamps: Vec<i64>,
    levels: Vec<String>,
    messages: Vec<String>,
    templates: Vec<String>,
    /// Dynamic extra fields.
    extra: HashMap<String, Vec<Value>>,
    row_count: usize,
}

impl Memtable {
    fn new(bucket: HourBucket) -> Self {
        Self {
            bucket,
            timestamps: Vec::new(),
            levels: Vec::new(),
            messages: Vec::new(),
            templates: Vec::new(),
            extra: HashMap::new(),
            row_count: 0,
        }
    }

    fn push(&mut self, record: &LogRecord) {
        let template = record
            .message
            .as_deref()
            .map(extract_template)
            .unwrap_or_default();

        self.timestamps.push(record.timestamp);
        self.levels
            .push(record.level.clone().unwrap_or_else(|| "UNKNOWN".to_owned()));
        self.messages
            .push(record.message.clone().unwrap_or_default());
        self.templates.push(template);

        // For each existing extra field, pad with null if this record doesn't have it
        let current_row = self.row_count;
        for (k, values) in &mut self.extra {
            if values.len() <= current_row {
                // Fill any gap with nulls
                while values.len() < current_row {
                    values.push(Value::Null);
                }
                // If record has this field, add it; otherwise null
                if let Some(v) = record.fields.get(k) {
                    values.push(v.clone());
                } else {
                    values.push(Value::Null);
                }
            }
        }

        // Add any new fields from this record (backfill previous rows with null)
        for (k, v) in &record.fields {
            if !self.extra.contains_key(k) {
                let mut values = vec![Value::Null; current_row];
                values.push(v.clone());
                self.extra.insert(k.clone(), values);
            }
        }

        self.row_count += 1;
    }

    fn len(&self) -> usize {
        self.row_count
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LogIngester
// ─────────────────────────────────────────────────────────────────────────────

/// Ingests structured log records into time-partitioned memtables.
///
/// Thread-safe — multiple writers can call [`ingest`] concurrently.
pub struct LogIngester {
    /// Active memtable per hour bucket.
    memtables: Mutex<HashMap<HourBucket, Memtable>>,
    /// Maximum records per memtable before flush is triggered.
    flush_threshold: usize,
    /// Total records ingested since creation.
    total_ingested: Arc<Mutex<u64>>,
}

impl LogIngester {
    pub fn new() -> Self {
        Self::with_flush_threshold(100_000)
    }

    pub fn with_flush_threshold(threshold: usize) -> Self {
        Self {
            memtables: Mutex::new(HashMap::new()),
            flush_threshold: threshold,
            total_ingested: Arc::new(Mutex::new(0)),
        }
    }

    /// Ingest a single log record.
    pub fn ingest(&self, record: LogRecord) -> Result<()> {
        let bucket = record.hour_bucket();
        let mut tables = self.memtables.lock().unwrap();
        let table = tables.entry(bucket).or_insert_with(|| Memtable::new(bucket));
        table.push(&record);

        *self.total_ingested.lock().unwrap() += 1;

        if table.len() >= self.flush_threshold {
            debug!("memtable for {} reached flush threshold", bucket);
            // In a real system, this would trigger an async flush to storage.
            // Here we log and leave the data in place.
        }

        Ok(())
    }

    /// Ingest multiple records, parsed from a newline-delimited JSON stream.
    pub fn ingest_ndjson(&self, ndjson: &str) -> Result<usize> {
        let mut count = 0;
        for line in ndjson.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match LogRecord::from_json_str(line) {
                Ok(record) => {
                    self.ingest(record)?;
                    count += 1;
                }
                Err(e) => {
                    warn!("failed to parse log line: {e}");
                }
            }
        }
        Ok(count)
    }

    /// Return all active bucket identifiers.
    pub fn active_buckets(&self) -> Vec<HourBucket> {
        let tables = self.memtables.lock().unwrap();
        let mut buckets: Vec<HourBucket> = tables.keys().copied().collect();
        buckets.sort();
        buckets
    }

    /// Total records ingested.
    pub fn total_ingested(&self) -> u64 {
        *self.total_ingested.lock().unwrap()
    }

    /// Read records from a specific bucket (for testing / query layer).
    pub fn read_bucket(&self, bucket: HourBucket) -> Option<Vec<LogRecord>> {
        let tables = self.memtables.lock().unwrap();
        let table = tables.get(&bucket)?;
        let records = (0..table.row_count)
            .map(|i| {
                // Reconstruct extra fields for this row index
                let mut fields = HashMap::new();
                for (k, values) in &table.extra {
                    if let Some(v) = values.get(i) {
                        fields.insert(k.clone(), v.clone());
                    }
                }
                LogRecord {
                    timestamp: table.timestamps[i],
                    level: Some(table.levels[i].clone()),
                    message: Some(table.messages[i].clone()),
                    fields,
                }
            })
            .collect();
        Some(records)
    }
}

impl Default for LogIngester {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(ts_nanos: i64, level: &str, msg: &str) -> LogRecord {
        LogRecord {
            timestamp: ts_nanos,
            level: Some(level.to_owned()),
            message: Some(msg.to_owned()),
            fields: HashMap::new(),
        }
    }

    #[test]
    fn ingest_and_count() {
        let ingester = LogIngester::new();
        let base = 1_700_000_000_000_000_000i64;
        for i in 0..10 {
            ingester
                .ingest(make_record(base + i * 1_000_000_000, "INFO", "test"))
                .unwrap();
        }
        assert_eq!(ingester.total_ingested(), 10);
    }

    #[test]
    fn hour_partitioning() {
        let ingester = LogIngester::new();
        // Two records in different hours
        let hour1 = 1_700_000_000_000_000_000i64; // some time
        let hour2 = hour1 + 3_600_000_000_000i64; // +1 hour
        ingester.ingest(make_record(hour1, "INFO", "a")).unwrap();
        ingester.ingest(make_record(hour2, "INFO", "b")).unwrap();

        let buckets = ingester.active_buckets();
        assert_eq!(buckets.len(), 2, "should have two distinct hour buckets");
    }

    #[test]
    fn template_extraction() {
        assert_eq!(
            extract_template("ERROR user 12345 failed auth"),
            "ERROR user {} failed auth"
        );
        assert_eq!(
            extract_template("write to /var/log/app.log failed"),
            "write to {} failed"
        );
    }

    #[test]
    fn ndjson_ingest() {
        let ingester = LogIngester::new();
        let data = r#"{"timestamp":1700000000000000000,"level":"INFO","message":"hello"}
{"timestamp":1700000001000000000,"level":"WARN","message":"world"}
"#;
        let count = ingester.ingest_ndjson(data).unwrap();
        assert_eq!(count, 2);
        assert_eq!(ingester.total_ingested(), 2);
    }

    #[test]
    fn ndjson_skips_bad_lines() {
        let ingester = LogIngester::new();
        let data = "not-json\n{\"timestamp\":1700000000000000000,\"level\":\"INFO\",\"message\":\"ok\"}\n";
        let count = ingester.ingest_ndjson(data).unwrap();
        assert_eq!(count, 1);
    }
}
