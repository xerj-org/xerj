//! Mapping-resolved query fields and semantic-capable index discovery.

use serde_json::Value;

/// These exact fields and these exact (flat) weights were measured, not
/// chosen. Swept 7 variants over 6 ground-truth queries on a 324-file Rust
/// corpus (`body, defs, title` flat: top3 6/6 — the winner; boosting `defs`
/// favours test modules; `title^2 body`: top3 2/6). Full table in
/// tools/xerj-code/SKILL.md; provenance: measure/SERVER_UPLIFT_SCORECARD.md.
///
/// Two fields are deliberately absent:
/// * `symbols.name` — `symbols` is an array of objects with no searchable
///   `.name` subpath; including it makes the whole multi_match return ZERO
///   hits with no error at all (it took an Aho-Corasick query from 0 hits to
///   3 correct files just to remove it).
/// * `"*"` — a bare wildcard flattens every score to the same value
///   (measured: every hit scored exactly 2.0).
///
/// `defs_expanded^0.5` is a low-weight RECALL field (per-symbol signatures +
/// identifier sub-words) boosted BELOW 1.0 on purpose. It is NOT safe to send
/// unconditionally — see [`resolve_fields`].
pub const FIELDS: &[&str] = &["body", "defs", "title", "defs_expanded^0.5"];

