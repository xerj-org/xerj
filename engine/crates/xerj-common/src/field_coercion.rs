//! Ingest-time coercion and type enforcement for numeric and boolean fields.
//!
//! THE single predicate for "what would Elasticsearch index for this value in
//! a field of this declared type?". Every write path that knows the typed
//! schema must call it and nothing else — the single-document `_doc`/`_create`
//! handlers in `xerj-api`, the per-item `_bulk` loop in `xerj-engine`, and the
//! `ignore_malformed` walker. When two ingest paths grew independent copies of
//! a validation rule before (the date predicate, `xerj_query::dates`), the
//! *strict* one ended up the laxer of the pair; sharing one function is what
//! stops that from happening again.
//!
//! ## What ES actually does (8.x, `coerce` defaults to `true`)
//!
//! For a numeric field, `NumberFieldMapper` parses the JSON token through
//! `XContentParser::intValue(coerce)` / `doubleValue(coerce)` and then range-
//! checks against the target width:
//!
//! | input into `integer` | ES 8.x                                    |
//! |----------------------|-------------------------------------------|
//! | `1`                  | `1`                                       |
//! | `1.9`                | `1` — truncated toward zero               |
//! | `"5"` / `"1.9"`      | `5` / `1` — string coerced then truncated |
//! | `9999999999`         | **400**, out of range for an integer      |
//! | `"abc"`              | **400**, cannot parse                     |
//! | `{"bad":"x"}`, `[…]` | **400**, object/array is not a number     |
//!
//! With `"coerce": false` on the field, a decimal part and a string are both
//! refused; the range check applies either way. `boolean` has no `coerce`
//! knob at all: `true`/`false` and the strings `"true"`/`"false"` are the
//! whole accepted set, and anything else — including `1`, `0` and `"yes"` —
//! is a 400.
//!
//! ## The XERJ divergence this closes (issue #781)
//!
//! XERJ stored whatever arrived, so an `integer` field holding `1.9` matched
//! `range {gte: 1.5}` and missed `term {i: 1}` — the same query over the same
//! declared-integer field returned different hits than ES. Coercing at ingest
//! is what makes the indexed value and the declared type agree.
//!
//! ## Rewriting `_source` moves the stored spelling — the query side pairs
//!
//! ES keeps `_source` byte-verbatim and coerces only the *indexed* value.
//! XERJ indexes from the stored source, so agreeing with ES on **hits** costs
//! source fidelity: ES returns `1.9` from `_source` where XERJ now returns
//! `1`, and a document written `{"b": "false"}` is stored `{"b": false}`.
//!
//! Two consequences, both handled — do not undo either half without the
//! other:
//!
//! 1. **A query operand written in the pre-coercion spelling must still
//!    match.** ES accepts `terms {b: ["false"]}` against a `boolean` field
//!    (`search/390_doc_values_search.yml` asserts exactly that), and once the
//!    stored value is a real `false` an exact-JSON comparison stops finding
//!    it. So `xerj_engine::index::rewrite_query_aliases` runs a `term`/`terms`
//!    value on a declared `boolean` through [`coerce_value`] — THIS predicate,
//!    on the read side — before any path sees it. The first cut of this module
//!    shipped without that and regressed the conformance suite from 0 failed
//!    to 1.
//! 2. **An index can hold BOTH representations.** Documents written before
//!    this change keep `"true"` / `"5"`; documents written after hold `true` /
//!    `5`; `_update` (deliberately not re-validated) can still merge the old
//!    spelling into a new document. Canonicalising the operand alone would
//!    then miss the older half, so `xerj_engine::index::json_scalar_equal`
//!    also relates a boolean to its string spelling in BOTH directions —
//!    for a scalar and for an element of a multi-valued field — alongside the
//!    number/string pair it already had. Terms aggregations already bucket the
//!    two spellings together. A reindex is still the way to make an index
//!    uniform, but nothing breaks without one.
//!
//! ## Known, deliberate narrowing
//!
//! * An empty string is left alone rather than dropped. ES treats `""` in a
//!   numeric field as "no value"; dropping it would rewrite `_source` for a
//!   case that is not a wrong-results bug.
//! * Only the field-level `"coerce": false` is honoured, not the index-level
//!   `index.mapping.coerce` setting.
//! * `scaled_float` is range-checked as a `double` and **not** quantised by
//!   its `scaling_factor`. ES indexes `Math.round(value * scaling_factor)`;
//!   reproducing that here would rewrite `_source` into the quantised value,
//!   which loses information the caller sent for no wrong-results gain.
//! * `_update` is not re-validated: it merges into a document that was already
//!   checked on write, matching how the existing date check scopes itself to
//!   `index` / `create`.
//! * Enforcement applies only to **declared** mappings. `index_mappings` is
//!   written by index-create-with-mappings and `PUT /_mapping` only, never by
//!   dynamic inference, so a schemaless index is untouched.
//!
//! Elasticsearch is referenced for semantics only. It is AGPL-3.0/SSPL-1.0/
//! Elastic-2.0 licensed and no code from it is reproduced here.

