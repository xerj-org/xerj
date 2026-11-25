//! The ES `dynamic` mapping setting: what it means for a document field the
//! mapping does not declare (#3514).
//!
//! | setting           | an undeclared field                                   |
//! |-------------------|-------------------------------------------------------|
//! | `true` (default)  | is mapped from its first value, searchable            |
//! | `false`           | stays in `_source` only: unmapped, not searchable     |
//! | `strict`          | rejects the document, 400 `strict_dynamic_mapping_exception` |
//! | `runtime`         | becomes a runtime field (queryable); XERJ maps it as `true` |
//!
//! The setting is inherited: an object without its own `dynamic` uses its
//! parent's, and the root's default is `true`. A `null` value or an empty
//! array/object is still a "dynamic introduction" under `strict`.
//!
//! The root setting drives the engine's [`DynamicMapping`]; the strict walk
//! below is shared by the single-document handlers in `xerj-api` and the
//! per-item `_bulk` loop in `xerj-engine`, so the two write paths give the
//! same answer for the same document.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::schema::DynamicMapping;

/// One object's `dynamic` setting, as written in a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicSetting {
    /// `true`: map new fields.
    True,
    /// `false`: keep new fields in `_source` only.
    False,
    /// `strict`: refuse documents that carry new fields.
    Strict,
    /// `runtime`: new fields become runtime fields.
    Runtime,
}

impl DynamicSetting {
    /// Parse a mapping's `dynamic` value: a JSON bool or one of the strings
    /// ES accepts. `None` for anything else.
    pub fn parse(v: &Value) -> Option<Self> {
        match v {
            Value::Bool(true) => Some(Self::True),
            Value::Bool(false) => Some(Self::False),
            Value::String(s) => match s.as_str() {
                "true" => Some(Self::True),
                "false" => Some(Self::False),
                "strict" => Some(Self::Strict),
                "runtime" => Some(Self::Runtime),
                _ => None,
            },
            _ => None,
        }
    }

    /// The engine mode for a root carrying this setting. `runtime` keeps
    /// mapping new fields: they are queryable in ES too, and XERJ has no
    /// runtime-field shadow mapping to put them in instead.
    pub fn engine_mode(self) -> DynamicMapping {
        match self {
            Self::True | Self::Runtime => DynamicMapping::Dynamic,
            Self::False => DynamicMapping::Runtime,
            Self::Strict => DynamicMapping::Strict,
        }
    }
}

/// The mapping body inside either blob shape (`{"properties": ..}` from
/// `PUT /_mapping`, `{"mappings": {..}}` from index create).
fn mapping_root(mapping: &Value) -> &Value {
    if mapping.get("properties").is_some() || mapping.get("dynamic").is_some() {
        return mapping;
    }
    mapping.get("mappings").unwrap_or(mapping)
}

/// The root `dynamic` setting of a mapping blob (either shape), `None` when
/// it is not set.
pub fn root_dynamic(mapping: &Value) -> Option<DynamicSetting> {
    mapping_root(mapping)
        .get("dynamic")
        .and_then(DynamicSetting::parse)
}

/// Names an ES mapping keeps queryable even though the engine schema may not
/// map them, consulted under a root `dynamic: false` before a query or agg
/// clause on an unmapped field is treated as matching nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MappingCoverage {
    /// The path and everything under it: a declared leaf (its multi-fields,
    /// a `flattened` key), a runtime field, an object that re-opens dynamic
    /// mapping (`dynamic: true` / `runtime`) under the `false` root.
    prefixes: Vec<String>,
    /// The path only: a declared object, whose undeclared children stay
    /// unmapped.
    exact: HashSet<String>,
}

impl MappingCoverage {
    /// True when the mapping declares `field`, or something it lives under
    /// that keeps it queryable.
    pub fn covers(&self, field: &str) -> bool {
        self.exact.contains(field)
            || self.prefixes.iter().any(|p| {
                field
                    .strip_prefix(p.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            })
    }

    /// What a mapping blob (either shape) declares.
    pub fn of(mapping: &Value) -> Self {
        let root = mapping_root(mapping);
        let mut out = Self::default();
        if let Some(rt) = root.get("runtime").and_then(Value::as_object) {
            out.prefixes.extend(rt.keys().cloned());
        }
        let dynamic = root_dynamic(mapping).unwrap_or(DynamicSetting::True);
        if let Some(props) = root.get("properties").and_then(Value::as_object) {
            coverage(props, "", dynamic, &mut out);
        }
        out
    }
}

fn coverage(
    props: &Map<String, Value>,
    base: &str,
    dynamic: DynamicSetting,
    out: &mut MappingCoverage,
) {
    for (name, spec) in props {
        let path = join(base, name);
        let Some(spec) = spec.as_object() else {
            continue;
        };
        if !is_open_container(spec) {
            out.prefixes.push(path);
            continue;
        }
        let own = spec
            .get("dynamic")
            .and_then(DynamicSetting::parse)
            .unwrap_or(dynamic);
        if matches!(own, DynamicSetting::True | DynamicSetting::Runtime) {
            out.prefixes.push(path);
            continue;
        }
        if let Some(children) = spec.get("properties").and_then(Value::as_object) {
            coverage(children, &path, own, out);
        }
        out.exact.insert(path);
    }
}

/// A field a `strict` object refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictViolation {
    /// The new field's name, relative to `within`.
    pub field: String,
    /// Full path of the strict object, `_doc` for the root.
    pub within: String,
}

