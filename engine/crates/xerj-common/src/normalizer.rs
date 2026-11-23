//! Keyword normalizers (#3513).
//!
//! A normalizer is the keyword-field counterpart of an analyzer: it rewrites
//! the whole value into one normalized term, and the same rewrite runs on the
//! values a query compares against that field. ES applies it before a keyword
//! is indexed (terms, doc values, aggregation keys) and to the value of every
//! term-level and full-text query that targets the field. `_source` keeps the
//! original value.
//!
//! This build supports the built-in `lowercase` normalizer and `custom`
//! normalizers whose filters are any of `lowercase`, `uppercase`,
//! `asciifolding` and `trim` (by built-in name, or declared under
//! `analysis.filter` with no options this build would ignore). Anything else
//! is refused at mapping time with the reason, never accepted and ignored.
//!
//! The resolved filter chain is stored on the field's
//! [`crate::types::FieldOptions::normalizer`], so the engine never needs the
//! index's analysis settings to apply it, and a schema written before this
//! field existed has no normalizer (existing indices are unchanged).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{FieldConfig, FieldType, Schema};

/// The per-character filters a normalizer may chain in this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizerFilter {
    /// Unicode lower-casing, one code point at a time.
    Lowercase,
    /// Unicode upper-casing, one code point at a time.
    Uppercase,
    /// Folds Latin letters with diacritics to their ASCII base letters,
    /// keeping case (`À` → `A`, `é` → `e`, `Æ` → `AE`, `ß` → `ss`).
    Asciifolding,
    /// Removes leading and trailing whitespace.
    Trim,
}

impl NormalizerFilter {
    /// The filter's ES name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lowercase => "lowercase",
            Self::Uppercase => "uppercase",
            Self::Asciifolding => "asciifolding",
            Self::Trim => "trim",
        }
    }

    fn from_builtin_name(name: &str) -> Option<Self> {
        match name {
            "lowercase" => Some(Self::Lowercase),
            "uppercase" => Some(Self::Uppercase),
            "asciifolding" => Some(Self::Asciifolding),
            "trim" => Some(Self::Trim),
            _ => None,
        }
    }

    fn apply(self, s: &str) -> String {
        match self {
            Self::Lowercase => s.chars().flat_map(char::to_lowercase).collect(),
            Self::Uppercase => s.chars().flat_map(char::to_uppercase).collect(),
            Self::Asciifolding => fold_to_ascii(s),
            Self::Trim => s.trim().to_string(),
        }
    }
}

/// Filters ES allows in a normalizer that this build does not implement.
/// Naming one is refused with "not supported", not with ES's "may not use".
const ES_NORMALIZER_FILTERS_NOT_BUILT: &[&str] = &[
    "arabic_normalization",
    "bengali_normalization",
    "cjk_width",
    "decimal_digit",
    "elision",
    "german_normalization",
    "hindi_normalization",
    "indic_normalization",
    "pattern_replace",
    "persian_normalization",
    "scandinavian_folding",
    "serbian_normalization",
    "sorani_normalization",
];

const SUPPORTED: &str = "lowercase, uppercase, asciifolding, trim";

/// A resolved normalizer: its name and the filters it runs, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordNormalizer {
    /// The name the mapping used (`lowercase`, or a custom normalizer's name).
    pub name: String,
    /// The filter chain, applied left to right.
    pub filters: Vec<NormalizerFilter>,
}

/// Why a normalizer name could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizerError {
    /// No normalizer of that name is declared and it is not a built-in.
    NotFound,
    /// The normalizer is declared but uses something this build cannot apply.
    Unsupported(String),
}

impl KeywordNormalizer {
    /// ES's built-in `lowercase` normalizer.
    pub fn builtin_lowercase() -> Self {
        Self {
            name: "lowercase".to_string(),
            filters: vec![NormalizerFilter::Lowercase],
        }
    }

    /// Normalize one keyword value.
    pub fn normalize(&self, s: &str) -> String {
        let mut out = s.to_string();
        for f in &self.filters {
            out = f.apply(&out);
        }
        out
    }

