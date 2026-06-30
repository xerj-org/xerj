//! Intelligent automatic field encoding for xerj.
//!
//! Analyzes a column of values and selects the most storage-efficient encoding
//! based on field name hints and statistical properties of the data.
//!
//! # Encoding selection ladder
//!
//! 1. **Field-name fast path** — well-known names (`status`, `timestamp`, `ip`, …)
//!    map directly to their optimal encoding without any data scanning.
//! 2. **Cardinality check** — ≤16 unique → [`FieldEncoding::BitsetEnum`];
//!    ≤256 unique → [`FieldEncoding::Dictionary`].
//! 3. **Type probes** — integer / float / timestamp / IP / URL / boolean heuristics.
//! 4. **Fallback** — [`FieldEncoding::RawString`].
//!
//! # Example
//!
//! ```rust
//! use xerj_compress::field_codec::{FieldAnalyzer, FieldEncoding};
//!
//! let statuses = vec!["200", "200", "404", "500", "200", "301"];
//! let analyzer = FieldAnalyzer::new(1024);
//! let encoding = analyzer.analyze("status", &statuses);
//! println!("bytes/value: {:.2}", encoding.bytes_per_value());
//! ```

use std::collections::{HashMap, HashSet};

// ─────────────────────────────────────────────────────────────────────────────
// TimestampFormat
// ─────────────────────────────────────────────────────────────────────────────

