//! Built-in transform plugins — no WASM required.
//!
//! Every plugin implements [`TransformPlugin`] and can be composed into a
//! [`Pipeline`](crate::pipeline::Pipeline).

use std::collections::HashMap;

use regex::Regex;
use serde_json::Value;
use tracing::debug;

use crate::{pipeline::ProcessAction, TransformPlugin};

// ─────────────────────────────────────────────────────────────────────────────
// JsonParsePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the value of a string field as JSON.
///
/// If `target` is empty the parsed fields are merged into the root document.
/// Otherwise the parsed object is stored at `doc[target]`.
///
/// The original field is removed after successful parsing.
pub struct JsonParsePlugin {
    /// Source field name containing the raw JSON string.
    field: String,
    /// Destination field (empty ⇒ merge into root).
    target: String,
}

impl JsonParsePlugin {
    pub fn new(field: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            target: target.into(),
        }
    }
}

impl TransformPlugin for JsonParsePlugin {
    fn name(&self) -> &str {
        "json_parse"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let raw = match doc.get(&self.field).and_then(Value::as_str) {
            Some(s) => s.to_owned(),
            None => return ProcessAction::Pass,
        };

        match serde_json::from_str::<Value>(&raw) {
            Ok(parsed) => {
                // Remove source field.
                if let Some(obj) = doc.as_object_mut() {
                    obj.remove(&self.field);
                }

                if self.target.is_empty() {
                    // Merge into root.
                    if let (Some(root), Some(parsed_obj)) =
                        (doc.as_object_mut(), parsed.as_object())
                    {
                        for (k, v) in parsed_obj {
                            root.insert(k.clone(), v.clone());
                        }
                    }
                } else {
                    doc[&self.target] = parsed;
                }
                ProcessAction::Pass
            }
            Err(e) => {
                debug!(field = self.field.as_str(), error = %e, "json_parse: failed to parse field");
                ProcessAction::Pass // leave doc unchanged on parse failure
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TimestampParsePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a timestamp string into ISO 8601 format.
///
/// Supported input formats (tried in order):
/// 1. Unix epoch seconds (integer or float string)
/// 2. User-provided format strings (passed verbatim to `chrono`)
/// 3. Common formats: `%Y-%m-%dT%H:%M:%S%.f%z`, `%Y-%m-%d %H:%M:%S`,
///    `%d/%b/%Y:%H:%M:%S %z` (Apache/nginx), RFC 2822, RFC 3339
pub struct TimestampParsePlugin {
    /// Source field containing the raw timestamp string.
    field: String,
    /// Extra format strings (chrono strftime syntax).
    formats: Vec<String>,
    /// Destination field (defaults to `@timestamp` when `None`).
    target: Option<String>,
}

impl TimestampParsePlugin {
    pub fn new(
        field: impl Into<String>,
        formats: Vec<String>,
        target: Option<String>,
    ) -> Self {
        Self {
            field: field.into(),
            formats,
            target,
        }
    }

    fn target_field(&self) -> &str {
        self.target.as_deref().unwrap_or("@timestamp")
    }
}

impl TransformPlugin for TimestampParsePlugin {
    fn name(&self) -> &str {
        "timestamp_parse"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let raw = match doc.get(&self.field) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return ProcessAction::Pass,
        };

        // Try unix epoch (integer seconds).
        if let Ok(epoch) = raw.trim().parse::<i64>() {
            use chrono::{TimeZone, Utc};
            let dt = Utc.timestamp_opt(epoch, 0).single();
            if let Some(dt) = dt {
                let iso = dt.to_rfc3339();
                doc[self.target_field()] = Value::String(iso);
                return ProcessAction::Pass;
            }
        }

        // Try unix epoch float.
        if let Ok(epoch_f) = raw.trim().parse::<f64>() {
            use chrono::{TimeZone, Utc};
            let secs = epoch_f as i64;
            let nanos = ((epoch_f.fract()) * 1_000_000_000.0) as u32;
            let dt = Utc.timestamp_opt(secs, nanos).single();
            if let Some(dt) = dt {
                let iso = dt.to_rfc3339();
                doc[self.target_field()] = Value::String(iso);
                return ProcessAction::Pass;
            }
        }

        // Try user-supplied formats.
        for fmt in &self.formats {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&raw, fmt) {
                use chrono::TimeZone;
                let utc = chrono::Utc.from_utc_datetime(&dt);
                doc[self.target_field()] = Value::String(utc.to_rfc3339());
                return ProcessAction::Pass;
            }
        }

        // Try RFC 3339 / ISO 8601 (chrono's built-in parser).
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&raw) {
            doc[self.target_field()] = Value::String(dt.to_rfc3339());
            return ProcessAction::Pass;
        }

        // Try RFC 2822.
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&raw) {
            doc[self.target_field()] = Value::String(dt.to_rfc3339());
            return ProcessAction::Pass;
        }

        // Try Apache/nginx combined log format: "10/Apr/2026:12:00:00 +0000"
        if let Ok(dt) = chrono::DateTime::parse_from_str(&raw, "%d/%b/%Y:%H:%M:%S %z") {
            doc[self.target_field()] = Value::String(dt.to_rfc3339());
            return ProcessAction::Pass;
        }

        // Try "YYYY-MM-DD HH:MM:SS"
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S") {
            use chrono::TimeZone;
            let utc = chrono::Utc.from_utc_datetime(&dt);
            doc[self.target_field()] = Value::String(utc.to_rfc3339());
            return ProcessAction::Pass;
        }

