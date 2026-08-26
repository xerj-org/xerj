//! Sort field representation and comparison utilities.
//!
//! xerj supports the same sort syntax as Elasticsearch:
//!
//! ```json
//! "sort": [
//!   { "date": "desc" },
//!   { "price": { "order": "asc", "mode": "avg", "missing": "_last" } },
//!   "_score"
//! ]
//! ```
//!
//! A missing `sort` defaults to `_score` descending.

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// SortField
// ─────────────────────────────────────────────────────────────────────────────

/// A single sort criterion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortField {
    /// The field name, or `_score` / `_doc` for the built-in sort keys.
    pub field: String,
    /// Ascending or descending.
    pub order: SortOrder,
    /// How to pick a value when a document has multiple values for the field.
    pub mode: SortMode,
    /// What to do with documents that are missing the field.
    pub missing: SortMissing,
    /// Optional ES sort-value format override (e.g. `strict_date_optional_time_nanos`).
    /// When set, the emitted `sort` array uses the formatted string rather
    /// than the raw numeric epoch that the engine produces by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// ES `unmapped_type`: when set, sorting a field that has no mapping is not
    /// an error — every document is treated as missing (ES semantics). The
    /// unresolvable-field guard skips a field carrying this, so a client that
    /// asks for `unmapped_type` (e.g. a cross-index sort, or a query written to
    /// survive a schema that predates the field) is not rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmapped_type: Option<String>,
}

impl SortField {
    /// The default sort: `_score` descending.
    pub fn score_desc() -> Self {
        Self {
            field: "_score".to_string(),
            order: SortOrder::Desc,
            mode: SortMode::default(),
            missing: SortMissing::default(),
            format: None,
            unmapped_type: None,
        }
    }

    /// Physical document order — cheapest possible sort for scroll / scan.
    pub fn doc_asc() -> Self {
        Self {
            field: "_doc".to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::default(),
            format: None,
            unmapped_type: None,
        }
    }

    /// Returns `true` if this sort field is the relevance score.
    pub fn is_score(&self) -> bool {
        self.field == "_score"
    }