/// Drop query fields that no index under the prefix actually maps.
///
/// This engine does NOT ignore an unmapped field in `multi_match` the way ES
/// does — including one silently collapses a MULTI-TOKEN query to ZERO hits.
/// Measured 2026-08-06, one index, exact totals (`relation: eq`):
///
/// ```text
/// query "log merge policy segment size buckets"
/// fields=["body"]                                 -> 673 hits
/// fields=["body","defs"]                          -> 673 hits
/// fields=["body","defs","title"]                  -> 673 hits
/// fields=["body","defs","title","defs_expanded"]  ->   0 hits
/// ```
///
/// `defs_expanded` was mapped in exactly 0 of the corpus's 219 indices, so
/// every multi-word query returned "no passage matches" — the silent-zero
/// failure shape. Full reproducer: measure/MULTIMATCH_DEFECT.md.
///
/// `mapping` is `None` when the mapping could not be read (transport hiccup,
/// auth): send the list UNCHANGED rather than silently narrowing it. The
/// floor is `["body"]` — never an empty field list.
pub fn resolve_fields(mapping: Option<&Value>) -> Vec<String> {
    let Some(mapping) = mapping else {
        return FIELDS.iter().map(|s| s.to_string()).collect();
    };
    let Some(obj) = mapping.as_object() else {
        return FIELDS.iter().map(|s| s.to_string()).collect();
    };
    let mut present = std::collections::BTreeSet::new();
    for m in obj.values() {
        if let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) {
            for key in props.keys() {
                present.insert(key.clone());
            }
        }
    }
    // A field any index maps as numeric cannot ride the wildcard
    // `multi_match`: the engine rejects the whole query with
    // `For input string: "<query>"` (#1284). One long `title` among 132
    // indices is enough. Keyword stays — that is the measured code-corpus
    // list, and a keyword is not this rejection.
    let numeric = numeric_fields(obj);
    let mut out: Vec<String> = FIELDS
        .iter()
        .filter(|f| {
            let base = f.split('^').next().unwrap_or(f);
            present.contains(base) && !numeric.contains(base)
        })
        .map(|s| s.to_string())
        .collect();
    // The plain-text extraction family (`.txt` mirrors, logs) puts record
    // content in `text`, not `body` — every other family (code, markdown,
    // adoc) uses `body`. Mapping-gated so the unreadable-mapping fallback
    // above never grows a field an index may not map (the MULTIMATCH_DEFECT
    // silent-zero). Found via the zalando G7 1/5: all 25 renamed `.txt`
    // chapters were invisible to `xerj code` while README.md matched.
    //
    // #1238: `text` joins BELOW 1.0, the same recall-leg posture as
    // `defs_expanded^0.5`. Measured on the live exploit group (5,644
    // indices, query "MCPJam inspector 23744", 36 needle PoC repos): at
    // full weight one plain-text sibling-CVE demo index outscored every
    // code-family hit and took the whole top-10; at ^0.5 the family stays
    // searchable (same 39 total hits) while the needle docs keep their
    // code-family scores.
    if present.contains("text") && !numeric.contains("text") {
        out.push("text^0.5".to_string());
    }
    // #1244 coverage gate: an own field joins the ^0.5 union only when at
    // least a quarter of the indices in the mapping union map it. Measured
    // on the two live corpora that motivated this, the coverage spectrum
    // has an empty middle: cve-records keeps its schema prose at 37-96%
    // (containers_cna_descriptions 95.8%, timeline 37.3% of 118 indices),
    // while exploit-pocs-2026 — where every own field is README frontmatter
    // junk from some vendored file — tops out at 1.0% (`description`) once
    // the standard fields are accounted for. Before the gate, the sorted
    // 24-cap kept literal `$comment` and cut `Summary` there: 2,143 own
    // text fields, all noise. `div_ceil` so a 5-index union demands 2.
    //
    // The union route alone assumes the corpus is ONE family. A multi-
    // family corpus breaks it: vuln-fix-commits (measured 2026-10-08) is
    // project-kb repo source (body/defs/title) PLUS a minority of payload
    // indices whose prose lives in `message` — 4 of 26 union indices,
    // under the quarter gate's 7, so the corpus answered from its tooling
    // files while every payload was unsearchable (the needle is rank 1 at
    // 36.35 queried on `message` directly). An exact-signature family
    // grouping was tried first and is too brittle: the mapper types only
    // fields present in a shard's documents, so the SAME logical family
    // splits into `message,patch,text` and `files,message,text` shards
    // and each half fails a majority rule. The family route below
    // therefore admits a field when ANY index that maps none of the
    // standard content fields maps it — an own-only index's content is
    // unreachable any other way (the floor does not fire while `body` is
    // mapped somewhere, and the #1158 `text` slot is synthesized EMPTY
    // for exactly the records that carry schema-named prose).
    //
    // Junk protection shifts from admission to priority: README
    // frontmatter junk lives in body-mapping indices and never enters
    // this route at all, while own-only junk (dataset column names) can
    // enter — but the cap below ranks legs by how many indices map them,
    // so a 1-index column name sorts behind every widely-mapped field.
    // That also ends the alphabetical-cap failure the #1244 gate was
    // partly compensating for ($comment kept, Summary cut).
    let min_union_indices = obj.len().div_ceil(4);
    let own_gated = own_recall_legs(obj, min_union_indices, &numeric);
    if out.is_empty() {
        // #1158: a raw-JSON corpus (ghsa-db's advisory mirrors, OSV) maps
        // NONE of the standard content fields — every record carries its
        // text in schema-named fields like `summary`/`details`. The old
        // `["body"]` floor here is a field no index maps, and an unmapped
        // multi_match field collapses a multi-token query to ZERO hits
        // with no error: the entire corpus answered "no passage matches"
        // while the same indices returned 10,000+ hits queried directly.
        // Fall back to the corpus's OWN text-typed fields (never `ax_*`
        // provenance) — still mapping-gated, so every field sent is one
        // at least one index really maps.
        //
        // The floor takes ANY own text field, coverage gate OFF: the only
        // alternative is `["body"]`, a field no index maps. A sparse
        // raw-JSON corpus whose shards each map different prose fields is
        // better served by its 10%-coverage fields than by a guaranteed-
        // wrong `body`.
        let mut own_any = own_text_fields(obj, 1, &numeric);
        own_any.truncate(MAX_OWN_FIELDS);
        if own_any.is_empty() {
            // `body` is the historical floor for a mapping that names no
            // text field. It is not a floor when some index maps `body` as
            // numeric: sending it is the #1284 rejection, not a silent zero.
            if numeric.contains("body") {
                Vec::new()
            } else {
                vec!["body".to_string()]
            }
        } else {
            own_any
        }
    } else {
        // #1244: the corpus's own text-typed fields join as ^0.5 recall
        // legs even when standard fields ARE mapped. Found by the
        // pre-registered g7-cve-records-2026-10-08 suite (0/7): the
        // records' prose lived in `containers_cna_descriptions` (rank 1
        // at 18.76 queried directly), but the wildcard sent only
        // `text^0.5` — the synthesized `text` was mapped by ONE shard
        // and EMPTY in every record with schema-named prose, and its
        // presence kept this branch from ever consulting the own-field
        // list. The union is safe against score ballooning because the
        // BM25 body rides `multi_match`'s default `best_fields`, which
        // this engine executes as dis_max (max, not sum — see the
        // `BestFields` arm in `query_node_to_fts`): a ^0.5 leg can only
        // win when it genuinely outranks the content fields at half
        // weight, the same posture `text^0.5` got from #1238.
        for f in own_gated {
            if !out.iter().any(|o| o.split('^').next() == Some(f.as_str())) {
                out.push(format!("{f}^0.5"));
            }
        }
        out
    }
}

/// Cap on how many own fields may join the `multi_match` as `^0.5` recall
/// legs (see [`own_text_fields`]). Measured against the exploit-pocs-2026
/// corpus that motivated the gate: 2,143 own text fields would otherwise
/// ride the query.
const MAX_OWN_FIELDS: usize = 24;

