//! Core domain types for the xerj engine.
//!
//! These types flow through every layer of the system. Keeping them in
//! `xerj-common` prevents circular crate dependencies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

use crate::error::XerjError;

// ═════════════════════════════════════════════════════════════════════════════
// Primitive IDs
// ═════════════════════════════════════════════════════════════════════════════

/// Internal document identifier — a monotonically increasing u64 assigned by
/// the storage layer. Not exposed through the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocId(pub u64);

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for DocId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

/// Segment identifier — unique within an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub u64);

impl fmt::Display for SegmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for SegmentId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

/// Monotonic sequence number — used by the WAL to order operations globally.
/// Also serves as the external document version when no explicit version is
/// provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SeqNo(pub u64);

impl fmt::Display for SeqNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for SeqNo {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// IndexName
// ═════════════════════════════════════════════════════════════════════════════

/// A validated index name.
///
/// Rules (intentionally stricter than Elasticsearch):
/// - Lowercase ASCII letters, digits, `-`, and `_` only.
/// - Must start with a letter.
/// - 1–255 characters.
/// - Must not start with `_` or `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IndexName(String);

impl IndexName {
    /// Create a new `IndexName`, validating the input.
    pub fn new(name: impl Into<String>) -> Result<Self, XerjError> {
        let name = name.into();
        Self::validate(&name)?;
        Ok(Self(name))
    }

    /// Returns the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(name: &str) -> Result<(), XerjError> {
        if name.len() > 255 {
            return Err(XerjError::invalid_mapping(format!(
                "index name too long: {} chars (max 255)",
                name.len()
            )));
        }
        // chars().next() returns None iff `name` is empty — fold the
        // empty-name check into the extraction so the two facts can't
        // drift apart in a future refactor.
        let Some(first) = name.chars().next() else {
            return Err(XerjError::invalid_mapping("index name cannot be empty"));
        };
        // Allow leading '.' for system indices like .kibana, .security-* etc.
        if !first.is_ascii_lowercase() && first != '.' {
            return Err(XerjError::invalid_mapping(format!(
                "index name must start with a lowercase letter or '.', got '{first}'"
            )));
        }
        // After a leading dot, the rest of the name is validated normally.
        let rest = if first == '.' { &name[1..] } else { name };
        for ch in rest.chars() {
            if !matches!(ch, 'a'..='z' | '0'..='9' | '-' | '_' | '.') {
                return Err(XerjError::invalid_mapping(format!(
                    "index name contains invalid character '{ch}' \
                     (only lowercase letters, digits, '-', '_', '.' allowed)"
                )));
            }
        }
        Ok(())
    }
}

impl fmt::Display for IndexName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IndexName {
    type Err = XerjError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for IndexName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Field types
// ═════════════════════════════════════════════════════════════════════════════

/// Every data type a field can have in a xerj index.
///
/// Designed to be a superset of Elasticsearch's core types while adding
/// `Chunk` (chunked text for RAG) and `Vector` as first-class citizens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// Analysed text — tokenised, not aggregatable.
    Text,
    /// Exact-match string — not tokenised, aggregatable (like ES `keyword`).
    Keyword,
    /// 64-bit signed integer.
    Long,
    /// 64-bit IEEE 754 float.
    Double,
    /// Boolean (`true` / `false`).
    Boolean,
    /// ISO-8601 timestamp or milliseconds-since-epoch.
    Date,
    /// IPv4 or IPv6 address stored as a 128-bit integer.
    Ip,
    /// Dense floating-point vector for ANN search.
    Vector,
    /// Pre-chunked text fragment with an associated embedding vector.
    /// Stores the chunk text, its vector, metadata, and a parent document
    /// reference — designed for RAG retrieval pipelines.
    Chunk,
    /// Latitude/longitude point.
    GeoPoint,
    /// Raw bytes, stored Base64-encoded in JSON responses.
    Binary,
    /// Nested JSON object stored as a sub-document with its own field namespace.
    Object,
    /// Nested array of objects, each queryable independently (like ES `nested`).
    Nested,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Text => "text",
            Self::Keyword => "keyword",
            Self::Long => "long",
            Self::Double => "double",
            Self::Boolean => "boolean",
            Self::Date => "date",
            Self::Ip => "ip",
            Self::Vector => "vector",
            Self::Chunk => "chunk",
            Self::GeoPoint => "geo_point",
            Self::Binary => "binary",
            Self::Object => "object",
            Self::Nested => "nested",
        };
        f.write_str(s)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Field options
// ═════════════════════════════════════════════════════════════════════════════

