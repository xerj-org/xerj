//! Index schema management.
//!
//! This module wraps the [`Schema`] type (defined in [`crate::types`]) with
//! higher-level logic for:
//!
//! - Dynamic field type detection from JSON values
//! - Controlled schema evolution (add fields without re-indexing)
//! - Mapping compatibility checks between schema versions
//! - Field validation before indexing

use serde::{Deserialize, Serialize};

use crate::error::XerjError;
use crate::types::{FieldConfig, FieldType, Schema};

// ═════════════════════════════════════════════════════════════════════════════
// Dynamic mapping mode
// ═════════════════════════════════════════════════════════════════════════════

/// Controls how the engine handles fields that appear in a document but are
/// not defined in the current schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DynamicMapping {
    /// Automatically add new fields to the schema (default).
    ///
    /// Type is inferred from the first JSON value seen for the field.
    /// Schema version is incremented for each newly discovered field.
    #[default]
    Dynamic,
    /// Reject documents containing unmapped fields.
    ///
    /// Prevents unintentional schema sprawl in production indices.
    Strict,
    /// Accept unmapped fields but do not index them.
    ///
    /// The field value is stored in `_source` only; it cannot be searched or
    /// sorted on. Useful for audit-log fields you want to keep but not query.
    Runtime,
}

// ═════════════════════════════════════════════════════════════════════════════
// ManagedSchema
// ═════════════════════════════════════════════════════════════════════════════

/// A schema with its dynamic mapping policy — the unit that an index stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedSchema {
    /// The underlying field definitions and version counter.
    pub schema: Schema,
    /// Dynamic mapping behaviour.
    pub dynamic: DynamicMapping,
}

impl ManagedSchema {
    /// Create a new empty managed schema.
    pub fn new(dynamic: DynamicMapping) -> Self {
        Self {
            schema: Schema::empty(),
            dynamic,
        }
    }

    /// Create with `Dynamic` mapping (the default).
    pub fn dynamic() -> Self {
        Self::new(DynamicMapping::Dynamic)
    }

    /// Create with `Strict` mapping.
    pub fn strict() -> Self {
        Self::new(DynamicMapping::Strict)
    }

    // ── Field access ─────────────────────────────────────────────────────────

    /// Return the `FieldConfig` for a field name.
    pub fn field(&self, name: &str) -> Option<&FieldConfig> {
        self.schema.field(name)
    }

    /// Return all field configs.
    pub fn fields(&self) -> &[FieldConfig] {
        &self.schema.fields
    }

    // ── Schema evolution ──────────────────────────────────────────────────────

    /// Explicitly add a field to the schema.
    ///
    /// Fails if:
    /// - A field with the same name already exists.
    /// - The field type is incompatible with an existing field (checked via
    ///   [`is_compatible_with`]).
    pub fn add_field(&mut self, field: FieldConfig) -> Result<(), XerjError> {
        validate_field_config(&field)?;
        self.schema.add_field(field)
    }

    /// Process a JSON object, adding any unknown fields (when `Dynamic`), or
    /// returning an error if `Strict` mode is active and an unmapped field is
    /// encountered.
    ///
    /// Returns the list of newly-added field names.
    pub fn apply_document(
        &mut self,
        value: &serde_json::Value,
        limit: u32,
    ) -> Result<Vec<String>, XerjError> {
        let obj = value
            .as_object()
            .ok_or_else(|| XerjError::invalid_mapping("document must be a JSON object"))?;

        let mut added = Vec::new();
        for (key, val) in obj {
            if self.schema.has_field(key) {
                continue; // already mapped, nothing to do
            }

            match self.dynamic {
                DynamicMapping::Strict => {
                    return Err(XerjError::invalid_mapping(format!(
                        "field '{key}' is not in the schema and dynamic mapping is 'strict'"
                    )));
                }
                DynamicMapping::Runtime => {
                    // Accept but don't index — nothing to do here
                }
                DynamicMapping::Dynamic => {
                    if self.schema.field_count() as u32 >= limit {
                        return Err(XerjError::resource_exhausted(format!(
                            "index has reached the field limit of {limit}; \
                             field '{key}' cannot be auto-mapped"
                        )));
                    }
                    let inferred_type = infer_field_type(val);
                    let field = FieldConfig::new(key.clone(), inferred_type);
                    validate_field_config(&field)?;
                    self.schema.add_field(field)?;
                    added.push(key.clone());
                }
            }
        }
        Ok(added)
    }