impl StrictViolation {
    /// The reason sentence ES sends.
    pub fn reason(&self) -> String {
        format!(
            "mapping set to strict, dynamic introduction of [{}] within [{}] is not allowed",
            self.field, self.within
        )
    }
}

/// ES error `type` for a [`StrictViolation`].
pub const STRICT_DYNAMIC_MAPPING_EXCEPTION: &str = "strict_dynamic_mapping_exception";

/// Declared paths of a mapping, flattened (`obj.inner.leaf`), plus every
/// proper prefix of a dotted property name (`subobjects: false` declares
/// `my.keyword.field` under `my`, so `my.keyword` is a path, not a new field).
struct Declared<'a> {
    specs: HashMap<String, &'a Map<String, Value>>,
    prefixes: HashSet<String>,
}

fn join(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}.{name}")
    }
}

fn collect<'a>(props: &'a Map<String, Value>, base: &str, out: &mut Declared<'a>) {
    for (name, spec) in props {
        let path = join(base, name);
        let mut cut = name.as_str();
        while let Some((head, _)) = cut.rsplit_once('.') {
            out.prefixes.insert(join(base, head));
            cut = head;
        }
        let Some(spec) = spec.as_object() else {
            continue;
        };
        if let Some(children) = spec.get("properties").and_then(Value::as_object) {
            collect(children, &path, out);
        }
        out.specs.insert(path, spec);
    }
}

/// Whether a declared spec is an object whose children the walk descends
/// into: `object` / `nested`, or no `type` with `properties`. A disabled
/// object (`enabled: false`) is not: ES does not parse inside it.
fn is_open_container(spec: &Map<String, Value>) -> bool {
    if spec.get("enabled").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    match spec.get("type").and_then(Value::as_str) {
        Some("object") | Some("nested") => true,
        Some(_) => false,
        None => true,
    }
}

/// True when the mapping declares `strict` anywhere (root or any object).
/// The per-document walk is skipped when it is false, so indices that never
/// say `strict` pay nothing.
pub fn mapping_has_strict(mapping: &Value) -> bool {
    fn any_strict(v: &Value) -> bool {
        match v {
            Value::Object(o) => {
                o.get("dynamic").and_then(DynamicSetting::parse) == Some(DynamicSetting::Strict)
                    || o.get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|p| p.values().any(any_strict))
            }
            _ => false,
        }
    }
    any_strict(mapping_root(mapping))
}

/// The first field of `doc` that a `strict` object in `mapping` refuses, or
/// `None` when the document parses. Root keys starting with `_` are skipped:
/// metadata fields are refused (or injected, like `_ignored`) elsewhere.
pub fn first_strict_violation(
    doc: &Map<String, Value>,
    mapping: &Value,
) -> Option<StrictViolation> {
    if !mapping_has_strict(mapping) {
        return None;
    }
    let root = mapping_root(mapping);
    let empty = Map::new();
    let props = root
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut declared = Declared {
        specs: HashMap::new(),
        prefixes: HashSet::new(),
    };
    collect(props, "", &mut declared);
    let dynamic = root_dynamic(mapping).unwrap_or(DynamicSetting::True);
    walk(doc, "", "_doc", dynamic, &declared, true)
}

fn walk(
    obj: &Map<String, Value>,
    base: &str,
    within: &str,
    dynamic: DynamicSetting,
    declared: &Declared<'_>,
    at_root: bool,
) -> Option<StrictViolation> {
    for (key, value) in obj {
        if at_root && key.starts_with('_') {
            continue;
        }
        if let Some(v) = check_key(key, value, base, within, dynamic, declared) {
            return Some(v);
        }
    }
    None
}

fn check_key(
    key: &str,
    value: &Value,
    base: &str,
    within: &str,
    dynamic: DynamicSetting,
    declared: &Declared<'_>,
) -> Option<StrictViolation> {
    let mut path = base.to_string();
    let mut within = within.to_string();
    let mut dynamic = dynamic;
    for seg in key.split('.') {
        path = join(&path, seg);
        match declared.specs.get(&path) {
            Some(spec) if is_open_container(spec) => {
                within = path.clone();
                if let Some(d) = spec.get("dynamic").and_then(DynamicSetting::parse) {
                    dynamic = d;
                }
            }
            // A leaf (its multi-fields are not document keys), a disabled
            // object, `flattened`: nothing below it is a dynamic field.
            Some(_) => return None,
            None if declared.prefixes.contains(&path) => {}
            None => {
                return (dynamic == DynamicSetting::Strict).then(|| StrictViolation {
                    field: seg.to_string(),
                    within,
                });
            }
        }
    }
    descend(value, &path, &within, dynamic, declared)
}