use serde_json::{Map, Number, Value};

/// What Elasticsearch would do with one value in a typed field.
#[derive(Debug, Clone, PartialEq)]
pub enum Coercion {
    /// Index the value exactly as it arrived.
    AsIs,
    /// ES indexes something else; this is the value to store instead.
    Rewrite(Value),
    /// ES refuses the whole document. Carries the `caused_by`-style detail.
    Reject(String),
}

/// The first field of a document that its declared type refuses.
#[derive(Debug, Clone, PartialEq)]
pub struct BadField {
    /// Full dotted path of the offending field.
    pub field: String,
    /// The declared type that refused it.
    pub ftype: String,
    /// Rendered preview of the offending value, ES-style.
    pub preview: String,
    /// Why it was refused (ES's `caused_by` sentence).
    pub detail: String,
}

impl BadField {
    /// The `reason` string ES puts on a `document_parsing_exception`.
    ///
    /// Same shape as the type-clash rejection `xerj_engine::bulk` already
    /// emits, so the two read identically on the wire.
    pub fn reason(&self, doc_id: &str) -> String {
        format!(
            "failed to parse field [{}] of type [{}] in document with id '{}'. \
             Preview of field's value: '{}'",
            self.field, self.ftype, doc_id, self.preview
        )
    }
}

/// Types this module enforces. Everything else (`text`, `keyword`, `date`,
/// `ip`, `geo_point`, ranges, vectors…) is left to its own validator.
pub fn is_enforced_type(ftype: &str) -> bool {
    matches!(
        ftype,
        "byte"
            | "short"
            | "integer"
            | "long"
            | "unsigned_long"
            | "half_float"
            | "float"
            | "double"
            | "scaled_float"
            | "boolean"
    )
}

/// Inclusive integer bounds of an integral ES type, in `i128` so that both
/// `i64::MIN` and `u64::MAX` are representable exactly — an `f64` bound would
/// round `i64::MAX` up and let `9223372036854775808` through as a `long`.
fn integral_bounds(ftype: &str) -> Option<(i128, i128)> {
    Some(match ftype {
        "byte" => (i8::MIN as i128, i8::MAX as i128),
        "short" => (i16::MIN as i128, i16::MAX as i128),
        "integer" => (i32::MIN as i128, i32::MAX as i128),
        "long" => (i64::MIN as i128, i64::MAX as i128),
        "unsigned_long" => (0, u64::MAX as i128),
        _ => return None,
    })
}

/// ES-style rendering of a value inside an error message: a string shows its
/// text unquoted, an object shows `{k=v, …}`, everything else its JSON form.
pub fn preview_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, val)| match val {
                    Value::String(s) => format!("{}={}", k, s),
                    other => format!("{}={}", k, other),
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        other => other.to_string(),
    }
}

fn out_of_range(ftype: &str, v: &Value) -> Coercion {
    Coercion::Reject(format!(
        "Value [{}] is out of range for a[n] {}",
        preview_value(v),
        ftype
    ))
}

fn not_finite(ftype: &str) -> Coercion {
    Coercion::Reject(format!("[{}] supports only finite values", ftype))
}

fn not_a_number(ftype: &str) -> Coercion {
    Coercion::Reject(format!(
        "Current token is not a numeric value, expected a[n] {}",
        ftype
    ))
}

/// The JSON number for an integral value the caller has already clamped into
/// its declared width's bounds, so it fits `i64` or `u64` exactly.
///
/// `as i64` alone would be wrong for `unsigned_long`, whose range runs past
/// `i64::MAX`: a saturating cast turns `1e19` into `i64::MAX` — a silently
/// different value, which is the class of bug this whole module exists to
/// close.
fn number_from_i128(i: i128) -> Value {
    if i > i64::MAX as i128 {
        Value::Number(Number::from(i as u64))
    } else {
        Value::Number(Number::from(i as i64))
    }
}