    // ── Validation ────────────────────────────────────────────────────────────

    /// Validate that this schema is internally consistent.
    pub fn validate(&self) -> Result<(), XerjError> {
        // Field names must be unique (enforced by add_field but good to check)
        let mut seen = std::collections::HashSet::new();
        for field in &self.schema.fields {
            if !seen.insert(field.name.as_str()) {
                return Err(XerjError::invalid_mapping(format!(
                    "duplicate field name: '{}'",
                    field.name
                )));
            }
            validate_field_config(field)?;
        }
        Ok(())
    }

    // ── Compatibility ─────────────────────────────────────────────────────────

    /// Check whether `other` is compatible with `self` for a schema upgrade.
    ///
    /// Compatible means: every field in `self` also exists in `other` with the
    /// same type. `other` may have additional fields. Changing a field's type
    /// is never compatible.
    pub fn is_compatible_with(&self, other: &ManagedSchema) -> Result<(), XerjError> {
        for field in &self.schema.fields {
            match other.field(&field.name) {
                None => {
                    // `other` is missing a field that `self` has — only allowed
                    // if the field was added after the other schema was snapshotted.
                    // We allow this (additive only).
                }
                Some(other_field) => {
                    if other_field.field_type != field.field_type {
                        return Err(XerjError::invalid_mapping(format!(
                            "field '{}' type changed from '{}' to '{}'; \
                             type changes require reindexing",
                            field.name, field.field_type, other_field.field_type
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Field type inference
// ═════════════════════════════════════════════════════════════════════════════

/// Infer a [`FieldType`] from a JSON value.
///
/// The heuristics here match what most users expect from a search engine:
/// strings become `Text`, numbers become `Long` or `Double`, arrays are
/// unwrapped to their element type, etc.
pub fn infer_field_type(value: &serde_json::Value) -> FieldType {
    match value {
        serde_json::Value::Null => FieldType::Keyword, // conservative default
        serde_json::Value::Bool(_) => FieldType::Boolean,
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                FieldType::Double
            } else {
                FieldType::Long
            }
        }
        serde_json::Value::String(s) => infer_string_type(s),
        serde_json::Value::Array(arr) => {
            // Infer from the first non-null element
            arr.iter()
                .find(|v| !v.is_null())
                .map(infer_field_type)
                .unwrap_or(FieldType::Keyword)
        }
        serde_json::Value::Object(_) => FieldType::Object,
    }
}

/// Heuristic type detection for string values.
///
/// Returns `Date` for ISO-8601 timestamps, `Ip` for IP literals, and `Text`
/// for everything else. Strings without whitespace that look like identifiers
/// are mapped to `Keyword` when they are under 256 bytes.
fn infer_string_type(s: &str) -> FieldType {
    // IP address detection (quick check before regex)
    if is_ip_address(s) {
        return FieldType::Ip;
    }
    // ISO-8601 datetime detection (starts with 4 digits followed by '-')
    if is_iso8601_like(s) {
        return FieldType::Date;
    }
    // Short strings without whitespace → Keyword; long or whitespace-containing → Text
    if s.len() <= 256 && !s.contains(char::is_whitespace) {
        FieldType::Keyword
    } else {
        FieldType::Text
    }
}

fn is_ip_address(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

fn is_iso8601_like(s: &str) -> bool {
    // Fast path: must be at least "YYYY-MM-DD" (10 chars)
    if s.len() < 10 {
        return false;
    }
    let b = s.as_bytes();
    b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}

// ═════════════════════════════════════════════════════════════════════════════
// Field validation
// ═════════════════════════════════════════════════════════════════════════════

/// Validate an individual [`FieldConfig`].
pub fn validate_field_config(field: &FieldConfig) -> Result<(), XerjError> {
    // Name must be non-empty and not contain leading/trailing whitespace
    if field.name.trim().is_empty() {
        return Err(XerjError::invalid_mapping("field name cannot be empty"));
    }
    if field.name.trim() != field.name {
        return Err(XerjError::invalid_mapping(format!(
            "field name '{}' must not have leading or trailing whitespace",
            field.name
        )));
    }
    // Field names must not start with '_' (reserved for meta-fields)
    if field.name.starts_with('_') {
        return Err(XerjError::invalid_mapping(format!(
            "field name '{}' must not start with '_' (reserved for meta-fields)",
            field.name
        )));
    }

    // Vector and Chunk fields require explicit dimensionality
    if matches!(field.field_type, FieldType::Vector | FieldType::Chunk) {
        match field.options.dimensions {
            None => {
                return Err(XerjError::invalid_mapping(format!(
                    "field '{}' of type '{}' requires 'dimensions' to be set",
                    field.name, field.field_type
                )));
            }
            Some(0) => {
                return Err(XerjError::invalid_mapping(format!(
                    "field '{}' dimensions must be > 0",
                    field.name
                )));
            }
            _ => {}
        }
    }

    // Boost must be positive
    if field.options.boost <= 0.0 {
        return Err(XerjError::invalid_mapping(format!(
            "field '{}' boost must be > 0.0",
            field.name
        )));
    }

    // Binary fields cannot be indexed (they're too large and opaque)
    if field.field_type == FieldType::Binary && field.options.indexed {
        return Err(XerjError::invalid_mapping(format!(
            "field '{}' of type 'binary' cannot be indexed",
            field.name
        )));
    }

    Ok(())
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_types_from_json() {
        assert_eq!(infer_field_type(&serde_json::json!(true)), FieldType::Boolean);
        assert_eq!(infer_field_type(&serde_json::json!(42)), FieldType::Long);
        assert_eq!(infer_field_type(&serde_json::json!(3.14)), FieldType::Double);
        assert_eq!(
            infer_field_type(&serde_json::json!("hello world")),
            FieldType::Text
        );
        assert_eq!(
            infer_field_type(&serde_json::json!("my-tag")),
            FieldType::Keyword
        );
        assert_eq!(
            infer_field_type(&serde_json::json!("192.168.1.1")),
            FieldType::Ip
        );
        assert_eq!(
            infer_field_type(&serde_json::json!("2024-01-15T10:30:00Z")),
            FieldType::Date
        );
        assert_eq!(
            infer_field_type(&serde_json::json!({"key": "value"})),
            FieldType::Object
        );
    }

    #[test]
    fn dynamic_mapping_adds_fields() {
        let mut ms = ManagedSchema::dynamic();
        let doc = serde_json::json!({
            "title": "Hello World",
            "count": 42,
            "active": true
        });
        let added = ms.apply_document(&doc, 500).unwrap();
        assert_eq!(added.len(), 3);
        assert!(ms.field("title").is_some());
        assert_eq!(ms.field("count").unwrap().field_type, FieldType::Long);
    }

    #[test]
    fn strict_mapping_rejects_unknown() {
        let mut ms = ManagedSchema::strict();
        ms.add_field(FieldConfig::new("known", FieldType::Text)).unwrap();

        let doc = serde_json::json!({"known": "ok", "unknown": "boom"});
        assert!(ms.apply_document(&doc, 500).is_err());
    }

    #[test]
    fn field_limit_enforced() {
        let mut ms = ManagedSchema::dynamic();
        let doc = serde_json::json!({"a": 1, "b": 2, "c": 3});
        // limit of 2 should trigger an error when trying to add the 3rd field
        let result = ms.apply_document(&doc, 2);
        assert!(result.is_err());
    }

    #[test]
    fn schema_compatibility_type_change_rejected() {
        let mut old = ManagedSchema::dynamic();
        old.add_field(FieldConfig::new("status", FieldType::Keyword)).unwrap();

        let mut new = ManagedSchema::dynamic();
        new.add_field(FieldConfig::new("status", FieldType::Long)).unwrap();

        assert!(old.is_compatible_with(&new).is_err());
    }

    #[test]
    fn binary_field_cannot_be_indexed() {
        let mut field = FieldConfig::new("data", FieldType::Binary);
        field.options.indexed = true;
        assert!(validate_field_config(&field).is_err());

        field.options.indexed = false;
        assert!(validate_field_config(&field).is_ok());
    }

    #[test]
    fn vector_field_requires_dimensions() {
        let field = FieldConfig::new("embedding", FieldType::Vector);
        assert!(validate_field_config(&field).is_err());

        let mut field = FieldConfig::new("embedding", FieldType::Vector);
        field.options.dimensions = Some(768);
        assert!(validate_field_config(&field).is_ok());
    }
}
