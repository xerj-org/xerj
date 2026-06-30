//! Log query engine.
//!
//! Supports time-range queries with field filters and aggregations.
//! Skips entire time partitions when min/max metadata proves they cannot
//! contribute to the result.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use xerj_common::XerjError;

use crate::ingest::{HourBucket, LogIngester, LogRecord};

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// Query types
// ─────────────────────────────────────────────────────────────────────────────

/// A filter clause applied to log records.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Filter {
    /// Match records with this log level.
    Level { value: String },
    /// Match records where a field equals a value.
    FieldEquals { field: String, value: Value },
    /// Match records where a field contains a substring.
    FieldContains { field: String, substring: String },
    /// Match records where a numeric field is in a range.
    FieldRange {
        field: String,
        min: Option<f64>,
        max: Option<f64>,
    },
}

impl Filter {
    fn matches(&self, record: &LogRecord) -> bool {
        match self {
            Filter::Level { value } => {
                record.level.as_deref().map_or(false, |l| {
                    l.eq_ignore_ascii_case(value)
                })
            }
            Filter::FieldEquals { field, value } => {
                record.fields.get(field).map_or(false, |v| v == value)
            }
            Filter::FieldContains { field, substring } => {
                record.fields.get(field).and_then(|v| v.as_str()).map_or(
                    false,
                    |s| s.contains(substring.as_str()),
                )
            }
            Filter::FieldRange { field, min, max } => {
                record
                    .fields
                    .get(field)
                    .and_then(|v| v.as_f64())
                    .map_or(false, |n| {
                        min.map_or(true, |m| n >= m) && max.map_or(true, |m| n <= m)
                    })
            }
        }
    }
}

/// The aggregation to compute over matching records.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Aggregation {
    /// Count of matching records.
    Count,
    /// Sum of a numeric field.
    Sum { field: String },
    /// Average of a numeric field.
    Avg { field: String },
    /// Minimum of a numeric field.
    Min { field: String },
    /// Maximum of a numeric field.
    Max { field: String },
    /// Count by time bucket (size in seconds).
    DateHistogram { interval_secs: u64 },
    /// Count by field value.
    Terms { field: String, size: usize },
}

/// A complete log query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogQuery {
    /// Start of the time range (Unix nanoseconds, inclusive).
    pub from: i64,
    /// End of the time range (Unix nanoseconds, inclusive).
    pub to: i64,
    /// Optional filter chain (all must match — implicit AND).
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Aggregation to compute (none = return raw records).
    pub aggregation: Option<Aggregation>,
    /// Maximum raw records to return (only when `aggregation` is None).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    1000
}

impl LogQuery {
    pub fn new(from: i64, to: i64) -> Self {
        Self {
            from,
            to,
            filters: vec![],
            aggregation: None,
            limit: default_limit(),
        }
    }

    pub fn with_level(mut self, level: impl Into<String>) -> Self {
        self.filters.push(Filter::Level { value: level.into() });
        self
    }

    pub fn with_aggregation(mut self, agg: Aggregation) -> Self {
        self.aggregation = Some(agg);
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Query result
// ─────────────────────────────────────────────────────────────────────────────

/// The result of a log query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    /// Total records matched (before limit).
    pub total: u64,
    /// Raw records (when no aggregation was requested).
    pub records: Vec<LogRecord>,
    /// Aggregation output (when an aggregation was requested).
    pub aggregations: HashMap<String, AggregationResult>,
    /// Time taken in milliseconds.
    pub took_ms: u64,
}

/// A single aggregation result value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregationResult {
    pub value: Option<f64>,
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub key: String,
    pub doc_count: u64,
    pub value: Option<f64>,
}

// ─────────────────────────────────────────────────────────────────────────────
// LogQueryExecutor
// ─────────────────────────────────────────────────────────────────────────────

/// Executes log queries against a [`LogIngester`].
pub struct LogQueryExecutor<'a> {
    ingester: &'a LogIngester,
}

impl<'a> LogQueryExecutor<'a> {
    pub fn new(ingester: &'a LogIngester) -> Self {
        Self { ingester }
    }