    /// Normalize the string values in a JSON value: a string, or every string
    /// inside an array (at any depth). Numbers, booleans, objects and nulls are
    /// returned unchanged.
    pub fn normalize_json(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.normalize(s)),
            Value::Array(arr) => Value::Array(arr.iter().map(|e| self.normalize_json(e)).collect()),
            other => other.clone(),
        }
    }

    /// Resolve `name` against an index's `analysis` block (the object that
    /// holds `normalizer`, `filter` and `char_filter`), or against the
    /// built-ins when `analysis` is `None`.
    ///
    /// A declared normalizer wins over the built-in of the same name.
    pub fn resolve(name: &str, analysis: Option<&Value>) -> Result<Self, NormalizerError> {
        if let Some(def) = analysis
            .and_then(|a| a.get("normalizer"))
            .and_then(|n| n.get(name))
        {
            return Self::from_custom(name, def, analysis);
        }
        if name == "lowercase" {
            return Ok(Self::builtin_lowercase());
        }
        Err(NormalizerError::NotFound)
    }

    fn from_custom(
        name: &str,
        def: &Value,
        analysis: Option<&Value>,
    ) -> Result<Self, NormalizerError> {
        let unsupported = |m: String| Err(NormalizerError::Unsupported(m));
        let Some(obj) = def.as_object() else {
            return unsupported(format!(
                "normalizer [{name}] must be an object with a [filter] list"
            ));
        };
        match obj.get("type").and_then(Value::as_str) {
            None | Some("custom") => {}
            Some(other) => {
                return unsupported(format!("Unknown normalizer type [{other}] for [{name}]"))
            }
        }
        for key in obj.keys() {
            if !matches!(key.as_str(), "type" | "filter" | "char_filter") {
                return unsupported(format!(
                    "normalizer [{name}] has an unknown setting [{key}]"
                ));
            }
        }
        let char_filters = string_list(obj.get("char_filter")).ok_or_else(|| {
            NormalizerError::Unsupported(format!(
                "normalizer [{name}]: [char_filter] must be a string or a list of strings"
            ))
        })?;
        if let Some(cf) = char_filters.first() {
            return unsupported(format!(
                "normalizer [{name}] uses char_filter [{cf}]: char filters in normalizers are \
                 not supported by xerj (supported normalizer filters: {SUPPORTED})"
            ));
        }
        let filter_names = string_list(obj.get("filter")).ok_or_else(|| {
            NormalizerError::Unsupported(format!(
                "normalizer [{name}]: [filter] must be a string or a list of strings"
            ))
        })?;
        let mut filters = Vec::with_capacity(filter_names.len());
        for f in filter_names {
            filters.push(resolve_filter(name, &f, analysis)?);
        }
        Ok(Self {
            name: name.to_string(),
            filters,
        })
    }
}

fn string_list(v: Option<&Value>) -> Option<Vec<String>> {
    match v {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::String(s)) => Some(vec![s.clone()]),
        Some(Value::Array(arr)) => arr.iter().map(|e| e.as_str().map(str::to_string)).collect(),
        Some(_) => None,
    }
}