/// Per-field configuration options.
///
/// Not all options apply to all field types; inapplicable options are ignored
/// at index-build time with a warning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldOptions {
    /// Enable full-text analysis. Only meaningful for `Text` fields (default: `true` for `Text`).
    #[serde(default = "bool_true")]
    pub analyzed: bool,
    /// Store the original field value for retrieval (default: `true`).
    #[serde(default = "bool_true")]
    pub stored: bool,
    /// Build an inverted index / BKD tree for this field (default: `true`).
    #[serde(default = "bool_true")]
    pub indexed: bool,
    /// Build doc-values column store for sorting and aggregation (default: `true`).
    #[serde(default = "bool_true")]
    pub doc_values: bool,
    /// Override the analyzer for this field (inherits from `fts.default_analyzer` if `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,
    /// Include term position information for phrase queries (default: `true` for `Text`).
    #[serde(default = "bool_true")]
    pub term_positions: bool,
    /// Dimensionality — required for `Vector` and `Chunk` fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    /// Similarity metric override for this vector field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<String>,
    /// Vector quantization scheme for this dense_vector field.
    ///
    /// `Some("scalar8")` opts this field into the serving-path SQ8 code store
    /// (1 byte/dim, ~4× memory reduction); `None`/absent keeps the exact
    /// full-precision f32 brute-force path. Set from the mapping's
    /// `index_options.type` (`int8_hnsw`/`int8_flat` → `scalar8`) in es_compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    /// Null value to substitute when the field is missing from a document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub null_value: Option<serde_json::Value>,
    /// Allow the field to appear more than once in a document (always `true` for arrays).
    #[serde(default = "bool_true")]
    pub multi_value: bool,
    /// Boost factor for relevance scoring (default: `1.0`).
    #[serde(default = "float_one")]
    pub boost: f32,
}

fn bool_true() -> bool {
    true
}
fn float_one() -> f32 {
    1.0
}

impl Default for FieldOptions {
    fn default() -> Self {
        Self {
            analyzed: true,
            stored: true,
            indexed: true,
            doc_values: true,
            analyzer: None,
            term_positions: true,
            dimensions: None,
            similarity: None,
            quantization: None,
            null_value: None,
            multi_value: true,
            boost: 1.0,
        }
    }
}

/// Optional embedding configuration attached to a field.
///
/// When present, the engine calls the configured embedding service to
/// automatically vectorise this field's text on ingest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Override the global `embedding.default_endpoint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Override the global `embedding.default_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Target field name where the resulting vector should be stored.
    /// Defaults to `"<field_name>_vector"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_field: Option<String>,
}

// ═════════════════════════════════════════════════════════════════════════════
// FieldConfig
// ═════════════════════════════════════════════════════════════════════════════

/// Complete configuration for a single field in an index mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldConfig {
    /// Fully-qualified field name (dot-separated for nested fields).
    pub name: String,
    /// Data type.
    pub field_type: FieldType,
    /// Indexing and storage options.
    #[serde(default)]
    pub options: FieldOptions,
    /// Optional embedding pipeline configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingConfig>,
    /// Sub-fields (for `Object` and `Nested` fields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldConfig>,
}

impl FieldConfig {
    /// Create a simple field with default options.
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            options: FieldOptions::default(),
            embedding: None,
            fields: Vec::new(),
        }
    }

    /// Builder: set a specific option.
    pub fn with_options(mut self, options: FieldOptions) -> Self {
        self.options = options;
        self
    }

    /// Builder: attach an embedding configuration.
    pub fn with_embedding(mut self, embedding: EmbeddingConfig) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /// Returns `true` if this field should produce an inverted index entry.
    pub fn is_searchable(&self) -> bool {
        self.options.indexed
    }

    /// Returns `true` if this field can be used in sorting and aggregations.
    pub fn is_aggregatable(&self) -> bool {
        self.options.doc_values && !matches!(self.field_type, FieldType::Text | FieldType::Binary)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Schema
// ═════════════════════════════════════════════════════════════════════════════

/// Index schema — the ordered collection of field configs plus version metadata.
///
/// See also the richer [`crate::schema`] module which wraps this type with
/// dynamic mapping logic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    /// All fields in definition order.
    pub fields: Vec<FieldConfig>,
    /// Schema version — incremented on every `add_field` call.
    pub version: u64,
    /// Timestamp of the last schema modification.
    pub updated_at: DateTime<Utc>,
}

impl Schema {
    /// Create an empty schema at version 0.
    pub fn empty() -> Self {
        Self {
            fields: Vec::new(),
            version: 0,
            updated_at: Utc::now(),
        }
    }