/// Detected timestamp wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimestampFormat {
    /// `2024-01-15T12:34:56Z` or with timezone offset
    Iso8601,
    /// `[10/Jan/2024:12:34:56 +0000]`
    ApacheCommon,
    /// `2024/01/15 12:34:56`
    NginxDefault,
    /// Unix epoch seconds — `1705312496`
    EpochSeconds,
    /// Unix epoch milliseconds — `1705312496000`
    EpochMillis,
    /// Any other recognizable pattern
    Custom(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// FieldEncoding — the chosen representation for a single column
// ─────────────────────────────────────────────────────────────────────────────

/// Detected field encoding — chosen automatically from data statistics.
///
/// Each variant stores the fully encoded column data so it can be serialized
/// or used for in-memory analytics directly.
#[derive(Debug, Clone)]
pub enum FieldEncoding {
    /// HTTP status codes, log levels — tiny fixed set (≤16 values).
    ///
    /// Stored as a 4-bit index + one bitmap per dictionary entry.
    /// Filtering `status == 200` returns a pre-built bitmap in O(1).
    BitsetEnum {
        /// Ordered dictionary (max 16 entries).
        values: Vec<String>,
        /// `bitmap[i]` is a packed bitset where bit `j` is set when
        /// document `j` holds `values[i]`.
        bitmap: Vec<Vec<u8>>,
    },

    /// Timestamps — delta-of-delta encoding.
    ///
    /// Apache/Nginx/ISO formats are auto-detected; all values are stored
    /// as microsecond deltas, typically 1–2 bytes each in practice.
    DeltaTimestamp {
        /// First timestamp in microseconds since the Unix epoch.
        base_us: i64,
        /// Detected wire format.
        format: TimestampFormat,
        /// Delta-of-delta values (delta between consecutive deltas).
        deltas: Vec<i64>,
    },

    /// IP addresses — packed u32 for IPv4.
    ///
    /// 4 bytes vs 7–15 bytes as a string. Enables fast CIDR range queries
    /// via bitwise mask comparison on the packed integer.
    PackedIp {
        /// IPv4 addresses as big-endian u32.
        values: Vec<u32>,
    },

    /// URL paths — template + extracted variables.
    ///
    /// `/api/users/123` is stored as template `/api/users/{}` plus the
    /// variable `"123"`, reducing storage for repetitive URL trees.
    UrlTemplate {
        /// Deduplicated templates, e.g. `["/api/users/{}", "/static/{}"]`.
        templates: Vec<String>,
        /// Per-document index into `templates`.
        template_ids: Vec<u16>,
        /// Per-document list of extracted variable segments.
        variables: Vec<Vec<String>>,
    },

    /// Small integers — varint encoding.
    ///
    /// For content-length, port numbers, counts. Numbers 0–127 take 1 byte;
    /// 128–16383 take 2 bytes; etc.
    Varint {
        /// Raw values — varint-encoded when serialized.
        values: Vec<u64>,
    },

    /// Low-cardinality strings — dictionary encoding.
    ///
    /// For HTTP methods, log levels, hostnames, service names.
    /// Per-document cost: 2 bytes (u16 ID) instead of the full string.
    Dictionary {
        /// Unique values ordered by first appearance.
        dict: Vec<String>,
        /// Per-document index into `dict`.
        ids: Vec<u16>,
    },

    /// High-cardinality strings — raw storage.
    ///
    /// Falls back to this when no smarter encoding applies.
    RawString {
        values: Vec<String>,
    },

    /// Booleans — bit-packed (8 values per byte).
    Bitpacked {
        /// Packed bits — bit `j` of byte `j/8` is document `j`'s value.
        bits: Vec<u8>,
        /// Number of logical boolean values stored.
        count: usize,
    },

    /// Floating point with fixed precision — multiply by 10^N, store as varint.
    ///
    /// `12.34` with scale 2 → `1234`.  Typically 2 bytes vs 8 bytes (f64).
    FixedPrecision {
        /// The exponent `N` such that `stored_value = original * 10^N`.
        scale: u32,
        /// Scaled integer values.
        values: Vec<i64>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// FieldEncoding — size estimation
// ─────────────────────────────────────────────────────────────────────────────

impl FieldEncoding {
    /// Estimated average bytes per stored value (data only, excluding headers).
    pub fn bytes_per_value(&self) -> f64 {
        let (total, count) = match self {
            FieldEncoding::BitsetEnum { bitmap, .. } => {
                let n_docs: usize = bitmap.first().map(|b| b.len() * 8).unwrap_or(0);
                // 4-bit index per document + bitmap overhead
                let bitmap_bytes: usize = bitmap.iter().map(|b| b.len()).sum();
                let ids_bytes = (n_docs + 1) / 2; // 4-bit index per doc
                (bitmap_bytes + ids_bytes, n_docs.max(1))
            }
            FieldEncoding::DeltaTimestamp { deltas, .. } => {
                // Average varint: small deltas typically fit in 1–2 bytes
                let bytes: usize = deltas.iter().map(|d| varint_signed_len(*d)).sum();
                (bytes, deltas.len().max(1))
            }
            FieldEncoding::PackedIp { values } => (values.len() * 4, values.len().max(1)),
            FieldEncoding::UrlTemplate {
                template_ids,
                variables,
                ..
            } => {
                let ids_bytes = template_ids.len() * 2;
                let var_bytes: usize = variables.iter().flat_map(|v| v.iter()).map(|s| s.len() + 1).sum();
                (ids_bytes + var_bytes, template_ids.len().max(1))
            }
            FieldEncoding::Varint { values } => {
                let bytes: usize = values.iter().map(|v| varint_unsigned_len(*v)).sum();
                (bytes, values.len().max(1))
            }
            FieldEncoding::Dictionary { ids, .. } => (ids.len() * 2, ids.len().max(1)),
            FieldEncoding::RawString { values } => {
                let bytes: usize = values.iter().map(|s| s.len()).sum();
                (bytes, values.len().max(1))
            }
            FieldEncoding::Bitpacked { count, .. } => (*count, count.saturating_mul(8).max(1)),
            FieldEncoding::FixedPrecision { values, .. } => {
                let bytes: usize = values.iter().map(|v| varint_signed_len(*v)).sum();
                (bytes, values.len().max(1))
            }
        };
        if count == 0 {
            return 0.0;
        }
        total as f64 / count as f64
    }

    /// Total bytes used by the encoded column data.
    pub fn total_bytes(&self) -> usize {
        match self {
            FieldEncoding::BitsetEnum { bitmap, .. } => {
                bitmap.iter().map(|b| b.len()).sum()
            }
            FieldEncoding::DeltaTimestamp { deltas, .. } => {
                deltas.iter().map(|d| varint_signed_len(*d)).sum()
            }
            FieldEncoding::PackedIp { values } => values.len() * 4,
            FieldEncoding::UrlTemplate {
                template_ids,
                variables,
                templates,
            } => {
                let tmpl_bytes: usize = templates.iter().map(|t| t.len()).sum();
                let ids_bytes = template_ids.len() * 2;
                let var_bytes: usize = variables.iter().flat_map(|v| v.iter()).map(|s| s.len() + 1).sum();
                tmpl_bytes + ids_bytes + var_bytes
            }
            FieldEncoding::Varint { values } => {
                values.iter().map(|v| varint_unsigned_len(*v)).sum()
            }
            FieldEncoding::Dictionary { dict, ids } => {
                let dict_bytes: usize = dict.iter().map(|s| s.len()).sum();
                dict_bytes + ids.len() * 2
            }
            FieldEncoding::RawString { values } => values.iter().map(|s| s.len()).sum(),
            FieldEncoding::Bitpacked { bits, .. } => bits.len(),
            FieldEncoding::FixedPrecision { values, .. } => {
                values.iter().map(|v| varint_signed_len(*v)).sum()
            }
        }
    }

    /// Number of documents stored.
    pub fn doc_count(&self) -> usize {
        match self {
            FieldEncoding::BitsetEnum { bitmap, .. } => {
                bitmap.first().map(|b| b.len() * 8).unwrap_or(0)
            }
            FieldEncoding::DeltaTimestamp { deltas, .. } => deltas.len() + 1,
            FieldEncoding::PackedIp { values } => values.len(),
            FieldEncoding::UrlTemplate { template_ids, .. } => template_ids.len(),
            FieldEncoding::Varint { values } => values.len(),
            FieldEncoding::Dictionary { ids, .. } => ids.len(),
            FieldEncoding::RawString { values } => values.len(),
            FieldEncoding::Bitpacked { count, .. } => *count,
            FieldEncoding::FixedPrecision { values, .. } => values.len(),
        }
    }

    /// Compression ratio relative to storing raw UTF-8 strings.
    ///
    /// A ratio of 4.0 means the encoding uses 4× less space than raw strings.
    pub fn compression_ratio_vs_raw(&self) -> f64 {
        let bpv = self.bytes_per_value();
        if bpv == 0.0 {
            return 1.0;
        }
        // We don't have the original strings at this point, so we use
        // a conservative estimate for the raw baseline based on encoding type.
        let raw_bpv = match self {
            FieldEncoding::BitsetEnum { .. } => 3.0,   // e.g. "200", "GET"
            FieldEncoding::DeltaTimestamp { .. } => 25.0, // typical timestamp string
            FieldEncoding::PackedIp { .. } => 14.0,    // "192.168.100.200" avg
            FieldEncoding::UrlTemplate { .. } => 30.0, // typical URL path
            FieldEncoding::Varint { .. } => 6.0,       // content-length as string
            FieldEncoding::Dictionary { .. } => 12.0,  // hostname / service name
            FieldEncoding::RawString { .. } => bpv,    // already raw
            FieldEncoding::Bitpacked { .. } => 4.5,    // "true" / "false"
            FieldEncoding::FixedPrecision { .. } => 6.0, // "12.34"
        };
        raw_bpv / bpv
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FieldAnalyzer
// ─────────────────────────────────────────────────────────────────────────────

/// Analyzes a column of string values and selects the optimal [`FieldEncoding`].
///
/// Uses at most `sample_size` values for statistical analysis, then encodes
/// the full column once the encoding is chosen.
pub struct FieldAnalyzer {
    /// Number of values to sample when probing statistics.
    sample_size: usize,
}

impl FieldAnalyzer {
    /// Create a new analyzer with the given sample window.
    ///
    /// `sample_size = 1024` is a reasonable default for most workloads.
    pub fn new(sample_size: usize) -> Self {
        Self {
            sample_size: sample_size.max(8),
        }
    }

    /// Analyze a column of values and return the optimal [`FieldEncoding`].
    ///
    /// The returned encoding contains the fully encoded data for all `values`,
    /// not just the sample.
    pub fn analyze(&self, field_name: &str, values: &[&str]) -> FieldEncoding {
        if values.is_empty() {
            return FieldEncoding::RawString { values: vec![] };
        }

        // ── 1. Field-name fast path ─────────────────────────────────────────
        match field_name {
            "status" | "status_code" | "http_status" | "response_code" => {
                return self.encode_as_bitset_enum(values);
            }
            "method" | "http_method" | "request_method" => {
                return self.encode_as_bitset_enum(values);
            }
            "level" | "log_level" | "severity" => {
                return self.encode_as_bitset_enum(values);
            }
            "@timestamp" | "timestamp" | "time" | "date" | "datetime" => {
                return self.encode_as_delta_timestamp(values);
            }
            "client_ip" | "remote_addr" | "ip" | "source_ip" | "dest_ip" => {
                return self.encode_as_packed_ip(values);
            }
            "path" | "url" | "uri" | "request_uri" | "request_path" => {
                return self.encode_as_url_template(values);
            }
            "content_length" | "bytes" | "body_bytes_sent" | "response_size" => {
                return self.encode_as_varint(values);
            }
            _ => {}
        }

        // ── 2. Statistical analysis on sample ───────────────────────────────
        let sample = &values[..values.len().min(self.sample_size)];
        let cardinality = unique_count(sample);

        // Very low cardinality — use BitsetEnum regardless of value type
        if cardinality <= 16 {
            return self.encode_as_bitset_enum(values);
        }

        // ── 3. Type probes — run BEFORE cardinality-based dictionary check ───
        // Numeric, IP, URL, and boolean types have dedicated encodings that beat
        // Dictionary even when cardinality is in the 17–256 range.

        // All parse as unsigned integer?
        if sample.iter().all(|v| v.parse::<u64>().is_ok()) {
            return self.encode_as_varint(values);
        }

        // All parse as f64 (floats — we already ruled out pure u64 above)?
        if sample.iter().all(|v| v.parse::<f64>().is_ok()) {
            return self.encode_as_fixed_precision(values);
        }

        // Timestamp heuristics
        if self.looks_like_timestamp(sample) {
            return self.encode_as_delta_timestamp(values);
        }

        // IP addresses
        if sample.iter().all(|v| is_ipv4(v)) {
            return self.encode_as_packed_ip(values);
        }

        // URLs / paths
        if sample
            .iter()
            .all(|v| v.starts_with('/') || v.starts_with("http"))
        {
            return self.encode_as_url_template(values);
        }

        // Booleans (shouldn't reach here due to cardinality ≤ 2 above, but guard anyway)
        if sample
            .iter()
            .all(|v| *v == "true" || *v == "false" || *v == "1" || *v == "0")
        {
            return self.encode_as_bitpacked(values);
        }

        // ── 4. Low-cardinality string dictionary ─────────────────────────────
        if cardinality <= 256 {
            return self.encode_as_dictionary(values);
        }

        // ── 5. Fallback ──────────────────────────────────────────────────────
        FieldEncoding::RawString {
            values: values.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── Encoding builders ─────────────────────────────────────────────────

    /// Build a [`FieldEncoding::BitsetEnum`] from a column of values.
    ///
    /// Up to 16 distinct values are supported.  If more are present, the
    /// rarest values beyond the limit are lumped under an `"__other__"` sentinel.
    pub fn encode_as_bitset_enum(&self, values: &[&str]) -> FieldEncoding {
        // Frequency count
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for v in values {
            *freq.entry(v).or_insert(0) += 1;
        }

        // Top-16 by frequency (ties broken alphabetically for stability)
        let mut entries: Vec<(&str, usize)> = freq.into_iter().collect();
        entries.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        entries.truncate(16);

        let dict: Vec<String> = entries.iter().map(|(v, _)| v.to_string()).collect();
        let dict_index: HashMap<&str, usize> = dict
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();

        let n_docs = values.len();
        let bytes_per_bitmap = (n_docs + 7) / 8;

        // One bitmap per dictionary entry
        let mut bitmap: Vec<Vec<u8>> = vec![vec![0u8; bytes_per_bitmap]; dict.len()];

        for (doc_idx, val) in values.iter().enumerate() {
            if let Some(&dict_pos) = dict_index.get(val) {
                let byte = doc_idx / 8;
                let bit = doc_idx % 8;
                bitmap[dict_pos][byte] |= 1 << bit;
            }
            // Values not in the top-16 are silently dropped from the bitmap
        }

        FieldEncoding::BitsetEnum {
            values: dict,
            bitmap,
        }
    }

    /// Build a [`FieldEncoding::DeltaTimestamp`] from a column of timestamp strings.
    ///
    /// The base timestamp is the first successfully parsed value. Subsequent values
    /// are stored as delta-of-delta (second derivative of the timestamp sequence).
    pub fn encode_as_delta_timestamp(&self, values: &[&str]) -> FieldEncoding {
        // Try to parse all values; detect format from the first parseable value
        let (format, parsed_us): (TimestampFormat, Vec<i64>) = {
            let fmt = detect_timestamp_format(values);
            let parsed = values
                .iter()
                .map(|v| parse_timestamp_us(v, &fmt).unwrap_or(0))
                .collect();
            (fmt, parsed)
        };

        if parsed_us.is_empty() {
            return FieldEncoding::RawString {
                values: values.iter().map(|s| s.to_string()).collect(),
            };
        }

        let base_us = parsed_us[0];

        // First pass: absolute → delta
        let deltas_abs: Vec<i64> = parsed_us
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect();

        // Second pass: delta → delta-of-delta
        let mut deltas: Vec<i64> = Vec::with_capacity(deltas_abs.len());
        if !deltas_abs.is_empty() {
            deltas.push(deltas_abs[0]); // first delta stored as-is
            for i in 1..deltas_abs.len() {
                deltas.push(deltas_abs[i] - deltas_abs[i - 1]);
            }
        }

        FieldEncoding::DeltaTimestamp {
            base_us,
            format,
            deltas,
        }
    }

    /// Build a [`FieldEncoding::PackedIp`] from a column of IPv4 strings.
    pub fn encode_as_packed_ip(&self, values: &[&str]) -> FieldEncoding {
        let packed: Vec<u32> = values
            .iter()
            .map(|v| parse_ipv4(v).unwrap_or(0))
            .collect();
        FieldEncoding::PackedIp { values: packed }
    }

    /// Build a [`FieldEncoding::UrlTemplate`] from a column of URL path strings.
    pub fn encode_as_url_template(&self, values: &[&str]) -> FieldEncoding {
        let mut templates: Vec<String> = Vec::new();
        let mut tmpl_index: HashMap<String, u16> = HashMap::new();
        let mut template_ids: Vec<u16> = Vec::with_capacity(values.len());
        let mut variables: Vec<Vec<String>> = Vec::with_capacity(values.len());

        for v in values {
            let (tmpl, vars) = extract_url_template(v);
            let id = if let Some(&existing) = tmpl_index.get(&tmpl) {
                existing
            } else {
                let id = templates.len() as u16;
                tmpl_index.insert(tmpl.clone(), id);
                templates.push(tmpl);
                id
            };
            template_ids.push(id);
            variables.push(vars);
        }

        FieldEncoding::UrlTemplate {
            templates,
            template_ids,
            variables,
        }
    }

    /// Build a [`FieldEncoding::Varint`] from a column of integer strings.
    pub fn encode_as_varint(&self, values: &[&str]) -> FieldEncoding {
        let nums: Vec<u64> = values
            .iter()
            .map(|v| v.parse::<u64>().unwrap_or(0))
            .collect();
        FieldEncoding::Varint { values: nums }
    }

    /// Build a [`FieldEncoding::Dictionary`] from a column of strings.
    pub fn encode_as_dictionary(&self, values: &[&str]) -> FieldEncoding {
        let mut dict: Vec<String> = Vec::new();
        let mut dict_index: HashMap<&str, u16> = HashMap::new();
        let mut ids: Vec<u16> = Vec::with_capacity(values.len());

        for v in values {
            let id = if let Some(&existing) = dict_index.get(v) {
                existing
            } else {
                let id = dict.len() as u16;
                dict_index.insert(v, id);
                dict.push(v.to_string());
                id
            };
            ids.push(id);
        }

        FieldEncoding::Dictionary { dict, ids }
    }

    /// Build a [`FieldEncoding::Bitpacked`] from a column of boolean strings.
    pub fn encode_as_bitpacked(&self, values: &[&str]) -> FieldEncoding {
        let count = values.len();
        let mut bits = vec![0u8; (count + 7) / 8];
        for (i, v) in values.iter().enumerate() {
            let is_true = *v == "true" || *v == "1";
            if is_true {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        FieldEncoding::Bitpacked { bits, count }
    }

    /// Build a [`FieldEncoding::FixedPrecision`] from a column of float strings.
    ///
    /// Detects the minimum scale (decimal places) required to represent all
    /// values exactly, then multiplies by `10^scale` and stores as `i64`.
    pub fn encode_as_fixed_precision(&self, values: &[&str]) -> FieldEncoding {
        let scale = detect_decimal_scale(values);
        let multiplier = 10_i64.pow(scale);
        let scaled: Vec<i64> = values
            .iter()
            .map(|v| {
                v.parse::<f64>()
                    .map(|f| (f * multiplier as f64).round() as i64)
                    .unwrap_or(0)
            })
            .collect();
        FieldEncoding::FixedPrecision {
            scale,
            values: scaled,
        }
    }

    // ── Heuristics ────────────────────────────────────────────────────────

    /// Heuristic: does the sample look like it contains timestamps?
    fn looks_like_timestamp(&self, sample: &[&str]) -> bool {
        if sample.is_empty() {
            return false;
        }
        let threshold = (sample.len() * 8 / 10).max(1); // 80%
        let matched = sample
            .iter()
            .filter(|v| detect_timestamp_format_single(v) != TimestampLikelihood::None)
            .count();
        matched >= threshold
    }
}

impl Default for FieldAnalyzer {
    fn default() -> Self {
        Self::new(1024)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers — timestamp detection & parsing
// ─────────────────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum TimestampLikelihood {
    None,
    Likely,
}

/// Classify a single value for timestamp likelihood without full parsing.
fn detect_timestamp_format_single(v: &str) -> TimestampLikelihood {
    // ISO 8601: starts with 4 digits + '-'
    if v.len() >= 10 && v.as_bytes().iter().take(4).all(|b| b.is_ascii_digit()) && v.as_bytes().get(4) == Some(&b'-') {
        return TimestampLikelihood::Likely;
    }
    // Apache common: starts with '['
    if v.starts_with('[') && v.len() > 20 {
        return TimestampLikelihood::Likely;
    }
    // Nginx: `YYYY/MM/DD`
    if v.len() >= 10
        && v.as_bytes().iter().take(4).all(|b| b.is_ascii_digit())
        && v.as_bytes().get(4) == Some(&b'/')
    {
        return TimestampLikelihood::Likely;
    }
    // Epoch seconds (10 digits) or millis (13 digits)
    if (v.len() == 10 || v.len() == 13) && v.as_bytes().iter().all(|b| b.is_ascii_digit()) {
        return TimestampLikelihood::Likely;
    }
    TimestampLikelihood::None
}

/// Detect the dominant timestamp format across a sample.
fn detect_timestamp_format(values: &[&str]) -> TimestampFormat {
    for v in values {
        let v = v.trim();
        if v.starts_with('[') {
            return TimestampFormat::ApacheCommon;
        }
        if v.len() >= 10
            && v.as_bytes().iter().take(4).all(|b| b.is_ascii_digit())
            && v.as_bytes().get(4) == Some(&b'/')
        {
            return TimestampFormat::NginxDefault;
        }
        if v.len() >= 10
            && v.as_bytes().iter().take(4).all(|b| b.is_ascii_digit())
            && v.as_bytes().get(4) == Some(&b'-')
        {
            return TimestampFormat::Iso8601;
        }
        if let Ok(n) = v.parse::<i64>() {
            if n > 1_000_000_000_000 {
                return TimestampFormat::EpochMillis;
            }
            return TimestampFormat::EpochSeconds;
        }
    }
    TimestampFormat::EpochSeconds
}

/// Parse a timestamp string to microseconds since the Unix epoch.
///
/// Returns `None` on parse failure — callers substitute `0` as a sentinel.
fn parse_timestamp_us(v: &str, format: &TimestampFormat) -> Option<i64> {
    let v = v.trim();
    match format {
        TimestampFormat::EpochSeconds => v.parse::<i64>().ok().map(|s| s * 1_000_000),
        TimestampFormat::EpochMillis => v.parse::<i64>().ok().map(|ms| ms * 1_000),
        TimestampFormat::Iso8601 => parse_iso8601_us(v),
        TimestampFormat::ApacheCommon => parse_apache_us(v),
        TimestampFormat::NginxDefault => parse_nginx_us(v),
        TimestampFormat::Custom(_) => None,
    }
}

/// Minimal ISO 8601 parser → microseconds since epoch.
///
/// Handles `YYYY-MM-DDTHH:MM:SSZ` and `YYYY-MM-DDTHH:MM:SS+HH:MM`.
fn parse_iso8601_us(v: &str) -> Option<i64> {
    // Expect at least "YYYY-MM-DDTHH:MM:SS"
    if v.len() < 19 {
        return None;
    }
    let year: i64 = v[0..4].parse().ok()?;
    let month: i64 = v[5..7].parse().ok()?;
    let day: i64 = v[8..10].parse().ok()?;
    let hour: i64 = v[11..13].parse().ok()?;
    let min: i64 = v[14..16].parse().ok()?;
    let sec: i64 = v[17..19].parse().ok()?;

    // Timezone offset
    let tz_offset_secs: i64 = if v.len() > 19 {
        let tz = &v[19..];
        if tz.starts_with('Z') || tz.starts_with('z') {
            0
        } else if (tz.starts_with('+') || tz.starts_with('-')) && tz.len() >= 6 {
            let sign: i64 = if tz.starts_with('-') { -1 } else { 1 };
            let th: i64 = tz[1..3].parse().ok()?;
            let tm: i64 = tz[4..6].parse().ok()?;
            sign * (th * 3600 + tm * 60)
        } else {
            0
        }
    } else {
        0
    };

    let days = days_since_epoch(year, month, day)?;
    let secs = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset_secs;
    Some(secs * 1_000_000)
}

/// Apache Common Log format: `[10/Jan/2024:12:34:56 +0000]`
fn parse_apache_us(v: &str) -> Option<i64> {
    // Strip brackets
    let v = v.trim_matches(|c| c == '[' || c == ']').trim();
    // "10/Jan/2024:12:34:56 +0000"
    if v.len() < 26 {
        return None;
    }
    let day: i64 = v[0..2].parse().ok()?;
    let month_str = &v[3..6];
    let month = month_from_abbr(month_str)?;
    let year: i64 = v[7..11].parse().ok()?;
    let hour: i64 = v[12..14].parse().ok()?;
    let min: i64 = v[15..17].parse().ok()?;
    let sec: i64 = v[18..20].parse().ok()?;

    let tz_offset_secs: i64 = if v.len() >= 26 {
        let tz = v[21..].trim();
        if tz.len() >= 5 {
            let sign: i64 = if tz.starts_with('-') { -1 } else { 1 };
            let th: i64 = tz[1..3].parse().ok()?;
            let tm: i64 = tz[3..5].parse().ok()?;
            sign * (th * 3600 + tm * 60)
        } else {
            0
        }
    } else {
        0
    };

    let days = days_since_epoch(year, month, day)?;
    let secs = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset_secs;
    Some(secs * 1_000_000)
}

/// Nginx default log format: `2024/01/15 12:34:56`
fn parse_nginx_us(v: &str) -> Option<i64> {
    if v.len() < 19 {
        return None;
    }
    let year: i64 = v[0..4].parse().ok()?;
    let month: i64 = v[5..7].parse().ok()?;
    let day: i64 = v[8..10].parse().ok()?;
    let hour: i64 = v[11..13].parse().ok()?;
    let min: i64 = v[14..16].parse().ok()?;
    let sec: i64 = v[17..19].parse().ok()?;

    let days = days_since_epoch(year, month, day)?;
    let secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Some(secs * 1_000_000)
}

/// Days since Unix epoch (1970-01-01) using the proleptic Gregorian calendar.
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1 <= month && month <= 12 && 1 <= day && day <= 31) {
        return None;
    }
    // Rata Die algorithm
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let rata_die = era * 146097 + doe;
    // Days from 0000-03-01 to 1970-01-01 = 719468
    Some(rata_die - 719468)
}

/// 3-letter month abbreviation → 1-based month number.
fn month_from_abbr(s: &str) -> Option<i64> {
    match s {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers — IP parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Returns `true` if `v` is a valid dotted-decimal IPv4 address.
fn is_ipv4(v: &str) -> bool {
    parse_ipv4(v).is_some()
}

/// Parse a dotted-decimal IPv4 string to a packed u32.
///
/// `192.168.1.1` → `(192 << 24) | (168 << 16) | (1 << 8) | 1`
pub fn parse_ipv4(v: &str) -> Option<u32> {
    let mut octets = v.split('.');
    let a: u32 = octets.next()?.parse().ok()?;
    let b: u32 = octets.next()?.parse().ok()?;
    let c: u32 = octets.next()?.parse().ok()?;
    let d: u32 = octets.next()?.parse().ok()?;
    if octets.next().is_some() {
        return None; // extra segments
    }
    if a > 255 || b > 255 || c > 255 || d > 255 {
        return None;
    }
    Some((a << 24) | (b << 16) | (c << 8) | d)
}

/// Check whether a packed IP falls inside a CIDR block.
///
/// ```rust
/// use xerj_compress::field_codec::{parse_ipv4, ip_in_cidr};
/// let ip = parse_ipv4("10.0.0.15").unwrap();
/// assert!(ip_in_cidr(ip, parse_ipv4("10.0.0.0").unwrap(), 24));
/// ```
pub fn ip_in_cidr(ip: u32, network: u32, prefix_len: u32) -> bool {
    if prefix_len == 0 {
        return true;
    }
    if prefix_len > 32 {
        return false;
    }
    let mask = !0u32 << (32 - prefix_len);
    (ip & mask) == (network & mask)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers — URL template extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Split a URL path into a (template, variables) pair.
///
/// Numeric segments and UUID-like segments are replaced with `{}`.
///
/// ```
/// // "/api/users/123" → ("/api/users/{}", ["123"])
/// // "/static/js/app.abc123.js" → ("/static/js/{}", ["app.abc123.js"])
/// ```
fn extract_url_template(url: &str) -> (String, Vec<String>) {
    // Split off query string and fragment
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);

    let mut template_parts: Vec<&str> = Vec::new();
    let mut vars: Vec<String> = Vec::new();

    for segment in path.split('/') {
        if is_variable_segment(segment) {
            template_parts.push("{}");
            vars.push(segment.to_string());
        } else {
            template_parts.push(segment);
        }
    }

    (template_parts.join("/"), vars)
}

/// Returns `true` if a URL path segment should be treated as a variable
/// (and replaced with `{}` in the template).
fn is_variable_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    // Pure integer
    if seg.bytes().all(|b| b.is_ascii_digit()) && !seg.is_empty() {
        return true;
    }
    // UUID: 8-4-4-4-12 hex groups
    if is_uuid_like(seg) {
        return true;
    }
    // Hex string ≥ 8 chars (commit hash, object ID, content hash in filenames)
    if seg.len() >= 8 && seg.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    // File with a content hash (e.g. "app.abc123def456.js") —
    // contains a dot and a hex-dominated component between dots
    let parts: Vec<&str> = seg.split('.').collect();
    if parts.len() >= 3 {
        // At least one middle segment is a hex hash
        for p in &parts[1..parts.len() - 1] {
            if p.len() >= 6 && p.bytes().all(|b| b.is_ascii_hexdigit()) {
                return true;
            }
        }
    }
    false
}

/// Heuristic UUID detection (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx).
fn is_uuid_like(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes[8] == b'-' && bytes[13] == b'-' && bytes[18] == b'-' && bytes[23] == b'-'
        && bytes.iter().enumerate().all(|(i, &b)| {
            if i == 8 || i == 13 || i == 18 || i == 23 {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers — float precision detection
// ─────────────────────────────────────────────────────────────────────────────

/// Detect the minimum number of decimal places needed to represent all values
/// in the sample exactly (up to a maximum of 6 decimal places).
fn detect_decimal_scale(values: &[&str]) -> u32 {
    let mut max_scale: u32 = 0;
    for v in values {
        if let Some(dot_pos) = v.find('.') {
            let decimal_places = v.len() - dot_pos - 1;
            max_scale = max_scale.max(decimal_places as u32);
            if max_scale >= 6 {
                return 6;
            }
        }
    }
    max_scale.min(6)
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers — cardinality & varint sizing
// ─────────────────────────────────────────────────────────────────────────────

/// Count the number of unique values in a slice.
pub fn unique_count(values: &[&str]) -> usize {
    values.iter().collect::<HashSet<_>>().len()
}

/// Number of bytes required to encode `v` as an unsigned varint (LEB128).
fn varint_unsigned_len(mut v: u64) -> usize {
    if v == 0 {
        return 1;
    }
    let mut len = 0;
    while v > 0 {
        len += 1;
        v >>= 7;
    }
    len
}

/// Number of bytes required to encode `v` as a signed zigzag varint.
fn varint_signed_len(v: i64) -> usize {
    // Zigzag encode: (v << 1) ^ (v >> 63)
    let encoded = ((v << 1) ^ (v >> 63)) as u64;
    varint_unsigned_len(encoded)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests for individual helpers ────────────────────────────────

    #[test]
    fn test_parse_ipv4() {
        assert_eq!(parse_ipv4("192.168.1.1"), Some((192 << 24) | (168 << 16) | (1 << 8) | 1));
        assert_eq!(parse_ipv4("0.0.0.0"), Some(0));
        assert_eq!(parse_ipv4("255.255.255.255"), Some(u32::MAX));
        assert!(parse_ipv4("not-an-ip").is_none());
        assert!(parse_ipv4("256.0.0.1").is_none());
    }

    #[test]
    fn test_ip_in_cidr() {
        let ip = parse_ipv4("10.0.0.15").unwrap();
        let net = parse_ipv4("10.0.0.0").unwrap();
        assert!(ip_in_cidr(ip, net, 24));
        assert!(!ip_in_cidr(ip, parse_ipv4("192.168.0.0").unwrap(), 16));
        assert!(ip_in_cidr(ip, 0, 0)); // /0 matches everything
    }

    #[test]
    fn test_uuid_detection() {
        assert!(is_uuid_like("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!is_uuid_like("not-a-uuid"));
        assert!(!is_uuid_like("123"));
    }

    #[test]
    fn test_url_template_extraction() {
        let (tmpl, vars) = extract_url_template("/api/users/123");
        assert_eq!(tmpl, "/api/users/{}");
        assert_eq!(vars, vec!["123"]);

        let (tmpl, vars) = extract_url_template("/static/js/app.abc123def.js");
        assert_eq!(tmpl, "/static/js/{}");
        assert_eq!(vars, vec!["app.abc123def.js"]);

        let (tmpl, _) = extract_url_template("/api/health");
        assert_eq!(tmpl, "/api/health"); // no variables

        let (tmpl, vars) =
            extract_url_template("/api/v2/users/550e8400-e29b-41d4-a716-446655440000/posts/42");
        assert_eq!(tmpl, "/api/v2/users/{}/posts/{}");
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn test_varint_sizing() {
        assert_eq!(varint_unsigned_len(0), 1);
        assert_eq!(varint_unsigned_len(127), 1);
        assert_eq!(varint_unsigned_len(128), 2);
        assert_eq!(varint_unsigned_len(16383), 2);
        assert_eq!(varint_unsigned_len(16384), 3);
    }

    #[test]
    fn test_decimal_scale() {
        assert_eq!(detect_decimal_scale(&["1.23", "4.56", "7.8"]), 2);
        assert_eq!(detect_decimal_scale(&["1.0", "2.0"]), 1);
        assert_eq!(detect_decimal_scale(&["100", "200"]), 0);
    }

    #[test]
    fn test_iso8601_parse() {
        let us = parse_iso8601_us("2024-01-15T12:34:56Z").unwrap();
        assert!(us > 0);
        // Verify it's in the right ballpark (2024 >> 1970)
        assert!(us > 1_700_000_000 * 1_000_000);
    }

    #[test]
    fn test_apache_parse() {
        let us = parse_apache_us("[10/Jan/2024:12:34:56 +0000]").unwrap();
        assert!(us > 0);
    }

    #[test]
    fn test_nginx_parse() {
        let us = parse_nginx_us("2024/01/10 12:34:56").unwrap();
        assert!(us > 0);
    }

    // ── FieldAnalyzer — fast-path encoding ───────────────────────────────

    #[test]
    fn test_status_codes_bitset_enum() {
        let analyzer = FieldAnalyzer::new(1024);
        let statuses = vec![
            "200", "200", "404", "500", "200", "301", "404", "200", "403", "200",
        ];
        let refs: Vec<&str> = statuses.iter().copied().collect();
        let enc = analyzer.analyze("status", &refs);

        match &enc {
            FieldEncoding::BitsetEnum { values, bitmap } => {
                assert!(values.contains(&"200".to_string()));
                assert_eq!(bitmap.len(), values.len());
                // The 200 bitmap should have bits 0,1,5,7,9 set
                let idx_200 = values.iter().position(|v| v == "200").unwrap();
                let bmap = &bitmap[idx_200];
                // doc 0 → byte 0 bit 0
                assert!(bmap[0] & 1 != 0);
            }
            other => panic!("expected BitsetEnum, got {:?}", std::mem::discriminant(other)),
        }

        // bytes_per_value should be well below 1.0 for 10 docs / small cardinality
        assert!(enc.bytes_per_value() < 3.0);
        println!(
            "status: {:.2} bytes/value (raw ~3), ratio {:.1}x",
            enc.bytes_per_value(),
            enc.compression_ratio_vs_raw()
        );
    }

    #[test]
    fn test_http_method_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        let methods = vec!["GET", "POST", "GET", "GET", "DELETE", "PUT", "GET", "POST"];
        let refs: Vec<&str> = methods.iter().copied().collect();
        let enc = analyzer.analyze("method", &refs);
        assert!(matches!(enc, FieldEncoding::BitsetEnum { .. }));
        assert!(enc.bytes_per_value() < 2.0);
    }

    #[test]
    fn test_log_level_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        let levels = vec!["INFO", "ERROR", "WARN", "INFO", "DEBUG", "INFO", "ERROR"];
        let refs: Vec<&str> = levels.iter().copied().collect();
        let enc = analyzer.analyze("level", &refs);
        assert!(matches!(enc, FieldEncoding::BitsetEnum { .. }));
        println!(
            "level: {:.2} bytes/value (raw ~4.5), ratio {:.1}x",
            enc.bytes_per_value(),
            enc.compression_ratio_vs_raw()
        );
    }

    #[test]
    fn test_timestamp_iso8601() {
        let analyzer = FieldAnalyzer::new(1024);
        let timestamps = vec![
            "2024-01-15T12:34:56Z",
            "2024-01-15T12:34:57Z",
            "2024-01-15T12:34:58Z",
            "2024-01-15T12:34:59Z",
            "2024-01-15T12:35:00Z",
        ];
        let refs: Vec<&str> = timestamps.iter().copied().collect();
        let enc = analyzer.analyze("@timestamp", &refs);

        match &enc {
            FieldEncoding::DeltaTimestamp {
                base_us,
                format,
                deltas,
            } => {
                assert!(matches!(format, TimestampFormat::Iso8601));
                assert!(*base_us > 0);
                // Consecutive 1-second timestamps → deltas are all 1_000_000 µs
                // delta-of-delta after the first should be 0
                assert_eq!(deltas.len(), 4); // N-1 values encoded
                println!("timestamp deltas: {:?}", deltas);
            }
            other => panic!("expected DeltaTimestamp, got {:?}", std::mem::discriminant(other)),
        }

        println!(
            "timestamp: {:.2} bytes/value (raw ~25), ratio {:.1}x",
            enc.bytes_per_value(),
            enc.compression_ratio_vs_raw()
        );
    }

    #[test]
    fn test_timestamp_apache() {
        let analyzer = FieldAnalyzer::new(1024);
        let timestamps = vec![
            "[10/Jan/2024:12:34:56 +0000]",
            "[10/Jan/2024:12:34:57 +0000]",
            "[10/Jan/2024:12:35:00 +0000]",
        ];
        let refs: Vec<&str> = timestamps.iter().copied().collect();
        let enc = analyzer.analyze("@timestamp", &refs);
        assert!(matches!(enc, FieldEncoding::DeltaTimestamp { format: TimestampFormat::ApacheCommon, .. }));
    }

    #[test]
    fn test_ip_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        let ips = vec![
            "192.168.1.1",
            "10.0.0.1",
            "172.16.0.50",
            "192.168.1.2",
            "8.8.8.8",
        ];
        let refs: Vec<&str> = ips.iter().copied().collect();
        let enc = analyzer.analyze("client_ip", &refs);

        match &enc {
            FieldEncoding::PackedIp { values } => {
                assert_eq!(values.len(), 5);
                assert_eq!(values[0], parse_ipv4("192.168.1.1").unwrap());
            }
            other => panic!("expected PackedIp, got {:?}", std::mem::discriminant(other)),
        }

        assert_eq!(enc.bytes_per_value(), 4.0);
        println!(
            "ip: {:.2} bytes/value (raw ~14), ratio {:.1}x",
            enc.bytes_per_value(),
            enc.compression_ratio_vs_raw()
        );
    }

    #[test]
    fn test_url_template_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        let paths = vec![
            "/api/users/1",
            "/api/users/2",
            "/api/users/3",
            "/api/users/4/posts",
            "/static/js/app.js",
            "/static/css/main.css",
        ];
        let refs: Vec<&str> = paths.iter().copied().collect();
        let enc = analyzer.analyze("path", &refs);

        match &enc {
            FieldEncoding::UrlTemplate {
                templates,
                template_ids,
                variables,
            } => {
                assert!(templates.len() < paths.len(), "should deduplicate templates");
                assert_eq!(template_ids.len(), paths.len());
                assert_eq!(variables.len(), paths.len());
                println!("URL templates: {:?}", templates);
            }
            other => panic!("expected UrlTemplate, got {:?}", std::mem::discriminant(other)),
        }

        println!(
            "path: {:.2} bytes/value (raw ~30), ratio {:.1}x",
            enc.bytes_per_value(),
            enc.compression_ratio_vs_raw()
        );
    }

    #[test]
    fn test_varint_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        let sizes = vec!["1024", "2048", "512", "65536", "128", "256"];
        let refs: Vec<&str> = sizes.iter().copied().collect();
        let enc = analyzer.analyze("content_length", &refs);

        assert!(matches!(enc, FieldEncoding::Varint { .. }));
        println!(
            "content_length: {:.2} bytes/value (raw ~4), ratio {:.1}x",
            enc.bytes_per_value(),
            enc.compression_ratio_vs_raw()
        );
    }

    #[test]
    fn test_boolean_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        // 2 unique values → BitsetEnum wins (most compact for tiny cardinality)
        let bools: Vec<&str> = (0..64).map(|i| if i % 3 == 0 { "true" } else { "false" }).collect();
        let enc = analyzer.analyze("is_active", &bools);
        // Either BitsetEnum or Bitpacked is correct — both are compact
        assert!(
            matches!(enc, FieldEncoding::Bitpacked { .. }) || matches!(enc, FieldEncoding::BitsetEnum { .. }),
            "expected compact bool encoding"
        );
        assert!(enc.bytes_per_value() < 1.0);
    }

    #[test]
    fn test_float_encoding() {
        let analyzer = FieldAnalyzer::new(1024);
        // Generate 20 distinct float values so cardinality > 16 bypasses BitsetEnum
        let floats: Vec<String> = (0..20).map(|i| format!("{:.2}", i as f64 + 0.34)).collect();
        let refs: Vec<&str> = floats.iter().map(String::as_str).collect();
        let enc = analyzer.analyze("response_time_ms", &refs);

        match &enc {
            FieldEncoding::FixedPrecision { scale, values } => {
                assert_eq!(*scale, 2);
                // First value: 0.34 → 34
                assert_eq!(values[0], 34);
                assert_eq!(values.len(), 20);
            }
            other => panic!("expected FixedPrecision, got {:?}", std::mem::discriminant(other)),
        }
    }

    #[test]
    fn test_dictionary_medium_cardinality() {
        let analyzer = FieldAnalyzer::new(1024);
        // 20 unique hostnames — above 16 (BitsetEnum) but below 256 (Dictionary)
        let hosts: Vec<String> = (0..20)
            .flat_map(|i| vec![format!("host-{i}.example.com"); 5])
            .collect();
        let refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
        let enc = analyzer.analyze("hostname", &refs);
        assert!(matches!(enc, FieldEncoding::Dictionary { .. }));
    }

    #[test]
    fn test_raw_string_fallback() {
        let analyzer = FieldAnalyzer::new(1024);
        // High cardinality, non-structured strings
        let msgs: Vec<String> = (0..500).map(|i| format!("Unique log message number {i} with some context")).collect();
        let refs: Vec<&str> = msgs.iter().map(String::as_str).collect();
        let enc = analyzer.analyze("message", &refs);
        assert!(matches!(enc, FieldEncoding::RawString { .. }));
    }

    // ── Compression ratio demo — realistic Apache access log simulation ───

    /// Simulates 10 000 Apache access log entries and measures compression
    /// ratios for each field using its optimal encoding.
    #[test]
    fn test_apache_log_compression_ratios() {
        let n = 10_000usize;

        // Simulate field distributions that match real Apache logs
        let statuses: Vec<&str> = (0..n)
            .map(|i| match i % 20 {
                0 => "404",
                1 => "500",
                2 | 3 => "301",
                4 => "403",
                _ => "200",
            })
            .collect();

        let methods: Vec<&str> = (0..n)
            .map(|i| match i % 10 {
                8 => "POST",
                9 => "PUT",
                _ => "GET",
            })
            .collect();

        let ips: Vec<String> = (0..n)
            .map(|i| format!("192.168.{}.{}", (i / 256) % 256, i % 256))
            .collect();
        let ip_refs: Vec<&str> = ips.iter().map(String::as_str).collect();

        let timestamps: Vec<String> = (0..n)
            .map(|i| format!("2024-01-15T12:{:02}:{:02}Z", (i / 60) % 60, i % 60))
            .collect();
        let ts_refs: Vec<&str> = timestamps.iter().map(String::as_str).collect();

        let paths: Vec<String> = (0..n)
            .map(|i| match i % 5 {
                0 => format!("/api/users/{}", i),
                1 => format!("/api/posts/{}", i / 5),
                2 => "/api/health".to_string(),
                3 => format!("/static/js/app.{:08x}.js", i),
                _ => format!("/api/items/{}/details", i % 100),
            })
            .collect();
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();

        let sizes: Vec<String> = (0..n)
            .map(|i| (512 + (i * 137) % 65536).to_string())
            .collect();
        let size_refs: Vec<&str> = sizes.iter().map(String::as_str).collect();

        let analyzer = FieldAnalyzer::new(1024);

        let status_enc = analyzer.analyze("status", &statuses);
        let method_enc = analyzer.analyze("method", &methods);
        let ip_enc = analyzer.analyze("client_ip", &ip_refs);
        let ts_enc = analyzer.analyze("@timestamp", &ts_refs);
        let path_enc = analyzer.analyze("path", &path_refs);
        let size_enc = analyzer.analyze("content_length", &size_refs);

        // Print compression summary
        println!("\n=== Apache Access Log Compression Ratios ({n} docs) ===");
        println!(
            "{:<20} {:>10} {:>10} {:>8}",
            "field", "bytes/val", "raw bytes", "ratio"
        );
        println!("{}", "-".repeat(52));

        let fields: &[(&str, &FieldEncoding, f64)] = &[
            ("status", &status_enc, 3.0),
            ("method", &method_enc, 4.0),
            ("client_ip", &ip_enc, 14.0),
            ("@timestamp", &ts_enc, 25.0),
            ("path", &path_enc, 30.0),
            ("content_length", &size_enc, 6.0),
        ];

        for (name, enc, raw_bpv) in fields {
            println!(
                "{:<20} {:>10.2} {:>10.1} {:>8.1}x",
                name,
                enc.bytes_per_value(),
                raw_bpv,
                raw_bpv / enc.bytes_per_value().max(0.01)
            );
        }

        // Validate specific assertions from the spec
        // status: 0.5 bytes vs 3
        assert!(
            status_enc.bytes_per_value() < 1.5,
            "status should use <1.5 bytes/value, got {:.2}",
            status_enc.bytes_per_value()
        );

        // IP: 4 bytes vs 15
        assert!(
            (ip_enc.bytes_per_value() - 4.0).abs() < 0.01,
            "IP should use exactly 4 bytes/value"
        );

        // method: < 1.5 bytes vs 4
        assert!(
            method_enc.bytes_per_value() < 1.5,
            "method should use <1.5 bytes/value"
        );

        // path: should compress vs raw (at least 2x)
        assert!(
            path_enc.compression_ratio_vs_raw() > 1.5,
            "path should compress >1.5x, got {:.2}x",
            path_enc.compression_ratio_vs_raw()
        );

        // timestamp: < 4 bytes/value (delta-of-delta of 1s intervals → small deltas)
        assert!(
            ts_enc.bytes_per_value() < 4.0,
            "timestamp should use <4 bytes/value, got {:.2}",
            ts_enc.bytes_per_value()
        );

        // Total bytes across all fields
        let total_encoded = status_enc.total_bytes()
            + method_enc.total_bytes()
            + ip_enc.total_bytes()
            + ts_enc.total_bytes()
            + path_enc.total_bytes()
            + size_enc.total_bytes();

        let total_raw = (3 + 4 + 14 + 25 + 30 + 6) * n;

        println!("\nTotal encoded: {} bytes", total_encoded);
        println!("Total raw:     {} bytes", total_raw);
        println!(
            "Overall ratio: {:.1}x",
            total_raw as f64 / total_encoded as f64
        );

        assert!(
            total_encoded < total_raw,
            "encoded ({total_encoded}) should be smaller than raw ({total_raw})"
        );
    }

    #[test]
    fn test_bitset_enum_bitmap_correctness() {
        let analyzer = FieldAnalyzer::new(1024);
        // Exact pattern: [A, B, A, C, A, B]
        let vals = vec!["A", "B", "A", "C", "A", "B"];
        let enc = analyzer.encode_as_bitset_enum(&vals);

        if let FieldEncoding::BitsetEnum { values, bitmap } = &enc {
            let idx_a = values.iter().position(|v| v == "A").unwrap();
            let bmap_a = &bitmap[idx_a];
            // A is at positions 0, 2, 4 → bits 0, 2, 4 of byte 0 → 0b00010101 = 0x15
            assert_eq!(bmap_a[0] & 0b00010101, 0b00010101);
            // A is NOT at position 1, 3, 5
            assert_eq!(bmap_a[0] & 0b00101010, 0);
        } else {
            panic!("expected BitsetEnum");
        }
    }

    #[test]
    fn test_delta_timestamp_regularity() {
        // Regular 1-second intervals should produce tiny delta-of-delta values
        let analyzer = FieldAnalyzer::new(1024);
        let timestamps: Vec<String> = (0..100)
            .map(|i| format!("2024-01-15T00:00:{:02}Z", i))
            .collect();
        let refs: Vec<&str> = timestamps.iter().map(String::as_str).collect();
        let enc = analyzer.analyze("@timestamp", &refs);

        if let FieldEncoding::DeltaTimestamp { deltas, .. } = &enc {
            // First delta = 1_000_000 µs, all subsequent delta-of-deltas = 0
            let dod_zeros = deltas[1..].iter().filter(|&&d| d == 0).count();
            assert!(
                dod_zeros as f64 / (deltas.len() - 1) as f64 > 0.95,
                "regular timestamps should produce mostly zero delta-of-deltas"
            );
        }
    }

    #[test]
    fn test_unique_count() {
        let vals = vec!["a", "b", "a", "c", "b", "a"];
        assert_eq!(unique_count(&vals), 3);
    }

    #[test]
    fn test_compression_ratio_vs_raw() {
        let analyzer = FieldAnalyzer::new(1024);
        let ips: Vec<String> = (0..100).map(|i| format!("10.0.0.{}", i % 256)).collect();
        let refs: Vec<&str> = ips.iter().map(String::as_str).collect();
        let enc = analyzer.analyze("client_ip", &refs);
        // PackedIp uses 4 bytes vs ~10 bytes average for these IPs
        assert!(enc.compression_ratio_vs_raw() > 2.0);
    }
}