        debug!(field = self.field.as_str(), raw = raw.as_str(), "timestamp_parse: no format matched");
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FieldRenamePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Rename document fields.
///
/// `mappings` maps `old_name → new_name`.  Fields not present in the document
/// are silently skipped.
pub struct FieldRenamePlugin {
    mappings: HashMap<String, String>,
}

impl FieldRenamePlugin {
    pub fn new(mappings: HashMap<String, String>) -> Self {
        Self { mappings }
    }
}

impl TransformPlugin for FieldRenamePlugin {
    fn name(&self) -> &str {
        "field_rename"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(obj) = doc.as_object_mut() {
            for (old, new) in &self.mappings {
                if let Some(val) = obj.remove(old.as_str()) {
                    obj.insert(new.clone(), val);
                }
            }
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DropFieldPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Remove one or more fields from a document.
pub struct DropFieldPlugin {
    fields: Vec<String>,
}

impl DropFieldPlugin {
    pub fn new(fields: Vec<String>) -> Self {
        Self { fields }
    }
}

impl TransformPlugin for DropFieldPlugin {
    fn name(&self) -> &str {
        "drop_field"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(obj) = doc.as_object_mut() {
            for field in &self.fields {
                obj.remove(field.as_str());
            }
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AddFieldPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Add a static field (or overwrite an existing one) with a fixed value.
pub struct AddFieldPlugin {
    field: String,
    value: Value,
}

impl AddFieldPlugin {
    pub fn new(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            value,
        }
    }
}

impl TransformPlugin for AddFieldPlugin {
    fn name(&self) -> &str {
        "add_field"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        doc[&self.field] = self.value.clone();
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RoutePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Route a document to a different target index based on a field value.
///
/// Looks up `doc[field]` in the `routes` map.  If a match is found, returns
/// [`ProcessAction::Route`] with the target index name.  Falls back to the
/// `default` index if configured; otherwise returns [`ProcessAction::Pass`].
pub struct RoutePlugin {
    /// Field to inspect.
    field: String,
    /// Field value → target index name.
    routes: HashMap<String, String>,
    /// Fallback target (used when no route matches).
    default: Option<String>,
}

impl RoutePlugin {
    pub fn new(
        field: impl Into<String>,
        routes: HashMap<String, String>,
        default: Option<String>,
    ) -> Self {
        Self {
            field: field.into(),
            routes,
            default,
        }
    }
}

impl TransformPlugin for RoutePlugin {
    fn name(&self) -> &str {
        "route"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let key = match doc.get(&self.field).and_then(Value::as_str) {
            Some(v) => v.to_owned(),
            None => {
                return self
                    .default
                    .as_ref()
                    .map(|d| ProcessAction::Route(d.clone()))
                    .unwrap_or(ProcessAction::Pass);
            }
        };

        if let Some(target) = self.routes.get(&key) {
            ProcessAction::Route(target.clone())
        } else if let Some(default) = &self.default {
            ProcessAction::Route(default.clone())
        } else {
            ProcessAction::Pass
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GrokPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Parse unstructured log text using Grok-style named patterns.
///
/// Supported pattern names:
/// - `NGINX_COMBINED` — standard nginx combined log format
/// - `APACHE_COMBINED` — Apache httpd combined log format
/// - `SYSLOG` — RFC 3164 syslog line
/// - `POSTGRESQL` — PostgreSQL log line
///
/// Extracted fields are merged into the root document.
pub struct GrokPlugin {
    /// Field containing the raw log line.
    field: String,
    /// Grok pattern name (e.g. `"NGINX_COMBINED"`).
    pattern_name: String,
    /// Compiled regex derived from the pattern.
    regex: Regex,
    /// Named capture group names in the order they appear in the regex.
    capture_names: Vec<Option<String>>,
}

impl GrokPlugin {
    pub fn new(field: impl Into<String>, pattern_name: impl Into<String>) -> Self {
        let pattern_name = pattern_name.into();
        let (regex_str, _) = grok_pattern(&pattern_name);
        let regex = Regex::new(regex_str).expect("built-in grok pattern must compile");
        let capture_names: Vec<Option<String>> = regex
            .capture_names()
            .map(|n| n.map(str::to_string))
            .collect();

        Self {
            field: field.into(),
            pattern_name,
            regex,
            capture_names,
        }
    }
}

impl TransformPlugin for GrokPlugin {
    fn name(&self) -> &str {
        "grok"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let text = match doc.get(&self.field).and_then(Value::as_str) {
            Some(s) => s.to_owned(),
            None => return ProcessAction::Pass,
        };

        if let Some(caps) = self.regex.captures(&text) {
            if let Some(obj) = doc.as_object_mut() {
                for name in self.capture_names.iter().flatten() {
                    if let Some(m) = caps.name(name) {
                        obj.insert(name.clone(), Value::String(m.as_str().to_string()));
                    }
                }
            }
        } else {
            debug!(
                field = self.field.as_str(),
                pattern = self.pattern_name.as_str(),
                "grok: pattern did not match"
            );
        }

        ProcessAction::Pass
    }
}

/// Returns `(regex_str, field_names)` for a built-in grok pattern.
fn grok_pattern(name: &str) -> (&'static str, &'static [&'static str]) {
    match name {
        "NGINX_COMBINED" | "APACHE_COMBINED" => (
            r#"^(?P<remote_addr>\S+) - (?P<remote_user>\S+) \[(?P<time_local>[^\]]+)\] "(?P<method>\S+) (?P<request_uri>\S+) (?P<http_version>[^"]+)" (?P<status>\d{3}) (?P<body_bytes_sent>\d+) "(?P<http_referer>[^"]*)" "(?P<http_user_agent>[^"]*)"#,
            &[
                "remote_addr", "remote_user", "time_local", "method", "request_uri",
                "http_version", "status", "body_bytes_sent", "http_referer", "http_user_agent",
            ],
        ),
        "SYSLOG" => (
            r#"^(?P<syslog_timestamp>\w{3}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}) (?P<hostname>\S+) (?P<program>[^\[:]+)(?:\[(?P<pid>\d+)\])?: (?P<message>.+)$"#,
            &["syslog_timestamp", "hostname", "program", "pid", "message"],
        ),
        "POSTGRESQL" => (
            r#"^(?P<log_time>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d+ \w+) \[(?P<pid>\d+)\] (?P<user_name>\S+)@(?P<database_name>\S+) (?P<severity>\w+):  (?P<message>.+)$"#,
            &["log_time", "pid", "user_name", "database_name", "severity", "message"],
        ),
        _ => (
            // Generic: capture everything as `message`.
            r#"^(?P<message>.+)$"#,
            &["message"],
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PiiRedactionPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Redact PII from string fields in a document.
///
/// Supported PII types:
/// - `email` — e.g. `user@example.com` → `[REDACTED_EMAIL]`
/// - `ip` — IPv4 and IPv6 addresses → `[REDACTED_IP]`
/// - `credit_card` — 13–19 digit numbers with optional separators →
///   `[REDACTED_CC]`
/// - `ssn` — US Social Security Numbers (`NNN-NN-NNNN`) → `[REDACTED_SSN]`
/// - `phone` — common phone number patterns → `[REDACTED_PHONE]`
pub struct PiiRedactionPlugin {
    /// Compiled `(regex, replacement)` pairs.
    patterns: Vec<(Regex, &'static str)>,
}

impl PiiRedactionPlugin {
    /// Build a new plugin that redacts the given PII types.
    ///
    /// `types` is a list of strings from `["email", "ip", "credit_card",
    /// "ssn", "phone"]`.  An empty list enables all types.
    pub fn new(types: Vec<String>) -> Self {
        let all = types.is_empty();

        let mut patterns = Vec::new();

        let wants = |t: &str| all || types.iter().any(|s| s == t);

        if wants("email") {
            patterns.push((
                Regex::new(r"(?i)[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}").unwrap(),
                "[REDACTED_EMAIL]",
            ));
        }
        if wants("ip") {
            // IPv4
            patterns.push((
                Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap(),
                "[REDACTED_IP]",
            ));
            // IPv6 (simplified)
            patterns.push((
                Regex::new(r"(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}").unwrap(),
                "[REDACTED_IP]",
            ));
        }
        if wants("credit_card") {
            patterns.push((
                Regex::new(r"\b(?:\d[ \-]?){13,19}\b").unwrap(),
                "[REDACTED_CC]",
            ));
        }
        if wants("ssn") {
            patterns.push((
                Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(),
                "[REDACTED_SSN]",
            ));
        }
        if wants("phone") {
            patterns.push((
                Regex::new(r"\b(?:\+?1[-.\s]?)?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}\b").unwrap(),
                "[REDACTED_PHONE]",
            ));
        }

        Self { patterns }
    }

    /// Redact PII from a single string.
    fn redact_str(&self, s: &str) -> String {
        let mut result = s.to_string();
        for (re, replacement) in &self.patterns {
            result = re.replace_all(&result, *replacement).into_owned();
        }
        result
    }

    /// Recursively redact all string values in a JSON value.
    fn redact_value(&self, v: &mut Value) {
        match v {
            Value::String(s) => {
                *s = self.redact_str(s);
            }
            Value::Object(obj) => {
                for val in obj.values_mut() {
                    self.redact_value(val);
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    self.redact_value(item);
                }
            }
            _ => {}
        }
    }
}

impl TransformPlugin for PiiRedactionPlugin {
    fn name(&self) -> &str {
        "pii_redaction"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        self.redact_value(doc);
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CopyFieldPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Copy the value of one field to another field.
///
/// The source field is left intact. If the source field does not exist the
/// document is passed through unchanged.
pub struct CopyFieldPlugin {
    source: String,
    target: String,
}

impl CopyFieldPlugin {
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
        }
    }
}

impl TransformPlugin for CopyFieldPlugin {
    fn name(&self) -> &str {
        "copy_field"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(val) = doc.get(&self.source).cloned() {
            doc[&self.target] = val;
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ConvertTypePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a field's value to a different type.
///
/// Supported target types: `integer`, `float`, `string`, `boolean`.
/// If conversion fails the document is passed through unchanged.
pub struct ConvertTypePlugin {
    field: String,
    target_type: String,
}

impl ConvertTypePlugin {
    pub fn new(field: impl Into<String>, target_type: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            target_type: target_type.into(),
        }
    }
}

impl TransformPlugin for ConvertTypePlugin {
    fn name(&self) -> &str {
        "convert"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let current = match doc.get(&self.field).cloned() {
            Some(v) => v,
            None => return ProcessAction::Pass,
        };

        let converted = match self.target_type.as_str() {
            "integer" => {
                let n = match &current {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.trim().parse::<i64>().ok(),
                    Value::Bool(b) => Some(if *b { 1 } else { 0 }),
                    _ => None,
                };
                n.map(|i| Value::Number(serde_json::Number::from(i)))
            }
            "float" => {
                let f = match &current {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.trim().parse::<f64>().ok(),
                    Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                    _ => None,
                };
                f.and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
            }
            "string" => {
                let s = match &current {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Null => Some("null".to_string()),
                    _ => None,
                };
                s.map(Value::String)
            }
            "boolean" => {
                let b = match &current {
                    Value::Bool(b) => Some(*b),
                    Value::String(s) => match s.to_lowercase().as_str() {
                        "true" | "1" | "yes" | "on" => Some(true),
                        "false" | "0" | "no" | "off" => Some(false),
                        _ => None,
                    },
                    Value::Number(n) => n.as_i64().map(|i| i != 0),
                    _ => None,
                };
                b.map(Value::Bool)
            }
            _ => None,
        };

        if let Some(v) = converted {
            doc[&self.field] = v;
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SplitPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Split a string field into an array using a separator.
///
/// The original string value is replaced with a JSON array of trimmed parts.
/// If the field is missing or not a string, the document passes through
/// unchanged.
pub struct SplitPlugin {
    field: String,
    separator: String,
}

impl SplitPlugin {
    pub fn new(field: impl Into<String>, separator: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            separator: separator.into(),
        }
    }
}

impl TransformPlugin for SplitPlugin {
    fn name(&self) -> &str {
        "split"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let s = match doc.get(&self.field).and_then(Value::as_str) {
            Some(s) => s.to_owned(),
            None => return ProcessAction::Pass,
        };
        let parts: Vec<Value> = s
            .split(self.separator.as_str())
            .map(|p| Value::String(p.trim().to_string()))
            .collect();
        doc[&self.field] = Value::Array(parts);
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LowercasePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Lowercase the string value of a field.
pub struct LowercasePlugin {
    field: String,
}

impl LowercasePlugin {
    pub fn new(field: impl Into<String>) -> Self {
        Self { field: field.into() }
    }
}

impl TransformPlugin for LowercasePlugin {
    fn name(&self) -> &str {
        "lowercase"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(Value::String(s)) = doc.get_mut(&self.field) {
            *s = s.to_lowercase();
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UppercasePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Uppercase the string value of a field.
pub struct UppercasePlugin {
    field: String,
}

impl UppercasePlugin {
    pub fn new(field: impl Into<String>) -> Self {
        Self { field: field.into() }
    }
}

impl TransformPlugin for UppercasePlugin {
    fn name(&self) -> &str {
        "uppercase"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(Value::String(s)) = doc.get_mut(&self.field) {
            *s = s.to_uppercase();
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SetPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Set a field to a value, optionally only if the field does not already exist.
///
/// When `override_existing` is `false` (the default), the field is only set if
/// it is absent from the document (or its value is `null`).  When `true` the
/// field is always overwritten — equivalent to [`AddFieldPlugin`].
pub struct SetPlugin {
    field: String,
    value: Value,
    override_existing: bool,
}

impl SetPlugin {
    pub fn new(field: impl Into<String>, value: Value, override_existing: bool) -> Self {
        Self {
            field: field.into(),
            value,
            override_existing,
        }
    }
}

impl TransformPlugin for SetPlugin {
    fn name(&self) -> &str {
        "set"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        let exists = doc
            .get(&self.field)
            .map(|v| !v.is_null())
            .unwrap_or(false);

        if self.override_existing || !exists {
            doc[&self.field] = self.value.clone();
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RemoveNullPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Remove all fields whose value is `null` or an empty string from the
/// top-level document object.
pub struct RemoveNullPlugin;

impl TransformPlugin for RemoveNullPlugin {
    fn name(&self) -> &str {
        "remove_null"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(obj) = doc.as_object_mut() {
            obj.retain(|_, v| !v.is_null() && v != &Value::String(String::new()));
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UrlDecodePlugin
// ─────────────────────────────────────────────────────────────────────────────

/// URL-decode (percent-decode) the string value of a field.
///
/// `+` is decoded as a space.  If the field is missing, not a string, or
/// decoding fails the document passes through unchanged.
pub struct UrlDecodePlugin {
    field: String,
}

impl UrlDecodePlugin {
    pub fn new(field: impl Into<String>) -> Self {
        Self { field: field.into() }
    }

    fn percent_decode(s: &str) -> Option<String> {
        let mut out = Vec::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'+' {
                out.push(b' ');
                i += 1;
            } else if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hi = (bytes[i + 1] as char).to_digit(16)? as u8;
                let lo = (bytes[i + 2] as char).to_digit(16)? as u8;
                out.push(hi << 4 | lo);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).ok()
    }
}

impl TransformPlugin for UrlDecodePlugin {
    fn name(&self) -> &str {
        "url_decode"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        if let Some(Value::String(s)) = doc.get_mut(&self.field) {
            if let Some(decoded) = Self::percent_decode(s) {
                *s = decoded;
            }
        }
        ProcessAction::Pass
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── JsonParsePlugin ──────────────────────────────────────────────────────

    #[test]
    fn json_parse_merges_into_root() {
        let plugin = JsonParsePlugin::new("raw", "");
        let mut doc = json!({ "raw": r#"{"level":"info","msg":"hi"}"# });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["level"], "info");
        assert_eq!(doc["msg"], "hi");
        assert!(doc.get("raw").is_none(), "source field should be removed");
    }

    #[test]
    fn json_parse_stores_at_target() {
        let plugin = JsonParsePlugin::new("payload", "parsed");
        let mut doc = json!({ "payload": r#"{"k":"v"}"# });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["parsed"]["k"], "v");
    }

    #[test]
    fn json_parse_passes_on_invalid_json() {
        let plugin = JsonParsePlugin::new("raw", "");
        let mut doc = json!({ "raw": "not-json" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        // Field left untouched when parse fails.
        assert_eq!(doc["raw"], "not-json");
    }

    #[test]
    fn json_parse_passes_when_field_missing() {
        let plugin = JsonParsePlugin::new("missing", "");
        let mut doc = json!({ "other": 1 });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
    }

    // ── TimestampParsePlugin ─────────────────────────────────────────────────

    #[test]
    fn timestamp_parse_iso8601() {
        let plugin = TimestampParsePlugin::new("ts", vec![], None);
        let mut doc = json!({ "ts": "2026-04-10T12:00:00Z" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        let ts = doc["@timestamp"].as_str().unwrap();
        assert!(ts.contains("2026-04-10"), "got: {ts}");
    }

    #[test]
    fn timestamp_parse_unix_epoch() {
        let plugin = TimestampParsePlugin::new("ts", vec![], None);
        let mut doc = json!({ "ts": "1744286400" }); // 2025-04-10 UTC approx
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert!(doc["@timestamp"].is_string());
    }

    #[test]
    fn timestamp_parse_apache_format() {
        let plugin = TimestampParsePlugin::new("ts", vec![], None);
        let mut doc = json!({ "ts": "10/Apr/2026:12:00:00 +0000" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        let ts = doc["@timestamp"].as_str().unwrap();
        assert!(ts.contains("2026-04-10"), "got: {ts}");
    }

    #[test]
    fn timestamp_parse_custom_format() {
        let plugin = TimestampParsePlugin::new(
            "ts",
            vec!["%Y/%m/%d %H:%M:%S".into()],
            Some("event.created".into()),
        );
        let mut doc = json!({ "ts": "2026/04/10 08:30:00" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        let ts = doc["event.created"].as_str().unwrap();
        assert!(ts.contains("2026-04-10"), "got: {ts}");
    }

    // ── FieldRenamePlugin ────────────────────────────────────────────────────

    #[test]
    fn field_rename_basic() {
        let plugin = FieldRenamePlugin::new(
            [("old_name".into(), "new_name".into())].into_iter().collect(),
        );
        let mut doc = json!({ "old_name": "value", "other": 1 });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["new_name"], "value");
        assert!(doc.get("old_name").is_none());
        assert_eq!(doc["other"], 1);
    }

    #[test]
    fn field_rename_missing_field_no_op() {
        let plugin = FieldRenamePlugin::new(
            [("does_not_exist".into(), "target".into())].into_iter().collect(),
        );
        let mut doc = json!({ "a": 1 });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert!(doc.get("target").is_none());
    }

    // ── DropFieldPlugin ──────────────────────────────────────────────────────

    #[test]
    fn drop_field_removes_fields() {
        let plugin = DropFieldPlugin::new(vec!["password".into(), "token".into()]);
        let mut doc = json!({ "user": "alice", "password": "secret", "token": "abc" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert!(doc.get("password").is_none());
        assert!(doc.get("token").is_none());
        assert_eq!(doc["user"], "alice");
    }

    #[test]
    fn drop_field_missing_field_no_op() {
        let plugin = DropFieldPlugin::new(vec!["missing".into()]);
        let mut doc = json!({ "a": 1 });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
    }

    // ── AddFieldPlugin ───────────────────────────────────────────────────────

    #[test]
    fn add_field_adds_new_field() {
        let plugin = AddFieldPlugin::new("env", json!("production"));
        let mut doc = json!({ "msg": "hello" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["env"], "production");
    }

    #[test]
    fn add_field_overwrites_existing() {
        let plugin = AddFieldPlugin::new("status", json!("new"));
        let mut doc = json!({ "status": "old" });
        plugin.process(&mut doc);
        assert_eq!(doc["status"], "new");
    }

    // ── RoutePlugin ──────────────────────────────────────────────────────────

    #[test]
    fn route_matches_field_value() {
        let routes = [
            ("error".into(), "logs-errors".into()),
            ("info".into(), "logs-info".into()),
        ]
        .into_iter()
        .collect();
        let plugin = RoutePlugin::new("level", routes, None);

        let mut doc = json!({ "level": "error", "msg": "oops" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Route("logs-errors".into()));
    }

    #[test]
    fn route_fallback_to_default() {
        let plugin = RoutePlugin::new(
            "level",
            HashMap::new(),
            Some("logs-misc".into()),
        );
        let mut doc = json!({ "level": "debug" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Route("logs-misc".into()));
    }

    #[test]
    fn route_pass_when_no_match_and_no_default() {
        let plugin = RoutePlugin::new("level", HashMap::new(), None);
        let mut doc = json!({ "level": "debug" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
    }

    // ── GrokPlugin ───────────────────────────────────────────────────────────

    #[test]
    fn grok_nginx_combined() {
        let plugin = GrokPlugin::new("message", "NGINX_COMBINED");
        let mut doc = json!({
            "message": r#"192.168.1.1 - alice [10/Apr/2026:12:00:00 +0000] "GET /index.html HTTP/1.1" 200 1234 "https://example.com" "Mozilla/5.0""#
        });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["remote_addr"], "192.168.1.1");
        assert_eq!(doc["status"], "200");
        assert_eq!(doc["method"], "GET");
    }

    #[test]
    fn grok_syslog() {
        let plugin = GrokPlugin::new("message", "SYSLOG");
        let mut doc = json!({
            "message": "Apr 10 12:00:00 myhost myapp[1234]: connection established"
        });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["hostname"], "myhost");
        assert_eq!(doc["program"], "myapp");
        assert_eq!(doc["pid"], "1234");
    }

    #[test]
    fn grok_no_match_passes_unchanged() {
        let plugin = GrokPlugin::new("message", "NGINX_COMBINED");
        let mut doc = json!({ "message": "totally unstructured text" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        // Original message should still be there.
        assert_eq!(doc["message"], "totally unstructured text");
    }

    // ── PiiRedactionPlugin ───────────────────────────────────────────────────

    #[test]
    fn pii_redacts_email() {
        let plugin = PiiRedactionPlugin::new(vec!["email".into()]);
        let mut doc = json!({ "msg": "contact user@example.com for info" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        let msg = doc["msg"].as_str().unwrap();
        assert!(!msg.contains("user@example.com"), "email not redacted: {msg}");
        assert!(msg.contains("[REDACTED_EMAIL]"), "got: {msg}");
    }

    #[test]
    fn pii_redacts_ipv4() {
        let plugin = PiiRedactionPlugin::new(vec!["ip".into()]);
        let mut doc = json!({ "remote": "192.168.1.100 connected" });
        assert_eq!(plugin.process(&mut doc), ProcessAction::Pass);
        let remote = doc["remote"].as_str().unwrap();
        assert!(remote.contains("[REDACTED_IP]"), "got: {remote}");
    }

    #[test]
    fn pii_redacts_multiple_types() {
        let plugin = PiiRedactionPlugin::new(vec!["email".into(), "ip".into()]);
        let mut doc = json!({
            "msg": "user@example.com from 10.0.0.1"
        });
        plugin.process(&mut doc);
        let msg = doc["msg"].as_str().unwrap();
        assert!(!msg.contains("user@example.com"));
        assert!(!msg.contains("10.0.0.1"));
    }

    #[test]
    fn pii_redacts_nested_fields() {
        let plugin = PiiRedactionPlugin::new(vec!["email".into()]);
        let mut doc = json!({
            "user": {
                "email": "test@test.com",
                "name": "Alice"
            }
        });
        plugin.process(&mut doc);
        let email = doc["user"]["email"].as_str().unwrap();
        assert!(email.contains("[REDACTED_EMAIL]"), "got: {email}");
        assert_eq!(doc["user"]["name"], "Alice");
    }

    #[test]
    fn pii_empty_types_redacts_all() {
        let plugin = PiiRedactionPlugin::new(vec![]);
        let mut doc = json!({ "msg": "email: foo@bar.com, ip: 1.2.3.4" });
        plugin.process(&mut doc);
        let msg = doc["msg"].as_str().unwrap();
        assert!(!msg.contains("foo@bar.com"));
        assert!(!msg.contains("1.2.3.4"));
    }
}