/// The exact integer part of the finite float `f`, and that value clamped into
/// a field's inclusive `lo..=hi` bounds.
///
/// Rust's float-to-int `as` saturates, so `f.trunc() as i128` is exact for
/// every value any of these widths can hold — no UB, no wraparound — and the
/// `clamp` then reproduces the Java narrowing cast ES applies after its own
/// (double-widened) range check. The caller compares the two: equal means the
/// float already IS the integer ES would index, different means ES's cast
/// changed it.
fn truncate_into(f: f64, lo: i128, hi: i128) -> (i128, i128) {
    let exact = f.trunc() as i128;
    (exact, exact.clamp(lo, hi))
}

/// Coerce one scalar (non-array) value for an integral field.
fn coerce_integral(ftype: &str, coerce: bool, v: &Value) -> Coercion {
    let (lo, hi) = match integral_bounds(ftype) {
        Some(b) => b,
        None => return Coercion::AsIs,
    };
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if (i as i128) < lo || (i as i128) > hi {
                    return out_of_range(ftype, v);
                }
                Coercion::AsIs
            } else if let Some(u) = n.as_u64() {
                if (u as i128) < lo || (u as i128) > hi {
                    return out_of_range(ftype, v);
                }
                Coercion::AsIs
            } else {
                // JSON float token: 1.9, 2.0, 1e40…
                let f = match n.as_f64() {
                    Some(f) if f.is_finite() => f,
                    _ => return not_finite(ftype),
                };
                if f < lo as f64 || f > hi as f64 {
                    return out_of_range(ftype, v);
                }
                // That range check is ES's own — and it is WIDER than the
                // declared width, because Java widens `Long.MAX_VALUE` to the
                // `double` 2^63 to make the comparison. So the float token
                // `9223372036854775808.0` survives it, and ES's `(long)` cast
                // then saturates the value to `Long.MAX_VALUE`. The
                // integer-token branch above is exact (`i128` bounds); this
                // branch has to reproduce BOTH halves of ES's behaviour, or a
                // bare `fract() == 0` shortcut stores a value the declared
                // width cannot hold — the residual leniency the `i128` bound
                // alone did not close.
                let (exact, clamped) = truncate_into(f, lo, hi);
                // Compare in the INTEGER domain. `clamped as f64 == f.trunc()`
                // would be fooled by the very rounding this guards against:
                // `i64::MAX as f64` is 2^63, so the two sides agree on exactly
                // the value that must not pass.
                if f.fract() == 0.0 && exact == clamped {
                    // `2.0` already equals the integer ES would index; leave
                    // `_source` alone rather than churn it to `2`.
                    return Coercion::AsIs;
                }
                if f.fract() != 0.0 && !coerce {
                    return Coercion::Reject(format!(
                        "Cannot coerce NUMBER to {} — value [{}] has a decimal part",
                        ftype, f
                    ));
                }
                Coercion::Rewrite(number_from_i128(clamped))
            }
        }
        Value::String(s) => {
            if s.trim().is_empty() {
                // "no value" — see the module note on empty strings.
                return Coercion::AsIs;
            }
            if !coerce {
                return Coercion::Reject(format!("Cannot coerce a string to a[n] {}", ftype));
            }
            let f: f64 = match s.trim().parse::<f64>() {
                Ok(f) if f.is_finite() => f,
                Ok(_) => return not_finite(ftype),
                Err(_) => return Coercion::Reject(format!("For input string: \"{}\"", s)),
            };
            if f < lo as f64 || f > hi as f64 {
                return out_of_range(ftype, v);
            }
            // ES runs a string through `Double.parseDouble` and truncates, so
            // `"1.9"` lands on 1 exactly as the bare number would. Prefer the
            // exact integer parse when it succeeds, so a `long` beyond f64's
            // 2^53 exact range keeps every digit — and range-check THAT parse
            // in `i128`, not the `f64` above: `"9223372036854775808"` rounds to
            // exactly `i64::MAX as f64` and so slips through the float bound,
            // while ES's `Numbers.toLong` throws on it.
            if let Ok(i) = s.trim().parse::<i128>() {
                if i < lo || i > hi {
                    return out_of_range(ftype, v);
                }
                return Coercion::Rewrite(number_from_i128(i));
            }
            Coercion::Rewrite(number_from_i128(truncate_into(f, lo, hi).1))
        }
        Value::Bool(_) | Value::Object(_) | Value::Array(_) => not_a_number(ftype),
        Value::Null => Coercion::AsIs,
    }
}