    /// Return the field config for the given name, if it exists.
    pub fn field(&self, name: &str) -> Option<&FieldConfig> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Return `true` if a field with this name already exists.
    pub fn has_field(&self, name: &str) -> bool {
        self.field(name).is_some()
    }

    /// Add a field, bumping the schema version.
    ///
    /// Returns an error if a field with this name already exists.
    pub fn add_field(&mut self, field: FieldConfig) -> Result<(), XerjError> {
        if self.has_field(&field.name) {
            return Err(XerjError::invalid_mapping(format!(
                "field '{}' already exists in schema",
                field.name
            )));
        }
        self.fields.push(field);
        self.version += 1;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Total number of fields (counting only top-level fields).
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

impl Default for Schema {
    fn default() -> Self {
        Self::empty()
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Document
// ═════════════════════════════════════════════════════════════════════════════

/// A document in the index.
///
/// `Document` is the boundary type between the API layer and the storage
/// layer. The `source` field holds the raw JSON as provided by the indexing
/// request; the engine does not transform it beyond validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// User-visible document ID (string for API compatibility).
    pub id: String,
    /// Internal document ID assigned by the storage layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<DocId>,
    /// Sequence number at the time this version was written.
    pub seq_no: SeqNo,
    /// Document version — incremented on every update.
    pub version: u64,
    /// The raw document source.
    pub source: serde_json::Value,
    /// Index this document belongs to.
    pub index: IndexName,
    /// Ingestion timestamp.
    pub timestamp: DateTime<Utc>,
}

impl Document {
    /// Create a new document with a generated UUID as the ID.
    pub fn new(index: IndexName, source: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            doc_id: None,
            seq_no: SeqNo(0),
            version: 1,
            source,
            index,
            timestamp: Utc::now(),
        }
    }

    /// Create a document with an explicit user-provided ID.
    pub fn with_id(id: impl Into<String>, index: IndexName, source: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            doc_id: None,
            seq_no: SeqNo(0),
            version: 1,
            source,
            index,
            timestamp: Utc::now(),
        }
    }

    /// Returns the source JSON object, or `None` if the source is not an object.
    pub fn source_object(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.source.as_object()
    }

    /// Look up a field value in the source by dot-separated path.
    ///
    /// For example, `doc.get_field("user.name")` navigates `source["user"]["name"]`.
    pub fn get_field(&self, path: &str) -> Option<&serde_json::Value> {
        let mut current = &self.source;
        for segment in path.split('.') {
            current = current.get(segment)?;
        }
        Some(current)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_name_valid() {
        assert!(IndexName::new("my-index-01").is_ok());
        assert!(IndexName::new("logs").is_ok());
        assert!(IndexName::new("a").is_ok());
        // System indices with leading dot are allowed.
        assert!(IndexName::new(".kibana").is_ok());
        assert!(IndexName::new(".kibana_1").is_ok());
        assert!(IndexName::new(".security-7").is_ok());
    }

    #[test]
    fn index_name_invalid() {
        assert!(IndexName::new("").is_err()); // empty
        assert!(IndexName::new("MyIndex").is_err()); // uppercase
        assert!(IndexName::new("_private").is_err()); // starts with _
        assert!(IndexName::new("bad name").is_err()); // space
        assert!(IndexName::new("1bad").is_err()); // starts with digit
    }

    #[test]
    fn schema_field_add_dedup() {
        let mut schema = Schema::empty();
        schema
            .add_field(FieldConfig::new("title", FieldType::Text))
            .unwrap();
        assert_eq!(schema.version, 1);

        let result = schema.add_field(FieldConfig::new("title", FieldType::Keyword));
        assert!(result.is_err(), "duplicate field should be rejected");
    }

    #[test]
    fn document_get_nested_field() {
        let source = serde_json::json!({
            "user": { "name": "Alice", "age": 30 }
        });
        let idx = IndexName::new("test").unwrap();
        let doc = Document::new(idx, source);

        assert_eq!(
            doc.get_field("user.name"),
            Some(&serde_json::json!("Alice"))
        );
        assert_eq!(doc.get_field("user.missing"), None);
        assert_eq!(doc.get_field("missing"), None);
    }

    #[test]
    fn field_types_round_trip_json() {
        let types = [
            FieldType::Text,
            FieldType::Vector,
            FieldType::Chunk,
            FieldType::GeoPoint,
        ];
        for ft in &types {
            let json = serde_json::to_string(ft).unwrap();
            let back: FieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(*ft, back);
        }
    }
}