fn resolve_filter(
    normalizer: &str,
    filter: &str,
    analysis: Option<&Value>,
) -> Result<NormalizerFilter, NormalizerError> {
    let not_built = |kind: &str| {
        NormalizerError::Unsupported(format!(
            "filter [{filter}] (type [{kind}]) in normalizer [{normalizer}] is not supported by \
             xerj (supported normalizer filters: {SUPPORTED})"
        ))
    };
    let may_not_use = || {
        NormalizerError::Unsupported(format!(
            "Custom normalizer [{normalizer}] may not use filter [{filter}]"
        ))
    };
    if let Some(def) = analysis
        .and_then(|a| a.get("filter"))
        .and_then(|f| f.get(filter))
    {
        let kind = def.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(resolved) = NormalizerFilter::from_builtin_name(kind) else {
            return Err(if ES_NORMALIZER_FILTERS_NOT_BUILT.contains(&kind) {
                not_built(kind)
            } else {
                may_not_use()
            });
        };
        // Only the option sets this build reproduces exactly.
        let honoured = def.as_object().is_some_and(|o| {
            o.iter().all(|(k, v)| match k.as_str() {
                "type" => true,
                "preserve_original" => resolved == NormalizerFilter::Asciifolding && v == false,
                _ => false,
            })
        });
        if !honoured {
            return Err(NormalizerError::Unsupported(format!(
                "filter [{filter}] in normalizer [{normalizer}] sets options xerj does not \
                 apply; a normalizer filter may only set [type] (supported: {SUPPORTED})"
            )));
        }
        return Ok(resolved);
    }
    if let Some(builtin) = NormalizerFilter::from_builtin_name(filter) {
        return Ok(builtin);
    }
    if ES_NORMALIZER_FILTERS_NOT_BUILT.contains(&filter) {
        return Err(not_built(filter));
    }
    Err(may_not_use())
}

/// The `analysis` block of an index's settings, in any of the nested shapes
/// ES accepts: `{"settings": {...}}` or the inner settings object, with the
/// block at `analysis` or `index.analysis`.
pub fn analysis_block(settings: &Value) -> Option<&Value> {
    let root = settings.get("settings").unwrap_or(settings);
    root.get("analysis")
        .or_else(|| root.get("index").and_then(|i| i.get("analysis")))
        .filter(|a| a.is_object())
}

/// One message per declared normalizer this build cannot apply. Empty when
/// every normalizer under `analysis.normalizer` resolves.
pub fn unsupported_declared_normalizers(analysis: &Value) -> Vec<String> {
    let Some(map) = analysis.get("normalizer").and_then(Value::as_object) else {
        return Vec::new();
    };
    map.keys()
        .filter_map(
            |name| match KeywordNormalizer::resolve(name, Some(analysis)) {
                Ok(_) => None,
                Err(NormalizerError::Unsupported(m)) => Some(m),
                Err(NormalizerError::NotFound) => None,
            },
        )
        .collect()
}

/// Every normalized keyword field of a schema, by the path a query names it
/// with: a top-level field (`tag`), an object leaf (`meta.tag`), or a
/// multi-field (`title.raw`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeywordNormalizers {
    by_path: HashMap<String, KeywordNormalizer>,
}

impl KeywordNormalizers {
    /// Collect the normalizers a schema declares.
    pub fn from_schema(schema: &Schema) -> Self {
        fn walk(
            fields: &[FieldConfig],
            prefix: &str,
            out: &mut HashMap<String, KeywordNormalizer>,
        ) {
            for f in fields {
                let path = if prefix.is_empty() {
                    f.name.clone()
                } else {
                    format!("{prefix}.{}", f.name)
                };
                if let (FieldType::Keyword, Some(n)) = (f.field_type, &f.options.normalizer) {
                    out.insert(path.clone(), n.clone());
                }
                match f.field_type {
                    FieldType::Object | FieldType::Nested => walk(&f.fields, &path, out),
                    _ => {
                        for m in &f.fields {
                            if let (FieldType::Keyword, Some(n)) =
                                (m.field_type, &m.options.normalizer)
                            {
                                out.insert(format!("{path}.{}", m.name), n.clone());
                            }
                        }
                    }
                }
            }
        }
        let mut by_path = HashMap::new();
        walk(&schema.fields, "", &mut by_path);
        Self { by_path }
    }

    /// True when no field is normalized (the common case: every hook is then
    /// a no-op).
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// The normalizer of the field at `path`, if it has one.
    pub fn get(&self, path: &str) -> Option<&KeywordNormalizer> {
        self.by_path.get(path)
    }