/// Coerce one scalar value for a floating-point field.
fn coerce_floating(ftype: &str, coerce: bool, v: &Value) -> Coercion {
    // ES range-checks a `float` by requiring the value to survive the cast to
    // 32-bit finitely, and a `half_float` by requiring it to survive the round
    // trip through 16-bit precision. 65504 is the largest half-float, but
    // round-to-nearest maps everything below 65520 back onto it — ES accepts
    // 65510 and rejects 65520 — so the boundary is 65520, not 65504. A plain
    // `f as f32` check (what the first cut used for both) accepts up to
    // 3.4e38 — 33 decimal orders of magnitude past where a `half_float`
    // actually overflows.
    const HALF_FLOAT_OVERFLOWS_AT: f64 = 65_520.0;
    let overflows = |f: f64| -> bool {
        match ftype {
            "float" => !(f as f32).is_finite(),
            "half_float" => f.abs() >= HALF_FLOAT_OVERFLOWS_AT,
            // `scaled_float` is deliberately NOT quantised by
            // `scaling_factor` here — see the module header.
            _ => false,
        }
    };
    let check = |f: f64, rewritten: bool| -> Coercion {
        if !f.is_finite() || overflows(f) {
            return not_finite(ftype);
        }
        if rewritten {
            match Number::from_f64(f) {
                Some(n) => Coercion::Rewrite(Value::Number(n)),
                None => not_finite(ftype),
            }
        } else {
            Coercion::AsIs
        }
    };
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) => check(f, false),
            None => not_finite(ftype),
        },
        Value::String(s) => {
            if s.trim().is_empty() {
                return Coercion::AsIs;
            }
            if !coerce {
                return Coercion::Reject(format!("Cannot coerce a string to a[n] {}", ftype));
            }
            match s.trim().parse::<f64>() {
                Ok(f) => check(f, true),
                Err(_) => Coercion::Reject(format!("For input string: \"{}\"", s)),
            }
        }
        Value::Bool(_) | Value::Object(_) | Value::Array(_) => not_a_number(ftype),
        Value::Null => Coercion::AsIs,
    }
}

/// Coerce one scalar value for a `boolean` field. ES has no `coerce` knob
/// here: `true`/`false` and their string spellings are the entire domain.
fn coerce_boolean(v: &Value) -> Coercion {
    match v {
        Value::Bool(_) | Value::Null => Coercion::AsIs,
        Value::String(s) if s.is_empty() => Coercion::AsIs,
        Value::String(s) => match s.as_str() {
            "true" => Coercion::Rewrite(Value::Bool(true)),
            "false" => Coercion::Rewrite(Value::Bool(false)),
            other => Coercion::Reject(format!(
                "Failed to parse value [{}] as only [true] or [false] are allowed.",
                other
            )),
        },
        other => Coercion::Reject(format!(
            "Failed to parse value [{}] as only [true] or [false] are allowed.",
            preview_value(other)
        )),
    }
}

/// What ES would index for `v` in a field declared `ftype`.
///
/// `coerce` is the field's effective `"coerce"` mapping parameter (ES default
/// `true`). Types this module does not own return [`Coercion::AsIs`].
pub fn coerce_value(ftype: &str, coerce: bool, v: &Value) -> Coercion {
    match ftype {
        "byte" | "short" | "integer" | "long" | "unsigned_long" => {
            coerce_integral(ftype, coerce, v)
        }
        "half_float" | "float" | "double" | "scaled_float" => coerce_floating(ftype, coerce, v),
        "boolean" => coerce_boolean(v),
        _ => Coercion::AsIs,
    }
}