/// Types the engine stores as `long` or `double` (`es_type_to_field_type` in
/// `xerj-engine`). A `multi_match` naming one of these fails the index with
/// `For input string: "<query>"` (#1284), and a wildcard fails every index
/// with it.
fn is_numeric_mapping_type(t: &str) -> bool {
    matches!(
        t,
        "long"
            | "integer"
            | "short"
            | "byte"
            | "unsigned_long"
            | "double"
            | "float"
            | "half_float"
            | "scaled_float"
    )
}

/// Field names any index in the union maps as numeric.
fn numeric_fields(obj: &serde_json::Map<String, Value>) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for m in obj.values() {
        let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) else {
            continue;
        };
        for (key, spec) in props {
            if spec
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(is_numeric_mapping_type)
            {
                out.insert(key.clone());
            }
        }
    }
    out
}

/// The corpus's own searchable content fields: every property mapped `text`
/// (or `semantic_text`) by at least `min_indices` of the indices in the
/// union, `ax_*` provenance excluded, names sorted, uncapped (callers cap —
/// see [`MAX_OWN_FIELDS`]). Callers pick the gate: the #1158 floor passes
/// `1` (any mapping index — the alternative is a guaranteed-wrong `body`),
/// the #1244 union passes a quarter of the union (see the coverage note in
/// [`resolve_fields`]).
fn own_text_fields(
    obj: &serde_json::Map<String, Value>,
    min_indices: usize,
    numeric: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    const AX: &str = "ax_";
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for m in obj.values() {
        let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) else {
            continue;
        };
        for (key, spec) in props {
            let searchable = spec
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "text" || t == "semantic_text");
            // #1237: `impl` is the symbol-record implementation field queried
            // by its own dedicated BM25 leg under `--fuse-symbols`; never fold
            // it into the file-level `multi_match` over `body`/`defs`/`title`.
            if searchable && !key.starts_with(AX) && key != "impl" && !numeric.contains(key) {
                *counts.entry(key.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= min_indices)
        .map(|(k, _)| k)
        .collect()
}

/// The #1244 union + family admission for own text fields, as one ranked
/// list. A field is admitted when EITHER:
///
/// * at least `min_union_indices` of the union's indices map it (the
///   quarter gate — schema prose that repeats across a single-family
///   corpus, measured 37-96% on cve-records), OR
/// * ANY index that maps none of the standard content fields maps it (the
///   family route — a minority family in a multi-family corpus, measured
///   `message` at 4/26 on vuln-fix-commits).
///
/// The list is capped at [`MAX_OWN_FIELDS`] entries ordered by how many
/// indices map each field (ties alphabetical), so coverage decides which
/// legs ride the query — not the field's name. Junk that slips through the
/// family route (dataset column names in own-only indices) carries the
/// lowest counts and sorts behind every widely-mapped field; the
/// alphabetical-cap failure this replaces kept a literal `$comment` and
/// cut `Summary` on exploit-pocs-2026.
fn own_recall_legs(
    obj: &serde_json::Map<String, Value>,
    min_union_indices: usize,
    numeric: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    const STANDARD: [&str; 4] = ["body", "defs", "title", "defs_expanded"];
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut family: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in obj.values() {
        let Some(props) = m.pointer("/mappings/properties").and_then(Value::as_object) else {
            continue;
        };
        let own_text: Vec<&String> = props
            .iter()
            .filter(|(key, spec)| {
                !key.starts_with("ax_")
                    && *key != "impl"
                    && !numeric.contains(*key)
                    && spec
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t == "text" || t == "semantic_text")
            })
            .map(|(key, _)| key)
            .collect();
        // An index that maps none of the standard content fields carries
        // its content ONLY in own fields — everything it maps joins the
        // family route.
        if !own_text.iter().any(|f| STANDARD.contains(&f.as_str())) {
            for f in &own_text {
                family.insert((*f).clone());
            }
        }
        for f in &own_text {
            *counts.entry((*f).clone()).or_insert(0) += 1;
        }
    }
    let mut admitted: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(f, n)| *n >= min_union_indices || family.contains(f))
        .collect();
    admitted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    admitted
        .into_iter()
        .take(MAX_OWN_FIELDS)
        .map(|(f, _)| f)
        .collect()
}

/// Whether at least one index under the prefix maps the symbol-record `impl`
/// field (#1237) as searchable text (`text` or `semantic_text`).
pub fn has_symbol_impl(mapping: Option<&Value>) -> bool {
    let Some(mapping) = mapping.and_then(Value::as_object) else {
        return false;
    };
    mapping.values().any(|m| {
        m.pointer("/mappings/properties/impl/type")
            .and_then(Value::as_str)
            .is_some_and(|t| t == "text" || t == "semantic_text")
    })
}