    /// Returns `true` if this sorts by internal document order.
    pub fn is_doc_order(&self) -> bool {
        self.field == "_doc"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SortOrder
// ─────────────────────────────────────────────────────────────────────────────

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    /// Smallest first (natural order for numbers, lexicographic for strings).
    Asc,
    /// Largest first.  Default for `_score`.
    #[default]
    Desc,
}

impl SortOrder {
    /// Flip the direction.
    pub fn reverse(self) -> Self {
        match self {
            SortOrder::Asc => SortOrder::Desc,
            SortOrder::Desc => SortOrder::Asc,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SortMode
// ─────────────────────────────────────────────────────────────────────────────

/// How to handle multi-valued fields in sort comparisons.
///
/// ES documentation: when a document has multiple values for the sort field,
/// the engine picks one representative value for the comparison according to
/// this mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    /// No explicit `mode` was requested. ES then picks the `min` value for an
    /// ascending sort and the `max` value for a descending one — see
    /// `effective(order)`. Kept distinct from an explicit `min`/`max` so that
    /// `{"order":"desc"}` (no mode) resolves to `max`, not the enum default.
    #[default]
    Default,
    /// Use the minimum value across all field values.
    Min,
    /// Use the maximum value across all field values.
    Max,
    /// Use the arithmetic mean.
    Avg,
    /// Use the sum (only meaningful for numeric fields).
    Sum,
    /// Use the median.
    Median,
}

impl SortMode {
    /// Resolve the effective reduction for a multi-valued field, applying ES's
    /// order-based default (`asc` -> min, `desc` -> max) when no explicit mode
    /// was given. An explicit mode is honoured regardless of order.
    pub fn effective(self, order: SortOrder) -> SortMode {
        match self {
            SortMode::Default => match order {
                SortOrder::Asc => SortMode::Min,
                SortOrder::Desc => SortMode::Max,
            },
            other => other,
        }
    }
}

/// Reduce a (possibly multi-valued) sort field value to the single
/// representative value ES would compare, per the sort `mode` and `order`.
///
/// A non-array value is returned unchanged. `null` array entries are ignored
/// (ES skips them when selecting the representative); an array that is empty or
/// all-null reduces to `Null`, so the request's `missing` policy governs it.
/// `min`/`max` use the same numbers-then-strings ordering as the sort
/// comparator; `avg`/`sum`/`median` are numeric-only (non-numeric entries are
/// ignored, matching ES, which rejects them at mapping time).
pub fn reduce_sort_value(
    raw: &serde_json::Value,
    order: SortOrder,
    mode: SortMode,
) -> serde_json::Value {
    use serde_json::Value;
    let arr = match raw {
        Value::Array(a) => a,
        other => return other.clone(),
    };
    let vals: Vec<&Value> = arr.iter().filter(|v| !v.is_null()).collect();
    if vals.is_empty() {
        return Value::Null;
    }
    match mode.effective(order) {
        SortMode::Min => vals
            .iter()
            .copied()
            .min_by(|a, b| compare_values(a, b, &SortMissing::Last))
            .cloned()
            .unwrap_or(Value::Null),
        SortMode::Max => vals
            .iter()
            .copied()
            .max_by(|a, b| compare_values(a, b, &SortMissing::Last))
            .cloned()
            .unwrap_or(Value::Null),
        SortMode::Avg | SortMode::Sum | SortMode::Median => {
            let mut nums: Vec<f64> = vals.iter().filter_map(|v| v.as_f64()).collect();
            if nums.is_empty() {
                return Value::Null;
            }
            let out = match mode.effective(order) {
                SortMode::Sum => nums.iter().sum(),
                SortMode::Avg => nums.iter().sum::<f64>() / nums.len() as f64,
                SortMode::Median => {
                    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let m = nums.len() / 2;
                    if nums.len() % 2 == 1 {
                        nums[m]
                    } else {
                        (nums[m - 1] + nums[m]) / 2.0
                    }
                }
                _ => unreachable!(),
            };
            serde_json::Number::from_f64(out)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        }
        // `effective` never returns `Default`.
        SortMode::Default => Value::Null,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SortMissing
// ─────────────────────────────────────────────────────────────────────────────

/// Where to place documents that are missing the sort field.
///
/// Hand-rolled serialization: the default `#[serde(untagged)]` treatment
/// collapses the two unit variants (`Last` / `First`) to `null` on the
/// wire, which made the query cache hash the same bytes for
/// `missing:"_last"` and `missing:"_first"`. That caused
/// `search/630_format_sort_missing_dates.yml` to serve a stale
/// missing-Last result to a subsequent missing-First request.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SortMissing {
    /// Documents missing the field sort last (default).
    #[default]
    Last,
    /// Documents missing the field sort first.
    First,
    /// Use a concrete substitute value for comparison.
    Value(serde_json::Value),
}

impl Serialize for SortMissing {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            SortMissing::Last => s.serialize_str("_last"),
            SortMissing::First => s.serialize_str("_first"),
            SortMissing::Value(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for SortMissing {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        match v {
            serde_json::Value::String(s) if s == "_last" => Ok(SortMissing::Last),
            serde_json::Value::String(s) if s == "_first" => Ok(SortMissing::First),
            other => Ok(SortMissing::Value(other)),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sort key extraction helpers (used by executor)
// ─────────────────────────────────────────────────────────────────────────────

/// Compare two sort-key arrays lexicographically, respecting each field's
/// `SortOrder`.
///
/// Used by the executor's top-K heap to determine document ranking.
///
/// Returns `std::cmp::Ordering` — the caller decides whether to keep or
/// discard the candidate document based on its heap position.
pub fn compare_sort_keys(
    a: &[serde_json::Value],
    b: &[serde_json::Value],
    fields: &[SortField],
) -> std::cmp::Ordering {
    for (i, field) in fields.iter().enumerate() {
        let av = a.get(i).unwrap_or(&serde_json::Value::Null);
        let bv = b.get(i).unwrap_or(&serde_json::Value::Null);
        // ES `missing: _last` / `_first` is direction-stable: missing
        // values stay at the end (or start) regardless of asc/desc on
        // non-null values. Compute the directional comparison only on
        // non-null pairs and let the missing-policy short-circuit drive
        // null placement directly.
        let cmp = match (av, bv) {
            (serde_json::Value::Null, serde_json::Value::Null) => std::cmp::Ordering::Equal,
            (serde_json::Value::Null, _) => match field.missing {
                SortMissing::First => std::cmp::Ordering::Less,
                SortMissing::Last => std::cmp::Ordering::Greater,
                SortMissing::Value(_) => {
                    let raw = compare_values(av, bv, &field.missing);
                    if field.order == SortOrder::Desc {
                        raw.reverse()
                    } else {
                        raw
                    }
                }
            },
            (_, serde_json::Value::Null) => match field.missing {
                SortMissing::First => std::cmp::Ordering::Greater,
                SortMissing::Last => std::cmp::Ordering::Less,
                SortMissing::Value(_) => {
                    let raw = compare_values(av, bv, &field.missing);
                    if field.order == SortOrder::Desc {
                        raw.reverse()
                    } else {
                        raw
                    }
                }
            },
            (_, _) => {
                let raw = compare_values(av, bv, &field.missing);
                if field.order == SortOrder::Desc {
                    raw.reverse()
                } else {
                    raw
                }
            }
        };
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }
    std::cmp::Ordering::Equal
}

/// Compare two JSON values for sort purposes.
///
/// Null / missing values are handled per `missing` policy; non-null values
/// follow a consistent total order: numbers < strings < everything else.
fn compare_values(
    a: &serde_json::Value,
    b: &serde_json::Value,
    missing: &SortMissing,
) -> std::cmp::Ordering {
    use serde_json::Value::*;
    use std::cmp::Ordering::*;

    match (a, b) {
        (Null, Null) => Equal,
        (Null, _) => match missing {
            SortMissing::First => Less,
            SortMissing::Last => Greater,
            SortMissing::Value(v) => compare_values(v, b, &SortMissing::Last),
        },
        (_, Null) => match missing {
            SortMissing::First => Greater,
            SortMissing::Last => Less,
            SortMissing::Value(v) => compare_values(a, v, &SortMissing::Last),
        },
        (Number(an), Number(bn)) => {
            let af = an.as_f64().unwrap_or(f64::NAN);
            let bf = bn.as_f64().unwrap_or(f64::NAN);
            af.partial_cmp(&bf).unwrap_or(Equal)
        }
        (String(as_), String(bs)) => as_.cmp(bs),
        (Bool(ab), Bool(bb)) => ab.cmp(bb),
        // Fallback: stringify and compare lexicographically
        _ => a.to_string().cmp(&b.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_compare_numbers_asc() {
        let fields = vec![SortField {
            field: "score".to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::Last,
            format: None,
            unmapped_type: None,
        }];
        let a = vec![json!(1)];
        let b = vec![json!(2)];
        assert_eq!(compare_sort_keys(&a, &b, &fields), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_compare_numbers_desc() {
        let fields = vec![SortField {
            field: "score".to_string(),
            order: SortOrder::Desc,
            mode: SortMode::default(),
            missing: SortMissing::Last,
            format: None,
            unmapped_type: None,
        }];
        let a = vec![json!(2)];
        let b = vec![json!(1)];
        assert_eq!(compare_sort_keys(&a, &b, &fields), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_missing_last() {
        let fields = vec![SortField {
            field: "x".to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::Last,
            format: None,
            unmapped_type: None,
        }];
        // null sorts after non-null
        assert_eq!(
            compare_sort_keys(&[json!(null)], &[json!(1)], &fields),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_missing_first() {
        let fields = vec![SortField {
            field: "x".to_string(),
            order: SortOrder::Asc,
            mode: SortMode::default(),
            missing: SortMissing::First,
            format: None,
            unmapped_type: None,
        }];
        assert_eq!(
            compare_sort_keys(&[json!(null)], &[json!(1)], &fields),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn test_multifield_tiebreak() {
        let fields = vec![
            SortField {
                field: "date".to_string(),
                order: SortOrder::Desc,
                mode: SortMode::default(),
                missing: SortMissing::Last,
                format: None,
                unmapped_type: None,
            },
            SortField {
                field: "name".to_string(),
                order: SortOrder::Asc,
                mode: SortMode::default(),
                missing: SortMissing::Last,
                format: None,
                unmapped_type: None,
            },
        ];
        // Same date, "alice" < "bob"
        let a = vec![json!("2024-01-01"), json!("alice")];
        let b = vec![json!("2024-01-01"), json!("bob")];
        assert_eq!(compare_sort_keys(&a, &b, &fields), std::cmp::Ordering::Less);
    }
}