/// Apply [`coerce_value`] to a whole field value, recursing into arrays.
///
/// Returns `Ok(Some(new))` when at least one element had to be rewritten,
/// `Ok(None)` when the value stands as sent, and `Err((detail, offender))` on
/// the first element ES would refuse.
fn coerce_field(ftype: &str, coerce: bool, v: &Value) -> Result<Option<Value>, (String, Value)> {
    match v {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            let mut changed = false;
            for el in arr {
                match coerce_field(ftype, coerce, el)? {
                    Some(new) => {
                        changed = true;
                        out.push(new);
                    }
                    None => out.push(el.clone()),
                }
            }
            Ok(changed.then_some(Value::Array(out)))
        }
        scalar => match coerce_value(ftype, coerce, scalar) {
            Coercion::AsIs => Ok(None),
            Coercion::Rewrite(new) => Ok(Some(new)),
            Coercion::Reject(detail) => Err((detail, scalar.clone())),
        },
    }
}

/// Walk `doc` against the mapping `properties`, coercing every numeric and
/// boolean field in place.
///
/// Returns `Ok(true)` when the document was modified, `Ok(false)` when it was
/// already ES-shaped, and `Err(BadField)` at the first value ES would refuse —
/// the caller turns that into a 400 `document_parsing_exception`.
///
/// Fields carrying `"ignore_malformed": true` are skipped outright: that
/// mapping asks for the bad value to be dropped into `_ignored`, which the
/// `ignore_malformed` walker has already done by the time this runs.
pub fn coerce_document(
    doc: &mut Map<String, Value>,
    props: &Map<String, Value>,
) -> Result<bool, BadField> {
    coerce_in(doc, props, "")
}