    /// The normalized paths.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.by_path.keys().map(String::as_str)
    }

    /// `source` with each normalized TOP-LEVEL field's value normalized, for
    /// the structures that index top-level `_source` keys (doc-values
    /// columns). `None` when no top-level key of `source` is normalized, so
    /// the caller keeps the original without a copy.
    pub fn normalized_top_level(&self, source: &Value) -> Option<Value> {
        if self.by_path.is_empty() {
            return None;
        }
        let obj = source.as_object()?;
        if !obj.keys().any(|k| self.by_path.contains_key(k)) {
            return None;
        }
        let mut out = obj.clone();
        for (k, v) in out.iter_mut() {
            if let Some(n) = self.by_path.get(k) {
                *v = n.normalize_json(v);
            }
        }
        Some(Value::Object(out))
    }
}

/// Fold Latin letters to ASCII, keeping case. A letter whose canonical
/// decomposition is an ASCII letter plus combining marks folds to that letter
/// (`é`, `Ŕ`, `ạ`); a fixed list covers the Latin letters that have no such
/// decomposition (`Æ`, `Ø`, `Ł`, `ß`, `Þ`, ...). Everything else, including a
/// standalone combining mark and every non-Latin script, passes through.
fn fold_to_ascii(s: &str) -> String {
    use unicode_normalization::char::{decompose_canonical, is_combining_mark};
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        if let Some(mapped) = fold_special(c) {
            out.push_str(mapped);
            continue;
        }
        let mut parts: Vec<char> = Vec::with_capacity(4);
        decompose_canonical(c, |d| parts.push(d));
        match parts.split_first() {
            Some((base, marks))
                if base.is_ascii_alphabetic()
                    && !marks.is_empty()
                    && marks.iter().all(|m| is_combining_mark(*m)) =>
            {
                out.push(*base);
            }
            _ => out.push(c),
        }
    }
    out
}