fn descend(
    value: &Value,
    path: &str,
    within: &str,
    dynamic: DynamicSetting,
    declared: &Declared<'_>,
) -> Option<StrictViolation> {
    match value {
        Value::Object(o) => walk(o, path, within, dynamic, declared, false),
        Value::Array(items) => items
            .iter()
            .find_map(|item| descend(item, path, within, dynamic, declared)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn violation(mapping: Value, doc: Value) -> Option<String> {
        first_strict_violation(doc.as_object().unwrap(), &mapping).map(|v| v.reason())
    }

    #[test]
    fn root_strict_refuses_a_new_field_and_a_null_one() {
        let m = json!({"mappings": {"dynamic": "strict", "properties": {"a": {"type": "long"}}}});
        assert_eq!(violation(m.clone(), json!({"a": 1})), None);
        assert_eq!(
            violation(m.clone(), json!({"a": 1, "z": 2})).as_deref(),
            Some("mapping set to strict, dynamic introduction of [z] within [_doc] is not allowed")
        );
        assert!(violation(m.clone(), json!({"z": null})).is_some());
        assert!(violation(m, json!({"z": []})).is_some());
    }

    #[test]
    fn object_strict_names_the_object_path() {
        let m = json!({"properties": {"o": {"dynamic": "strict", "properties": {"in": {"properties": {"x": {"type": "long"}}}}}}});
        assert_eq!(violation(m.clone(), json!({"o": {"in": {"x": 1}}})), None);
        assert_eq!(
            violation(m.clone(), json!({"o": {"in": {"y": 1}}})).as_deref(),
            Some("mapping set to strict, dynamic introduction of [y] within [o.in] is not allowed")
        );
        assert!(violation(m.clone(), json!({"o.in.y": 1})).is_some());
        assert!(violation(m.clone(), json!({"o": [{"in": {"x": 1}}, {"k": 1}]})).is_some());
        assert_eq!(violation(m, json!({"other": 1})), None);
    }

    #[test]
    fn a_child_can_reopen_a_strict_parent() {
        let m = json!({"dynamic": "strict", "properties": {"open": {"type": "object", "dynamic": true}, "off": {"dynamic": false}}});
        assert_eq!(violation(m.clone(), json!({"open": {"anything": 1}})), None);
        assert_eq!(violation(m.clone(), json!({"off": {"anything": 1}})), None);
        assert!(violation(m, json!({"new": 1})).is_some());
    }

    #[test]
    fn leaves_disabled_objects_and_dotted_declarations_are_not_new_fields() {
        let m = json!({"dynamic": "strict", "properties": {
            "t": {"type": "text", "fields": {"raw": {"type": "keyword"}}},
            "off": {"type": "object", "enabled": false},
            "my": {"subobjects": false, "properties": {"k.f": {"type": "keyword"}}}
        }});
        assert_eq!(
            violation(m.clone(), json!({"t": "x", "off": {"q": 1}})),
            None
        );
        assert_eq!(violation(m.clone(), json!({"my": {"k": {"f": "v"}}})), None);
        assert_eq!(violation(m.clone(), json!({"my.k.f": "v"})), None);
        assert!(violation(m, json!({"my": {"k": {"g": "v"}}})).is_some());
    }

    #[test]
    fn non_strict_mappings_never_refuse() {
        for d in [json!(true), json!(false), json!("runtime")] {
            let m = json!({"dynamic": d, "properties": {}});
            assert_eq!(violation(m, json!({"z": 1})), None);
        }
    }

    #[test]
    fn coverage_keeps_declared_leaves_runtime_fields_and_reopened_objects() {
        let c = MappingCoverage::of(&json!({"mappings": {
            "dynamic": false,
            "runtime": {"rt": {"type": "long"}},
            "properties": {
                "title": {"type": "text"},
                "o2": {"properties": {"a": {"type": "long"}}},
                "open": {"type": "object", "dynamic": true},
                "labels": {"type": "flattened"}
            }
        }}));
        for f in [
            "title",
            "title.raw",
            "rt",
            "o2",
            "o2.a",
            "open",
            "open.x.y",
            "labels.k",
        ] {
            assert!(c.covers(f), "{f}");
        }
        for f in ["r", "obj.r", "o2.b", "titles", "openx"] {
            assert!(!c.covers(f), "{f}");
        }
    }

    #[test]
    fn root_setting_maps_to_the_engine_mode() {
        let mode = |m: Value| root_dynamic(&m).map(DynamicSetting::engine_mode);
        assert_eq!(
            mode(json!({"mappings": {"dynamic": false}})),
            Some(DynamicMapping::Runtime)
        );
        assert_eq!(
            mode(json!({"dynamic": "false"})),
            Some(DynamicMapping::Runtime)
        );
        assert_eq!(
            mode(json!({"dynamic": "strict"})),
            Some(DynamicMapping::Strict)
        );
        assert_eq!(
            mode(json!({"dynamic": "runtime"})),
            Some(DynamicMapping::Dynamic)
        );
        assert_eq!(mode(json!({"properties": {}})), None);
    }
}