fn coerce_in(
    doc: &mut Map<String, Value>,
    props: &Map<String, Value>,
    prefix: &str,
) -> Result<bool, BadField> {
    let mut changed = false;
    let keys: Vec<String> = doc.keys().cloned().collect();
    for field in keys {
        let Some(spec) = props.get(&field) else {
            continue;
        };
        // `"enabled": false` — ES does not parse, index or validate ANYTHING
        // inside such an object; it is stored in `_source` and otherwise
        // ignored. The object recursion below keys off `properties`, and a
        // disabled object is still allowed to declare typed sub-properties, so
        // without this guard those would be enforced where ES ignores them.
        if spec.get("enabled").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let full = if prefix.is_empty() {
            field.clone()
        } else {
            format!("{}.{}", prefix, field)
        };
        let ftype = spec.get("type").and_then(Value::as_str).unwrap_or("");

        // Sub-objects: recurse. A mapping with `properties` but no declared
        // `type` is an implicit object in ES, so key off `properties` rather
        // than off the type name.
        if let Some(child_props) = spec.get("properties").and_then(Value::as_object) {
            if ftype.is_empty() || ftype == "object" || ftype == "nested" {
                let child_props = child_props.clone();
                match doc.get_mut(&field) {
                    Some(Value::Object(child)) => {
                        changed |= coerce_in(child, &child_props, &full)?;
                    }
                    Some(Value::Array(arr)) => {
                        for el in arr.iter_mut() {
                            if let Value::Object(child) = el {
                                changed |= coerce_in(child, &child_props, &full)?;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
        }

        if !is_enforced_type(ftype) {
            continue;
        }
        if spec
            .get("ignore_malformed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let coerce = spec.get("coerce").and_then(Value::as_bool).unwrap_or(true);

        let Some(value) = doc.get(&field) else {
            continue;
        };
        match coerce_field(ftype, coerce, value) {
            Ok(None) => {}
            Ok(Some(new)) => {
                doc.insert(field.clone(), new);
                changed = true;
            }
            Err((detail, bad)) => {
                return Err(BadField {
                    field: full,
                    ftype: ftype.to_string(),
                    preview: preview_value(&bad),
                    detail,
                });
            }
        }
    }
    Ok(changed)
}

/// Does this mapping declare ANY field whose type this module enforces?
///
/// The gate that keeps the `_bulk` turbo path free: an index with no numeric
/// or boolean field never needs its raw doc bytes parsed for coercion, so it
/// skips the whole check. Conservative on false positives (a user field
/// literally named `type` holding the string `"integer"` still returns true
/// and pays for one parse); never falsely negative.
pub fn mapping_has_enforced_types(mapping: &Value) -> bool {
    match mapping {
        Value::Object(m) => {
            if m.get("type")
                .and_then(Value::as_str)
                .map(is_enforced_type)
                .unwrap_or(false)
            {
                return true;
            }
            m.values().any(mapping_has_enforced_types)
        }
        Value::Array(arr) => arr.iter().any(mapping_has_enforced_types),
        _ => false,
    }
}

/// Pull the `properties` map out of a stored index mapping, which is held
/// either bare (`{"properties": …}`) or wrapped (`{"mappings": {"properties":
/// …}}`) depending on which API wrote it.
pub fn mapping_properties(mapping: &Value) -> Option<&Map<String, Value>> {
    mapping
        .get("properties")
        .or_else(|| mapping.get("mappings").and_then(|m| m.get("properties")))
        .and_then(Value::as_object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn coerced(ftype: &str, v: Value) -> Coercion {
        coerce_value(ftype, true, &v)
    }

    #[test]
    fn integer_truncates_a_fraction_the_way_es_does() {
        assert_eq!(coerced("integer", json!(1.9)), Coercion::Rewrite(json!(1)));
        assert_eq!(
            coerced("integer", json!(-1.9)),
            Coercion::Rewrite(json!(-1))
        );
        assert_eq!(coerced("long", json!(2.5)), Coercion::Rewrite(json!(2)));
        // Already integral: no `_source` churn.
        assert_eq!(coerced("integer", json!(2.0)), Coercion::AsIs);
        assert_eq!(coerced("integer", json!(7)), Coercion::AsIs);
    }

    #[test]
    fn numeric_strings_are_coerced_and_junk_strings_are_refused() {
        assert_eq!(coerced("integer", json!("5")), Coercion::Rewrite(json!(5)));
        assert_eq!(
            coerced("integer", json!("1.9")),
            Coercion::Rewrite(json!(1))
        );
        assert_eq!(
            coerced("double", json!("2.5")),
            Coercion::Rewrite(json!(2.5))
        );
        assert!(matches!(coerced("long", json!("abc")), Coercion::Reject(_)));
        assert!(matches!(
            coerced("double", json!("abc")),
            Coercion::Reject(_)
        ));
    }

    #[test]
    fn out_of_range_is_refused_on_every_width() {
        assert!(matches!(
            coerced("integer", json!(9999999999i64)),
            Coercion::Reject(_)
        ));
        assert!(matches!(coerced("byte", json!(200)), Coercion::Reject(_)));
        assert!(matches!(
            coerced("short", json!(40000)),
            Coercion::Reject(_)
        ));
        assert!(matches!(
            coerced("unsigned_long", json!(-1)),
            Coercion::Reject(_)
        ));
        // A u64 beyond i64::MAX must not pass as a `long`; an f64 bound would
        // have rounded i64::MAX up and let this through. The same value must
        // be refused in every SPELLING it can arrive in — bare integer token,
        // string, and float token (which ES's own check admits and its cast
        // then saturates).
        assert!(matches!(
            coerced("long", json!(9223372036854775808u64)),
            Coercion::Reject(_)
        ));
        assert!(matches!(
            coerced("long", json!("9223372036854775808")),
            Coercion::Reject(_)
        ));
        assert_eq!(
            coerced("long", json!("9223372036854775807")),
            Coercion::Rewrite(json!(9223372036854775807i64))
        );
        assert_eq!(
            coerced("long", json!(9223372036854775807i64)),
            Coercion::AsIs
        );
        assert_eq!(coerced("integer", json!(2147483647)), Coercion::AsIs);
    }

    #[test]
    fn an_unsigned_long_past_i64_max_truncates_without_saturating() {
        // 1e19 is inside `unsigned_long` and outside `i64`. This string form
        // reaches the truncating cast (neither `i64` nor `u64` parses it), and
        // a plain `as i64` there would have clamped it to i64::MAX — a
        // silently different value, exactly the class of bug this closes.
        let got = coerced("unsigned_long", json!("10000000000000000000.5"));
        match got {
            Coercion::Rewrite(Value::Number(n)) => {
                assert_eq!(n.as_u64(), Some(10_000_000_000_000_000_000));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            coerced("unsigned_long", json!(1.0e30f64)),
            Coercion::Reject(_)
        ));
    }

    #[test]
    fn objects_and_arrays_are_not_numbers() {
        assert!(matches!(
            coerced("integer", json!({"bad": "x"})),
            Coercion::Reject(_)
        ));
        assert!(matches!(
            coerced("integer", json!(true)),
            Coercion::Reject(_)
        ));
        assert!(matches!(
            coerced("boolean", json!({"a": 1})),
            Coercion::Reject(_)
        ));
    }

    #[test]
    fn coerce_false_refuses_what_coerce_true_would_fix() {
        assert!(matches!(
            coerce_value("integer", false, &json!(1.9)),
            Coercion::Reject(_)
        ));
        assert!(matches!(
            coerce_value("integer", false, &json!("5")),
            Coercion::Reject(_)
        ));
        // The range check is not a coercion and applies either way.
        assert!(matches!(
            coerce_value("integer", false, &json!(9999999999i64)),
            Coercion::Reject(_)
        ));
    }

    #[test]
    fn boolean_takes_true_false_and_their_spellings_only() {
        assert_eq!(coerced("boolean", json!(true)), Coercion::AsIs);
        assert_eq!(
            coerced("boolean", json!("true")),
            Coercion::Rewrite(json!(true))
        );
        assert_eq!(
            coerced("boolean", json!("false")),
            Coercion::Rewrite(json!(false))
        );
        assert!(matches!(
            coerced("boolean", json!("yes")),
            Coercion::Reject(_)
        ));
        assert!(matches!(coerced("boolean", json!(1)), Coercion::Reject(_)));
    }

    /// The `i128` bound closes the INTEGER token. The float token has a second
    /// hole: ES compares the raw double against a bound Java widened to
    /// `double`, so `Long.MAX_VALUE` becomes 2^63 and `9223372036854775808.0`
    /// passes the check — ES's `(long)` cast then saturates it. Storing the
    /// float verbatim (what a bare `fract() == 0` shortcut does) leaves a
    /// value no `long` can hold sitting in a `long` field.
    #[test]
    fn a_float_token_past_the_width_is_saturated_the_way_es_casts_it() {
        assert_eq!(
            coerced("long", json!(9223372036854775808.0f64)),
            Coercion::Rewrite(json!(9223372036854775807i64))
        );
        assert_eq!(
            coerced("integer", json!(-2147483649.0f64)),
            Coercion::Reject("Value [-2147483649.0] is out of range for a[n] integer".to_string())
        );
        // Inside the width, an integral float is still left alone.
        assert_eq!(
            coerced("long", json!(9223372036854775807.0f64)),
            Coercion::Rewrite(json!(9223372036854775807i64))
        );
        assert_eq!(coerced("long", json!(1234.0)), Coercion::AsIs);
    }

    /// ES round-trips a `half_float` through 16-bit precision and rejects what
    /// comes back infinite. 65504 is the largest half-float and
    /// round-to-nearest maps everything under 65520 onto it, so the boundary
    /// is 65520 — an `f32` finiteness check (which accepts 3.4e38) is ~2^128
    /// too wide.
    #[test]
    fn half_float_is_bounded_at_the_half_precision_overflow() {
        assert_eq!(coerced("half_float", json!(65504.0)), Coercion::AsIs);
        assert_eq!(coerced("half_float", json!(65510.0)), Coercion::AsIs);
        assert!(matches!(
            coerced("half_float", json!(65520.0)),
            Coercion::Reject(_)
        ));
        assert!(matches!(
            coerced("half_float", json!(1.0e38)),
            Coercion::Reject(_)
        ));
        // `float` keeps its own, wider bound.
        assert_eq!(coerced("float", json!(1.0e38)), Coercion::AsIs);
    }

    /// `"enabled": false` tells ES not to parse anything inside the object.
    /// The recursion keys off `properties`, which a disabled object may still
    /// declare, so it needs its own guard.
    #[test]
    fn a_disabled_object_is_not_walked() {
        let props = json!({
            "meta": {
                "enabled": false,
                "properties": {"n": {"type": "integer"}}
            }
        });
        let mut doc = json!({"meta": {"n": "not a number at all"}})
            .as_object()
            .unwrap()
            .clone();
        assert!(!coerce_document(&mut doc, props.as_object().unwrap())
            .expect("a disabled object must not be rejected"));
        assert_eq!(doc["meta"]["n"], json!("not a number at all"));
    }

    #[test]
    fn narrow_floats_refuse_values_that_overflow_them() {
        assert!(matches!(
            coerced("float", json!(1e300)),
            Coercion::Reject(_)
        ));
        assert_eq!(coerced("double", json!(1e300)), Coercion::AsIs);
        assert_eq!(coerced("float", json!(1.5)), Coercion::AsIs);
    }

    #[test]
    fn empty_string_and_null_are_left_alone() {
        assert_eq!(coerced("integer", json!("")), Coercion::AsIs);
        assert_eq!(coerced("integer", Value::Null), Coercion::AsIs);
        assert_eq!(coerced("boolean", json!("")), Coercion::AsIs);
        assert_eq!(coerced("boolean", Value::Null), Coercion::AsIs);
    }

    #[test]
    fn a_document_is_rewritten_in_place_and_nested_fields_are_reached() {
        let props = json!({
            "i": {"type": "integer"},
            "b": {"type": "boolean"},
            "inner": {"properties": {"n": {"type": "long"}}}
        });
        let mut doc = json!({"i": 1.9, "b": "true", "inner": {"n": "42"}})
            .as_object()
            .unwrap()
            .clone();
        let changed = coerce_document(&mut doc, props.as_object().unwrap()).expect("should coerce");
        assert!(changed);
        assert_eq!(doc.get("i"), Some(&json!(1)));
        assert_eq!(doc.get("b"), Some(&json!(true)));
        assert_eq!(doc["inner"]["n"], json!(42));
    }

    #[test]
    fn a_bad_nested_field_reports_its_dotted_path() {
        let props = json!({"inner": {"properties": {"n": {"type": "integer"}}}});
        let mut doc = json!({"inner": {"n": "abc"}}).as_object().unwrap().clone();
        let bad = coerce_document(&mut doc, props.as_object().unwrap()).unwrap_err();
        assert_eq!(bad.field, "inner.n");
        assert_eq!(bad.ftype, "integer");
        assert_eq!(bad.preview, "abc");
        assert!(bad
            .reason("7")
            .contains("failed to parse field [inner.n] of type [integer] in document with id '7'"));
    }

    #[test]
    fn arrays_are_coerced_element_wise() {
        let props = json!({"i": {"type": "integer"}});
        let mut doc = json!({"i": [1.9, "3", 4]}).as_object().unwrap().clone();
        assert!(coerce_document(&mut doc, props.as_object().unwrap()).expect("ok"));
        assert_eq!(doc.get("i"), Some(&json!([1, 3, 4])));

        let mut bad_doc = json!({"i": [1, "abc"]}).as_object().unwrap().clone();
        assert!(coerce_document(&mut bad_doc, props.as_object().unwrap()).is_err());
    }

    #[test]
    fn ignore_malformed_fields_are_left_to_their_own_walker() {
        let props = json!({"i": {"type": "integer", "ignore_malformed": true}});
        let mut doc = json!({"i": "abc"}).as_object().unwrap().clone();
        assert!(!coerce_document(&mut doc, props.as_object().unwrap()).expect("no rejection"));
        assert_eq!(doc.get("i"), Some(&json!("abc")));
    }

    #[test]
    fn unmapped_and_non_numeric_fields_are_untouched() {
        let props = json!({"k": {"type": "keyword"}, "t": {"type": "text"}});
        let mut doc = json!({"k": 5, "t": "hi", "unmapped": {"any": "shape"}})
            .as_object()
            .unwrap()
            .clone();
        assert!(!coerce_document(&mut doc, props.as_object().unwrap()).expect("ok"));
        assert_eq!(doc.get("k"), Some(&json!(5)));
    }

    #[test]
    fn the_turbo_gate_fires_only_on_a_numeric_or_boolean_mapping() {
        assert!(mapping_has_enforced_types(&json!({
            "properties": {"msg": {"type": "text"}, "n": {"type": "long"}}
        })));
        assert!(mapping_has_enforced_types(&json!({
            "properties": {"o": {"properties": {"b": {"type": "boolean"}}}}
        })));
        assert!(!mapping_has_enforced_types(&json!({
            "properties": {"msg": {"type": "text"}, "k": {"type": "keyword"}}
        })));
        assert!(!mapping_has_enforced_types(&json!({})));
    }

    #[test]
    fn mapping_properties_reads_both_stored_shapes() {
        let bare = json!({"properties": {"a": {"type": "integer"}}});
        let wrapped = json!({"mappings": {"properties": {"a": {"type": "integer"}}}});
        assert!(mapping_properties(&bare).is_some());
        assert!(mapping_properties(&wrapped).is_some());
        assert!(mapping_properties(&json!({"settings": {}})).is_none());
    }
}