fn fold_special(c: char) -> Option<&'static str> {
    Some(match c {
        'Æ' => "AE",
        'æ' => "ae",
        'Ð' | 'Đ' => "D",
        'ð' | 'đ' => "d",
        'Ø' => "O",
        'ø' => "o",
        'Þ' => "TH",
        'þ' => "th",
        'ß' => "ss",
        'ẞ' => "SS",
        'Ħ' => "H",
        'ħ' => "h",
        'ı' => "i",
        'Ĳ' => "IJ",
        'ĳ' => "ij",
        'ĸ' => "q",
        'Ŀ' | 'Ł' => "L",
        'ŀ' | 'ł' => "l",
        'ŉ' => "'n",
        'Ŋ' => "N",
        'ŋ' => "n",
        'Œ' => "OE",
        'œ' => "oe",
        'Ŧ' => "T",
        'ŧ' => "t",
        'ſ' => "s",
        'ƒ' => "f",
        'ﬀ' => "ff",
        'ﬁ' => "fi",
        'ﬂ' => "fl",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chain(filters: &[NormalizerFilter]) -> KeywordNormalizer {
        KeywordNormalizer {
            name: "t".into(),
            filters: filters.to_vec(),
        }
    }

    #[test]
    fn builtin_lowercase_resolves_without_settings() {
        let n = KeywordNormalizer::resolve("lowercase", None).unwrap();
        assert_eq!(n.normalize("FRUIT Salad"), "fruit salad");
        assert_eq!(
            KeywordNormalizer::resolve("nope", None),
            Err(NormalizerError::NotFound)
        );
    }

    #[test]
    fn asciifolding_keeps_case_and_folds_latin() {
        let n = chain(&[NormalizerFilter::Asciifolding]);
        assert_eq!(n.normalize("BÀR"), "BAR");
        assert_eq!(n.normalize("Ærøskøbing"), "AEroskobing");
        assert_eq!(n.normalize("straße"), "strasse");
        assert_eq!(n.normalize("Łódź"), "Lodz");
        assert_eq!(n.normalize("Tiếng Việt"), "Tieng Viet");
        // Non-Latin scripts and a standalone combining mark pass through.
        assert_eq!(n.normalize("Москва"), "Москва");
        assert_eq!(n.normalize("e\u{301}"), "e\u{301}");
    }

    #[test]
    fn custom_chain_runs_in_order() {
        let analysis = json!({
            "normalizer": {"n": {"type": "custom", "filter": ["trim", "asciifolding", "uppercase"]}}
        });
        let n = KeywordNormalizer::resolve("n", Some(&analysis)).unwrap();
        assert_eq!(n.normalize("  café crème "), "CAFE CREME");
        assert_eq!(
            n.normalize_json(&json!(["a", 1, null])),
            json!(["A", 1, null])
        );
    }

    #[test]
    fn declared_filters_resolve_by_type() {
        let analysis = json!({
            "filter": {"my_fold": {"type": "asciifolding", "preserve_original": false},
                       "my_lower": {"type": "lowercase"}},
            "normalizer": {"n": {"filter": ["my_fold", "my_lower"]}}
        });
        let n = KeywordNormalizer::resolve("n", Some(&analysis)).unwrap();
        assert_eq!(
            n.filters,
            vec![NormalizerFilter::Asciifolding, NormalizerFilter::Lowercase]
        );
    }

    #[test]
    fn unsupported_filters_are_refused_with_a_reason() {
        let cases = [
            (
                json!({"normalizer": {"n": {"filter": ["stemmer"]}}}),
                "may not use filter [stemmer]",
            ),
            (
                json!({"normalizer": {"n": {"filter": ["cjk_width"]}}}),
                "not supported by xerj",
            ),
            (
                json!({"normalizer": {"n": {"char_filter": ["q"], "filter": ["lowercase"]}}}),
                "char_filter [q]",
            ),
            (
                json!({"filter": {"l": {"type": "lowercase", "language": "greek"}},
                       "normalizer": {"n": {"filter": ["l"]}}}),
                "sets options",
            ),
            (
                json!({"filter": {"f": {"type": "asciifolding", "preserve_original": true}},
                       "normalizer": {"n": {"filter": ["f"]}}}),
                "sets options",
            ),
            (
                json!({"normalizer": {"n": {"type": "pattern"}}}),
                "Unknown normalizer type",
            ),
        ];
        for (analysis, want) in cases {
            match KeywordNormalizer::resolve("n", Some(&analysis)) {
                Err(NormalizerError::Unsupported(m)) => assert!(m.contains(want), "{m}"),
                other => panic!("{analysis}: {other:?}"),
            }
            assert_eq!(unsupported_declared_normalizers(&analysis).len(), 1);
        }
    }

    #[test]
    fn schema_paths_cover_leaves_objects_and_multi_fields() {
        let mut schema = Schema::empty();
        let mut tag = FieldConfig::new("tag", FieldType::Keyword);
        tag.options.normalizer = Some(KeywordNormalizer::builtin_lowercase());
        let mut title = FieldConfig::new("title", FieldType::Text);
        let mut raw = FieldConfig::new("raw", FieldType::Keyword);
        raw.options.normalizer = Some(KeywordNormalizer::builtin_lowercase());
        title.fields.push(raw);
        let mut meta = FieldConfig::new("meta", FieldType::Object);
        let mut owner = FieldConfig::new("owner", FieldType::Keyword);
        owner.options.normalizer = Some(KeywordNormalizer::builtin_lowercase());
        meta.fields.push(owner);
        meta.fields
            .push(FieldConfig::new("plain", FieldType::Keyword));
        for f in [tag, title, meta] {
            schema.add_field(f).unwrap();
        }
        let n = KeywordNormalizers::from_schema(&schema);
        let mut paths: Vec<&str> = n.paths().collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["meta.owner", "tag", "title.raw"]);

        let src = json!({"tag": ["A", "B"], "title": "T"});
        assert_eq!(
            n.normalized_top_level(&src),
            Some(json!({"tag": ["a", "b"], "title": "T"}))
        );
        assert_eq!(n.normalized_top_level(&json!({"title": "T"})), None);
    }
}