/// Indices under the prefix whose `field` (default `body`) is mapped as
/// `semantic_text`.
///
/// A `semantic` query against an index where the field is plain `text` does
/// not return fewer hits — it fails the WHOLE search with a 400, taking every
/// other index in the wildcard down with it. So the capable set is discovered
/// from the mapping and the vector arm is aimed ONLY at those indices.
///
/// Returns `(capable, total)`, names sorted. An unreadable/unparseable
/// mapping yields an empty capable set (degrade to BM25), never a guess.
pub fn semantic_capable(mapping: Option<&Value>) -> (Vec<String>, usize) {
    let Some(mapping) = mapping.and_then(Value::as_object) else {
        return (Vec::new(), 0);
    };
    let mut capable: Vec<String> = mapping
        .iter()
        .filter(|(_, m)| {
            m.pointer("/mappings/properties/body/type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "semantic_text")
        })
        .map(|(name, _)| name.clone())
        .collect();
    capable.sort();
    (capable, mapping.len())
}

/// One semantic field per index, then indices grouped by that field.
///
/// Preference is `body`, then `text`, then the alphabetically first
/// `semantic_text` property. Code indices often elect `code` (or prose
/// elects `text`) while a document dataset elects `body`; a query that only
/// names `body` never reaches the others, and posting `body` at an index
/// that maps it as plain `text` 400s the whole request.
///
/// Groups are ordered `body`, `text`, then the remaining field names.
/// Index names inside a group are sorted. An unreadable mapping is empty.
pub fn semantic_groups(mapping: Option<&Value>) -> (Vec<(String, Vec<String>)>, usize) {
    let Some(mapping) = mapping.and_then(Value::as_object) else {
        return (Vec::new(), 0);
    };
    let mut by_field: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (name, body) in mapping {
        let Some(chosen) = elected_semantic_field(body) else {
            continue;
        };
        by_field.entry(chosen).or_default().push(name.clone());
    }
    let mut groups: Vec<(String, Vec<String>)> = by_field.into_iter().collect();
    for (_, indices) in &mut groups {
        indices.sort();
    }
    groups.sort_by(|left, right| {
        semantic_field_rank(&left.0)
            .cmp(&semantic_field_rank(&right.0))
            .then_with(|| left.0.cmp(&right.0))
    });
    (groups, mapping.len())
}

fn semantic_field_rank(field: &str) -> u8 {
    match field {
        "body" => 0,
        "text" => 1,
        _ => 2,
    }
}

fn elected_semantic_field(mapping_body: &Value) -> Option<String> {
    let props = mapping_body
        .pointer("/mappings/properties")
        .and_then(Value::as_object)?;
    let mut semantic: Vec<&str> = props
        .iter()
        .filter(|(_, spec)| {
            spec.get("type")
                .and_then(Value::as_str)
                .is_some_and(|ty| ty == "semantic_text")
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if semantic.is_empty() {
        return None;
    }
    semantic.sort_unstable();
    let chosen = ["body", "text"]
        .into_iter()
        .find(|preferred| semantic.contains(preferred))
        .unwrap_or(semantic[0]);
    Some(chosen.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mapping(indices: &[(&str, &[&str], bool)]) -> Value {
        let mut obj = serde_json::Map::new();
        for (name, fields, semantic_body) in indices {
            let mut props = serde_json::Map::new();
            for f in fields.iter() {
                props.insert(
                    f.to_string(),
                    json!({ "type": if *semantic_body && *f == "body" { "semantic_text" } else { "text" } }),
                );
            }
            obj.insert(
                name.to_string(),
                json!({ "mappings": { "properties": props } }),
            );
        }
        Value::Object(obj)
    }

    #[test]
    fn unmapped_fields_are_dropped_and_the_floor_is_body() {
        // The MULTIMATCH_DEFECT guard: `defs_expanded` unmapped in every
        // index must be dropped, not sent.
        let m = mapping(&[
            ("xc-kv-b1-000", &["body", "defs", "title"], false),
            ("xc-kv-b1-001", &["body", "defs"], false),
        ]);
        assert_eq!(
            resolve_fields(Some(&m)),
            vec!["body".to_string(), "defs".into(), "title".into()]
        );

        // No FIELDS member and NOTHING text-typed at all -> the ["body"]
        // floor, never empty, never a keyword field. (A text-typed field
        // no standard name covers is the #1158 own-fields case below.)
        let thin = json!({ "i": { "mappings": { "properties": {
            "unrelated": { "type": "keyword" }
        } } } });
        assert_eq!(resolve_fields(Some(&thin)), vec!["body".to_string()]);
    }

    #[test]
    fn an_unreadable_mapping_sends_the_full_list() {
        assert_eq!(
            resolve_fields(None),
            FIELDS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            resolve_fields(Some(&json!("junk"))),
            FIELDS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
    }

    /// #1139: the `.txt` family's content field is `text` — it joins the
    /// multi_match list ONLY when an index actually maps it, so the
    /// unreadable-mapping fallback list above stays exactly FIELDS.
    /// #1238: it joins at ^0.5 (recall leg), never at full weight.
    #[test]
    fn text_family_content_joins_only_when_mapped() {
        let with_text = mapping(&[("i", &["body", "text"], false)]);
        assert_eq!(
            resolve_fields(Some(&with_text)),
            vec!["body".to_string(), "text^0.5".to_string()]
        );
        // No FIELDS member AND no text-typed field -> the ["body"] floor
        // still holds (keyword placeholders — text-typed ones are #1158's
        // own-fields fallback, covered by its own test).
        let thin = json!({ "i": { "mappings": { "properties": {
            "unrelated": { "type": "keyword" }
        } } } });
        assert_eq!(resolve_fields(Some(&thin)), vec!["body".to_string()]);
    }

    #[test]
    fn capable_discovery_finds_only_semantic_text_indices() {
        let m = mapping(&[
            ("plain-1", &["body", "defs"], false),
            ("sem-1", &["body", "defs"], true),
            ("sem-2", &["body"], true),
        ]);
        let (capable, total) = semantic_capable(Some(&m));
        assert_eq!(capable, vec!["sem-1".to_string(), "sem-2".to_string()]);
        assert_eq!(total, 3);
        assert!(semantic_capable(None) == (Vec::new(), 0));
    }

    /// A code index that elects `code` (not `body`) and a prose index that
    /// elects `text` must both be queried, on their own field. `body` wins
    /// when an index maps both.
    #[test]
    fn semantic_groups_follow_the_field_each_index_actually_embeds() {
        let m = json!({
            "manifest": { "mappings": { "properties": {
                "body": { "type": "semantic_text" },
                "text": { "type": "text" }
            }}},
            "code": { "mappings": { "properties": {
                "body": { "type": "text" },
                "code": { "type": "semantic_text" }
            }}},
            "prose": { "mappings": { "properties": {
                "text": { "type": "semantic_text" },
                "title": { "type": "text" }
            }}},
            "both": { "mappings": { "properties": {
                "body": { "type": "semantic_text" },
                "text": { "type": "semantic_text" }
            }}},
            "plain": { "mappings": { "properties": {
                "body": { "type": "text" }
            }}}
        });
        let (groups, total) = semantic_groups(Some(&m));
        assert_eq!(total, 5);
        assert_eq!(
            groups,
            vec![
                (
                    "body".to_string(),
                    vec!["both".to_string(), "manifest".to_string()]
                ),
                ("text".to_string(), vec!["prose".to_string()]),
                ("code".to_string(), vec!["code".to_string()]),
            ]
        );
        assert!(semantic_groups(None).0.is_empty());
    }

    /// #1158: a raw-JSON corpus (ghsa-db advisories) maps none of the
    /// standard content fields — the old `["body"]` floor was a field no
    /// index maps, and an unmapped multi_match field collapses a
    /// multi-token query to zero hits. The corpus's own text-typed fields
    /// must take the floor's place, `ax_*` provenance excluded.
    #[test]
    fn a_raw_json_corpus_falls_back_to_its_own_text_fields() {
        let mut obj = serde_json::Map::new();
        let props = json!({
            "id":        { "type": "keyword" },
            "summary":   { "type": "text" },
            "details":   { "type": "text" },
            "severity":  { "type": "keyword" },
            "ax_path":   { "type": "text" },
            "ax_file":   { "type": "text" },
            "modified":  { "type": "date" }
        });
        obj.insert(
            "xc-ghsa-000".to_string(),
            json!({ "mappings": { "properties": props } }),
        );
        let m = Value::Object(obj);
        assert_eq!(
            resolve_fields(Some(&m)),
            vec!["details".to_string(), "summary".to_string()],
            "own text fields, sorted; ax_*/keyword/date excluded"
        );
    }

    /// The floor is unchanged when the corpus maps nothing searchable at
    /// all (only keywords/provenance): `["body"]`, never an empty list.
    #[test]
    fn a_corpus_with_no_text_typed_fields_keeps_the_body_floor() {
        let mut obj = serde_json::Map::new();
        let props = json!({
            "id":     { "type": "keyword" },
            "count":  { "type": "long" }
        });
        obj.insert(
            "xc-kv-000".to_string(),
            json!({ "mappings": { "properties": props } }),
        );
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec!["body".to_string()]
        );
    }

    /// A mixed corpus where SOME index maps `body` — #1244 reversed the
    /// old pin (this test used to assert `["body"]` alone, "the fallback
    /// fires only on the no-standard-field floor"): the pre-registered
    /// g7-cve-records-2026-10-08 suite measured that rule losing the whole
    /// corpus (0/7), because one shard's synthesized — and empty-in-real-
    /// records — `text` made the field list non-empty and the own-field
    /// legs never joined. Now the sibling's `summary` rides beside `body`
    /// as a ^0.5 recall leg (best_fields = dis_max, so it can only win by
    /// genuinely outranking the content field at half weight), and `body`
    /// keeps its full weight and its position.
    #[test]
    fn own_text_fields_join_as_recall_legs_beside_body() {
        let m = mapping(&[("xc-mixed-000", &["body"], false)]);
        let mut obj = m.as_object().unwrap().clone();
        obj.insert(
            "xc-mixed-001".to_string(),
            json!({ "mappings": { "properties": {
                "id": { "type": "keyword" },
                "summary": { "type": "text" }
            } } }),
        );
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec!["body".to_string(), "summary^0.5".to_string()],
            "body keeps full weight; summary joins as the ^0.5 recall leg"
        );
    }

    /// The #1244 shape exactly: a wildcard where ONE shard maps the
    /// synthesized `text` (empty in every prose-carrying record) and the
    /// rest keep their prose in schema-named text fields. Before the fix
    /// this resolved to `["text^0.5"]` and the corpus answered noise —
    /// the g7-cve-records 0/7. The own fields must join even though
    /// `text` made the list non-empty, and `text^0.5` itself must not be
    /// duplicated when it is also an own text field.
    #[test]
    fn the_cve_records_shape_joins_prose_fields_beside_synthesized_text() {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "xc-cve-records-0".to_string(),
            json!({ "mappings": { "properties": {
                "cveMetadata_cveId": { "type": "keyword" },
                "containers_cna_descriptions": { "type": "text" },
                "containers_cna_title": { "type": "text" }
            } } }),
        );
        obj.insert(
            "xc-cve-records-1".to_string(),
            json!({ "mappings": { "properties": {
                "cveMetadata_cveId": { "type": "keyword" },
                "text": { "type": "text" }
            } } }),
        );
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec![
                "text^0.5".to_string(),
                "containers_cna_descriptions^0.5".to_string(),
                "containers_cna_title^0.5".to_string(),
            ],
            "own prose fields join beside the synthesized text; text^0.5 not duplicated"
        );
    }

    /// The #1244 coverage gate on the UNION path, measured on the live
    /// corpora: schema prose repeats across shards (cve-records 37-96%),
    /// README frontmatter junk does not (exploit-pocs-2026: 2,143 own text
    /// fields, all <=1% once the standard fields are accounted for — the
    /// alphabetical 24-cap kept a literal `$comment` and cut `Summary`).
    /// Five indices, quarter gate demands 2: `summary` at 2/5 joins,
    /// `$comment` at 1/5 stays out. The FLOOR (no standard field mapped
    /// anywhere) is exempt — its only alternative is a guaranteed-wrong
    /// `body`, so even a 1/5 field beats it there.
    #[test]
    fn union_legs_need_quarter_coverage_but_the_floor_takes_any() {
        let mut obj = serde_json::Map::new();
        for i in 0..5 {
            let mut props = serde_json::Map::new();
            props.insert("body".to_string(), json!({ "type": "text" }));
            if i < 2 {
                props.insert("summary".to_string(), json!({ "type": "text" }));
            }
            if i == 0 {
                props.insert("$comment".to_string(), json!({ "type": "text" }));
            }
            obj.insert(
                format!("xc-frontmatter-{i:03}"),
                json!({ "mappings": { "properties": props } }),
            );
        }
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec!["body".to_string(), "summary^0.5".to_string()],
            "summary (2/5) joins as the recall leg; $comment (1/5) fails the quarter gate"
        );

        // Same five indices with `body` nowhere: the floor fires and takes
        // BOTH own fields, coverage be damned — `["body"]` would be a field
        // no index maps.
        let mut bare = serde_json::Map::new();
        for i in 0..5 {
            let mut props = serde_json::Map::new();
            if i < 2 {
                props.insert("summary".to_string(), json!({ "type": "text" }));
            }
            if i == 0 {
                props.insert("$comment".to_string(), json!({ "type": "text" }));
            }
            if !props.is_empty() {
                bare.insert(
                    format!("xc-bare-{i:03}"),
                    json!({ "mappings": { "properties": props } }),
                );
            }
        }
        let mut got = resolve_fields(Some(&Value::Object(bare)));
        got.sort();
        assert_eq!(
            got,
            vec!["$comment".to_string(), "summary".to_string()],
            "the #1158 floor is exempt from the coverage gate"
        );
    }

    /// The #1244 family route, measured on vuln-fix-commits: repo-source
    /// indices (body/defs/title) beside a MINORITY of payload indices whose
    /// prose lives in `message` — 4 of 26 live indices, under the union
    /// quarter gate, so the corpus answered from its tooling files while
    /// every payload was unsearchable. The payload indices share their
    /// mapping signature, so `message` joins by family majority. `text`
    /// (synthesized, empty in payload records) was already on the list via
    /// the #1238 route and must not be duplicated.
    #[test]
    fn a_minority_family_joins_by_signature_not_union_share() {
        let mut obj = serde_json::Map::new();
        // The code-family majority: 10 repo-source indices.
        for i in 0..10 {
            obj.insert(
                format!("xc-vfc-repo-{i:03}"),
                json!({ "mappings": { "properties": {
                    "body":  { "type": "text" },
                    "defs":  { "type": "text" },
                    "title": { "type": "text" }
                } } }),
            );
        }
        // The payload family: 2 indices, identical signature, no standard
        // field. Union = 12, quarter gate demands 3: `message` at 2 fails.
        for i in 0..2 {
            obj.insert(
                format!("xc-vfc-payloads-{i:03}"),
                json!({ "mappings": { "properties": {
                    "cve":     { "type": "keyword" },
                    "message": { "type": "text" },
                    "patch":   { "type": "semantic_text" },
                    "text":    { "type": "text" }
                } } }),
            );
        }
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec![
                "body".to_string(),
                "defs".to_string(),
                "title".to_string(),
                "text^0.5".to_string(),
                "message^0.5".to_string(),
                "patch^0.5".to_string(),
            ],
            "payload-family prose joins beside the code-family standard fields"
        );
    }

    /// The family route's boundary: a field in an index that maps `body`
    /// — the README-frontmatter shape from exploit-pocs-2026 — never
    /// enters the family route, however many indices carry it (it would
    /// have to pass the union gate instead). But a lone own-only index
    /// inside a code corpus DOES contribute its fields: its content is
    /// unreachable any other way, and the coverage-ranked cap (next
    /// test) keeps its 1-index fields behind every widely-mapped leg.
    #[test]
    fn body_indices_never_enter_the_family_route_but_a_lone_own_index_does() {
        let mut obj = serde_json::Map::new();
        // Nine plain code indices give the union a quarter gate of 3.
        for i in 0..9 {
            obj.insert(
                format!("xc-code-{i:03}"),
                json!({ "mappings": { "properties": {
                    "body": { "type": "text" }
                } } }),
            );
        }
        // Two body-mapping indices sharing a frontmatter field: body
        // family, not an own-only family — `frontmatter` must stay out of
        // the family route AND sits at 2/12, under the union gate.
        for i in 0..2 {
            obj.insert(
                format!("xc-README-{i:03}"),
                json!({ "mappings": { "properties": {
                    "body":        { "type": "text" },
                    "frontmatter": { "type": "text" }
                } } }),
            );
        }
        // One lone raw-JSON index: `summary` joins by the family route.
        obj.insert(
            "xc-lone-json-000".to_string(),
            json!({ "mappings": { "properties": {
                "summary": { "type": "text" }
            } } }),
        );
        assert_eq!(
            resolve_fields(Some(&Value::Object(obj))),
            vec!["body".to_string(), "summary^0.5".to_string()],
            "frontmatter (body index) stays out; summary (own-only index) joins at ^0.5"
        );
    }

    /// The cap ranks legs by mapping coverage, not name: a field mapped
    /// by two own-only indices survives while 25 alphabetically-earlier
    /// 1-index fields fight for the remaining slots. This is the rule
    /// that ends the `$comment`-kept-`Summary`-cut failure the #1244
    /// alphabetical cap produced on exploit-pocs-2026.
    #[test]
    fn the_cap_ranks_legs_by_coverage_not_name() {
        let mut obj = serde_json::Map::new();
        for i in 0..4 {
            obj.insert(
                format!("xc-code-{i:03}"),
                json!({ "mappings": { "properties": {
                    "body": { "type": "text" }
                } } }),
            );
        }
        let mut wide = serde_json::Map::new();
        for i in 1..=25 {
            wide.insert(format!("a{i:02}"), json!({ "type": "text" }));
        }
        obj.insert(
            "xc-wide-json-000".to_string(),
            json!({ "mappings": { "properties": Value::Object(wide) } }),
        );
        for i in 0..2 {
            obj.insert(
                format!("xc-zfamily-{i:03}"),
                json!({ "mappings": { "properties": {
                    "z_field": { "type": "text" }
                } } }),
            );
        }
        // 7 indices -> quarter gate 2: z_field (2) passes the union gate
        // outright; the a-fields join via the family route at count 1.
        // Coverage order: body(4), z_field(2), then a01.. by name; cap 24
        // cuts a23/a24/a25 and keeps z_field despite its name.
        let got = resolve_fields(Some(&Value::Object(obj)));
        assert!(
            got.contains(&"z_field^0.5".to_string()),
            "2-index field kept"
        );
        assert!(!got.contains(&"a23^0.5".to_string()), "26th leg cut");
        assert!(got.contains(&"a01^0.5".to_string()) && got.contains(&"a22^0.5".to_string()));
        assert_eq!(got.first().unwrap(), "body");
        assert_eq!(got.len(), 1 + 24 - 1); // body full-weight + 23 own legs
    }

    #[test]
    fn symbol_impl_is_detected_and_excluded_from_file_leg_fields() {
        let m = mapping(&[("xc-code-000", &["body", "defs", "title", "impl"], false)]);
        assert!(has_symbol_impl(Some(&m)));
        assert_eq!(
            resolve_fields(Some(&m)),
            vec!["body".to_string(), "defs".into(), "title".into()],
            "impl is queried by its own BM25 leg under --fuse-symbols, never folded into file multi_match"
        );
        let no_impl = mapping(&[("xc-code-000", &["body", "defs", "title"], false)]);
        assert!(!has_symbol_impl(Some(&no_impl)));
    }

    /// A keyword `title` is the measured code-corpus list. Dropping it would
    /// move BM25. Only a numeric mapping is poison.
    #[test]
    fn keyword_title_stays_in_the_measured_field_list() {
        let m = json!({ "i": { "mappings": { "properties": {
            "body": { "type": "text" },
            "defs": { "type": "text" },
            "title": { "type": "keyword" }
        } } } });
        assert_eq!(
            resolve_fields(Some(&m)),
            vec!["body".to_string(), "defs".into(), "title".into()]
        );
    }

    /// One index mapping `title` as `long` makes a wildcard `multi_match` on
    /// `title` fail every index with `For input string: "<query>"` (#1284).
    /// The qabot-review field report (2026-10-16) was this shape: `xerj code`
    /// exited 2 on a 132-index corpus whose hand-written `body`/`defs` query
    /// returned hits. `title` leaves the list; `body` and `defs` stay.
    #[test]
    fn a_numeric_title_on_any_index_is_omitted_from_the_union_query() {
        let m = json!({
            "code": { "mappings": { "properties": {
                "body": { "type": "text" },
                "defs": { "type": "text" },
                "title": { "type": "text" }
            } } },
            "stats": { "mappings": { "properties": {
                "body": { "type": "text" },
                "title": { "type": "long" }
            } } }
        });
        let fields = resolve_fields(Some(&m));
        assert!(fields.iter().any(|f| f == "body"), "{fields:?}");
        assert!(fields.iter().any(|f| f == "defs"), "{fields:?}");
        assert!(
            !fields.iter().any(|f| f.split('^').next() == Some("title")),
            "{fields:?}"
        );
    }

    /// An own field that is text on one index and `integer` on another is
    /// the same rejection. It must not take a slot in the 24-leg cap.
    #[test]
    fn an_own_field_numeric_on_any_index_is_not_a_recall_leg() {
        let m = json!({
            "prose": { "mappings": { "properties": {
                "body": { "type": "text" },
                "summary": { "type": "text" }
            } } },
            "counts": { "mappings": { "properties": {
                "body": { "type": "text" },
                "summary": { "type": "integer" }
            } } }
        });
        let fields = resolve_fields(Some(&m));
        assert!(
            !fields
                .iter()
                .any(|f| f.split('^').next() == Some("summary")),
            "{fields:?}"
        );
        assert!(fields.iter().any(|f| f == "body"), "{fields:?}");
    }

    /// When `body` itself is numeric and nothing else is text, the historical
    /// `["body"]` floor would be the rejection. Empty means "do not send".
    #[test]
    fn a_numeric_body_and_nothing_else_does_not_fall_back_to_body() {
        let m = json!({ "i": { "mappings": { "properties": {
            "body": { "type": "double" }
        } } } });
        assert!(resolve_fields(Some(&m)).is_empty());
    }
}