    /// Execute a query and return results.
    pub fn execute(&self, query: &LogQuery) -> Result<QueryResult> {
        let start = std::time::Instant::now();

        // Validate
        if query.from > query.to {
            return Err(XerjError::invalid_query("from > to in time range"));
        }

        // Determine which hour buckets overlap with the time range
        let from_secs = query.from / 1_000_000_000;
        let to_secs = query.to / 1_000_000_000;
        let from_bucket = HourBucket::from_timestamp_secs(from_secs);
        let to_bucket = HourBucket::from_timestamp_secs(to_secs);

        // Collect matching records from all relevant buckets
        let mut matching: Vec<LogRecord> = Vec::new();
        for bucket in self.ingester.active_buckets() {
            // Block-skip: skip buckets entirely outside the time range
            if bucket < from_bucket || bucket > to_bucket {
                continue;
            }

            if let Some(records) = self.ingester.read_bucket(bucket) {
                for record in records {
                    // Time filter
                    if record.timestamp < query.from || record.timestamp > query.to {
                        continue;
                    }
                    // Field filters
                    if !query.filters.iter().all(|f| f.matches(&record)) {
                        continue;
                    }
                    matching.push(record);
                }
            }
        }

        let total = matching.len() as u64;
        let took_ms = start.elapsed().as_millis() as u64;

        // Build result
        match &query.aggregation {
            None => {
                let records = matching.into_iter().take(query.limit).collect();
                Ok(QueryResult {
                    total,
                    records,
                    aggregations: HashMap::new(),
                    took_ms,
                })
            }
            Some(agg) => {
                let agg_result = self.compute_aggregation(agg, &matching)?;
                Ok(QueryResult {
                    total,
                    records: vec![],
                    aggregations: {
                        let mut m = HashMap::new();
                        m.insert("result".to_owned(), agg_result);
                        m
                    },
                    took_ms,
                })
            }
        }
    }

    fn compute_aggregation(
        &self,
        agg: &Aggregation,
        records: &[LogRecord],
    ) -> Result<AggregationResult> {
        match agg {
            Aggregation::Count => Ok(AggregationResult {
                value: Some(records.len() as f64),
                buckets: vec![],
            }),

            Aggregation::Sum { field } => {
                let sum: f64 = records
                    .iter()
                    .filter_map(|r| r.fields.get(field).and_then(Value::as_f64))
                    .sum();
                Ok(AggregationResult {
                    value: Some(sum),
                    buckets: vec![],
                })
            }

            Aggregation::Avg { field } => {
                let vals: Vec<f64> = records
                    .iter()
                    .filter_map(|r| r.fields.get(field).and_then(Value::as_f64))
                    .collect();
                let avg = if vals.is_empty() {
                    None
                } else {
                    Some(vals.iter().sum::<f64>() / vals.len() as f64)
                };
                Ok(AggregationResult { value: avg, buckets: vec![] })
            }

            Aggregation::Min { field } => {
                let min = records
                    .iter()
                    .filter_map(|r| r.fields.get(field).and_then(Value::as_f64))
                    .fold(f64::INFINITY, f64::min);
                Ok(AggregationResult {
                    value: if min.is_infinite() { None } else { Some(min) },
                    buckets: vec![],
                })
            }

            Aggregation::Max { field } => {
                let max = records
                    .iter()
                    .filter_map(|r| r.fields.get(field).and_then(Value::as_f64))
                    .fold(f64::NEG_INFINITY, f64::max);
                Ok(AggregationResult {
                    value: if max.is_infinite() { None } else { Some(max) },
                    buckets: vec![],
                })
            }

            Aggregation::DateHistogram { interval_secs } => {
                let mut counts: HashMap<i64, u64> = HashMap::new();
                let interval_ns = (*interval_secs as i64) * 1_000_000_000;
                for r in records {
                    let bucket_ts = (r.timestamp / interval_ns) * interval_ns;
                    *counts.entry(bucket_ts).or_insert(0) += 1;
                }
                let mut buckets: Vec<Bucket> = counts
                    .into_iter()
                    .map(|(ts, count)| Bucket {
                        key: ts.to_string(),
                        doc_count: count,
                        value: None,
                    })
                    .collect();
                buckets.sort_by_key(|b| b.key.parse::<i64>().unwrap_or(0));
                Ok(AggregationResult { value: None, buckets })
            }

            Aggregation::Terms { field, size } => {
                let mut counts: HashMap<String, u64> = HashMap::new();
                for r in records {
                    let key = match r.fields.get(field) {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) => v.to_string(),
                        None => {
                            // Also check top-level level/message fields
                            if field == "level" {
                                r.level.clone().unwrap_or_else(|| "UNKNOWN".to_owned())
                            } else {
                                continue;
                            }
                        }
                    };
                    *counts.entry(key).or_insert(0) += 1;
                }
                let mut buckets: Vec<Bucket> = counts
                    .into_iter()
                    .map(|(key, count)| Bucket {
                        key,
                        doc_count: count,
                        value: None,
                    })
                    .collect();
                buckets.sort_by(|a, b| b.doc_count.cmp(&a.doc_count));
                buckets.truncate(*size);
                Ok(AggregationResult { value: None, buckets })
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(offset_secs: i64) -> i64 {
        // Base timestamp: 2023-11-14 00:00:00 UTC in nanoseconds
        1_699_920_000_000_000_000i64 + offset_secs * 1_000_000_000
    }

    fn setup_ingester() -> LogIngester {
        let ingester = LogIngester::new();
        let records = vec![
            LogRecord { timestamp: ts(0), level: Some("INFO".into()), message: Some("start".into()), fields: HashMap::new() },
            LogRecord { timestamp: ts(10), level: Some("WARN".into()), message: Some("slow".into()), fields: {
                let mut m = HashMap::new();
                m.insert("latency_ms".into(), Value::from(150.0));
                m
            }},
            LogRecord { timestamp: ts(20), level: Some("ERROR".into()), message: Some("fail".into()), fields: {
                let mut m = HashMap::new();
                m.insert("latency_ms".into(), Value::from(5000.0));
                m
            }},
            LogRecord { timestamp: ts(30), level: Some("INFO".into()), message: Some("done".into()), fields: HashMap::new() },
        ];
        for r in records {
            ingester.ingest(r).unwrap();
        }
        ingester
    }

    #[test]
    fn count_all() {
        let ingester = setup_ingester();
        let exec = LogQueryExecutor::new(&ingester);
        let q = LogQuery::new(ts(-1), ts(60)).with_aggregation(Aggregation::Count);
        let result = exec.execute(&q).unwrap();
        assert_eq!(result.aggregations["result"].value, Some(4.0));
    }

    #[test]
    fn filter_by_level() {
        let ingester = setup_ingester();
        let exec = LogQueryExecutor::new(&ingester);
        let q = LogQuery::new(ts(-1), ts(60)).with_level("INFO");
        let result = exec.execute(&q).unwrap();
        assert_eq!(result.total, 2);
    }

    #[test]
    fn sum_aggregation() {
        let ingester = setup_ingester();
        let exec = LogQueryExecutor::new(&ingester);
        let q = LogQuery::new(ts(-1), ts(60))
            .with_aggregation(Aggregation::Sum { field: "latency_ms".into() });
        let result = exec.execute(&q).unwrap();
        assert_eq!(result.aggregations["result"].value, Some(5150.0));
    }

    #[test]
    fn max_aggregation() {
        let ingester = setup_ingester();
        let exec = LogQueryExecutor::new(&ingester);
        let q = LogQuery::new(ts(-1), ts(60))
            .with_aggregation(Aggregation::Max { field: "latency_ms".into() });
        let result = exec.execute(&q).unwrap();
        assert_eq!(result.aggregations["result"].value, Some(5000.0));
    }

    #[test]
    fn terms_aggregation() {
        let ingester = setup_ingester();
        let exec = LogQueryExecutor::new(&ingester);
        let q = LogQuery::new(ts(-1), ts(60))
            .with_aggregation(Aggregation::Terms { field: "level".into(), size: 5 });
        let result = exec.execute(&q).unwrap();
        let buckets = &result.aggregations["result"].buckets;
        // INFO appears twice, should be first
        assert_eq!(buckets[0].key, "INFO");
        assert_eq!(buckets[0].doc_count, 2);
    }

    #[test]
    fn invalid_time_range() {
        let ingester = LogIngester::new();
        let exec = LogQueryExecutor::new(&ingester);
        let q = LogQuery::new(ts(100), ts(0)); // from > to
        assert!(exec.execute(&q).is_err());
    }
}
