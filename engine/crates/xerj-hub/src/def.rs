//! Corpus Hub v2 — `def` mode: one exact definition lookup (plan §3).
//!
//! `def` answers "what is X" from a corpus's `-defs` index: a term query on the
//! `symbol` keyword (exact), with a `match` on the analysed `symbol_text` as a
//! fallback for a near miss. Hits are exact-first, then by file path. Zero hits
//! is a normal, empty [`Answer`] — a miss is an answer, not an error.
//!
//! [`lookup`] is the one function the CLI (`xerj code <slug> --def`), the MCP
//! `xerj_hub_def` tool (plan §7) and the hosted `/_hub/v1/def` route (plan §6)
//! all call, so every surface returns the same [`Answer`]. It reads the local
//! state ledger for the corpus's pinned revision and licence (written by
//! `xerj hub pull`, see `state::write_hubpack_state`) and queries the node for
//! the hits; the query half ([`query_defs`]) is pure over [`XcHttp`] and unit
//! tested with a fake node.

use chrono::NaiveDate;
use serde_json::{json, Value};
use xerj_common::hub::{Answer, AnswerMode, AnsweredFrom, Hit, LicenceClass};
use xerj_common::xccode::{state, XcHttp};

/// `def`'s default result count (plan §3: "k default 5").
pub const DEFAULT_K: usize = 5;
/// Never return more than this, however large a `k` a caller passes.
const MAX_K: usize = 50;

/// Corpus facts the [`Answer`] carries, resolved from the local state ledger.
struct DefMeta {
    corpus: String,
    index_prefix: String,
    pin_sha: String,
    pin_date: String,
    age_days: u32,
    licence_spdx: String,
    licence_class: LicenceClass,
    answered_from: AnsweredFrom,
    /// A note to attach to the answer (e.g. the corpus is not indexed here).
    warning: Option<String>,
}

impl DefMeta {
    /// Resolve the corpus's pin, licence and the exact index prefix from
    /// `{root}/state/{slug}.json`. A hub pack carries pin + licence directly; a
    /// corpus that is not indexed here degrades to the `xc-<slug>` namespace
    /// with a warning (the CLI refuses that case earlier with an exit code; the
    /// hosted and MCP surfaces get an honest empty answer instead of a panic).
    fn resolve(root: &std::path::Path, slug: &str) -> DefMeta {
        match state::load_state(root, slug) {
            Ok(st) => {
                let licence_class = parse_licence_class(st.licence_class.as_deref());
                let pin_date = st.pin_date.clone().unwrap_or_default();
                let age_days = age_days_from(&pin_date, st.indexed_at.as_deref());
                DefMeta {
                    corpus: st.name(slug),
                    index_prefix: state::query_prefix(&st),
                    pin_sha: st.pin_sha.clone().unwrap_or_default(),
                    pin_date,
                    age_days,
                    licence_spdx: st.licence_spdx.clone().unwrap_or_default(),
                    licence_class,
                    answered_from: if st.is_hubpack() {
                        AnsweredFrom::LocalPack
                    } else {
                        AnsweredFrom::LocalIndex
                    },
                    warning: None,
                }
            }
            Err(_) => DefMeta {
                corpus: slug.to_string(),
                index_prefix: format!("xc-{slug}"),
                pin_sha: String::new(),
                pin_date: String::new(),
                age_days: 0,
                licence_spdx: String::new(),
                licence_class: LicenceClass::AdaptWithAttribution,
                answered_from: AnsweredFrom::LocalPack,
                warning: Some(format!(
                    "corpus '{slug}' is not pulled; run `xerj hub pull {slug}`"
                )),
            },
        }
    }
}

/// Look up `symbol` in `slug`'s defs index and build the [`Answer`].
///
/// `k` is the max hits (0 means [`DEFAULT_K`]); it is clamped to [`MAX_K`].
/// Reads the local state ledger for the corpus's pin and licence, then queries
/// `<index_prefix>-defs`. A transport error becomes a warning on an otherwise
/// empty answer (the CLI reports transport failures as an exit code before it
/// gets here).
pub fn lookup(http: &impl XcHttp, slug: &str, symbol: &str, k: usize) -> Answer {
    let root = xerj_autoindex::xc::code_root();
    lookup_in(http, &root, slug, symbol, k)
}

/// [`lookup`] with the state root injected — the seam the tests use so they do
/// not depend on `$HOME`/`$XERJ_CODE_HOME`.
pub fn lookup_in(
    http: &impl XcHttp,
    root: &std::path::Path,
    slug: &str,
    symbol: &str,
    k: usize,
) -> Answer {
    let k = if k == 0 { DEFAULT_K } else { k.min(MAX_K) };
    let meta = DefMeta::resolve(root, slug);
    let defs_index = format!("{}-defs", meta.index_prefix);

    let mut warnings = Vec::new();
    if let Some(w) = &meta.warning {
        warnings.push(w.clone());
    }
    let hits = match query_defs(http, &defs_index, symbol, k) {
        Ok(hits) => hits,
        Err(e) if e.contains("__404__") => {
            // No defs index for this corpus — treated as "no definition here".
            Vec::new()
        }
        Err(e) => {
            warnings.push(format!("defs query failed: {e}"));
            Vec::new()
        }
    };

    build_answer(&meta, hits, warnings)
}

/// Build the [`Answer`] from resolved corpus facts and the retrieved hits. An
/// approach-only corpus tags every hit with the read-don't-copy licence
/// warning (plan §5), so the licence travels with the text wherever the answer
/// is rendered.
fn build_answer(meta: &DefMeta, mut hits: Vec<Hit>, warnings: Vec<String>) -> Answer {
    if meta.licence_class == LicenceClass::ApproachOnly {
        let note = format!(
            "approach-only licence ({}): read, do not copy",
            if meta.licence_spdx.is_empty() {
                "unknown"
            } else {
                &meta.licence_spdx
            }
        );
        for hit in &mut hits {
            if hit.licence_warning.is_none() {
                hit.licence_warning = Some(note.clone());
            }
        }
    }
    Answer {
        corpus: meta.corpus.clone(),
        pin_sha: meta.pin_sha.clone(),
        pin_date: meta.pin_date.clone(),
        age_days: meta.age_days,
        licence_spdx: meta.licence_spdx.clone(),
        licence_class: meta.licence_class,
        answered_from: meta.answered_from,
        mode: AnswerMode::Def,
        hits,
        truncated: false,
        warnings,
    }
}

/// The pure query half: a term query on `symbol` with a `match` on
/// `symbol_text` as a fallback, against `defs_index`, returning up to `k` hits
/// sorted exact-first then by file path. `Err` carries the transport/HTTP
/// reason (incl. the `__404__` sentinel for a missing index).
pub fn query_defs(
    http: &impl XcHttp,
    defs_index: &str,
    symbol: &str,
    k: usize,
) -> Result<Vec<Hit>, String> {
    // Fetch a window wider than `k` so the exact-first re-sort has the exact
    // hit to promote even when fuzzy `symbol_text` matches are plentiful, then
    // truncate to `k`.
    let window = k.saturating_mul(4).clamp(10, MAX_K);
    let body = json!({
        "size": window,
        "query": {
            "bool": {
                "should": [
                    { "term": { "symbol": symbol } },
                    { "match": { "symbol_text": symbol } }
                ],
                "minimum_should_match": 1
            }
        }
    });

    let resp = http.search(defs_index, &body)?;
    let rows = resp
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Parse each row, remembering whether its `symbol` is an EXACT match so the
    // re-sort can put exact definitions first.
    let mut parsed: Vec<(bool, String, u32, Hit)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let src = match row.get("_source") {
            Some(s) => s,
            None => continue,
        };
        let sym = src.get("symbol").and_then(Value::as_str).unwrap_or("");
        let file = src
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let line = src.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
        let hit = Hit {
            file: file.clone(),
            line,
            kind: src
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            text: src
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value: src.get("value").and_then(Value::as_str).map(str::to_string),
            score: row.get("_score").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            licence_warning: None,
        };
        parsed.push((sym == symbol, file, line, hit));
    }

    // Exact matches first, then by file path, then by line — a total,
    // deterministic order independent of the server's score.
    parsed.sort_by(|a, b| {
        b.0.cmp(&a.0) // exact (true) before fuzzy (false)
            .then_with(|| a.1.cmp(&b.1)) // file path ascending
            .then_with(|| a.2.cmp(&b.2)) // line ascending
    });
    parsed.truncate(k);
    Ok(parsed.into_iter().map(|(_, _, _, h)| h).collect())
}

/// Map a kebab-case licence-class string to the enum, defaulting to the most
/// permissive (`adapt-with-attribution`) when absent or unrecognised — the
/// answer still carries the SPDX, and an unknown class never upgrades to a
/// stricter one by surprise.
fn parse_licence_class(s: Option<&str>) -> LicenceClass {
    match s {
        Some("approach-only") => LicenceClass::ApproachOnly,
        Some("excluded") => LicenceClass::Excluded,
        _ => LicenceClass::AdaptWithAttribution,
    }
}

/// Days between the pin date (`YYYY-MM-DD`) and today, else between
/// `indexed_at` and now, else 0. Never negative.
fn age_days_from(pin_date: &str, indexed_at: Option<&str>) -> u32 {
    if let Ok(d) = NaiveDate::parse_from_str(pin_date, "%Y-%m-%d") {
        let today = chrono::Utc::now().date_naive();
        return (today - d).num_days().max(0) as u32;
    }
    if let Some(stamp) = indexed_at {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%SZ") {
            let age = chrono::Utc::now()
                .naive_utc()
                .signed_duration_since(dt)
                .num_days();
            return age.max(0) as u32;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake [`XcHttp`] that returns a canned search response and records the
    /// query body it was handed, so a test can assert the def query shape.
    struct FakeNode {
        response: Value,
        last_body: RefCell<Option<Value>>,
    }
    impl FakeNode {
        fn new(response: Value) -> Self {
            FakeNode {
                response,
                last_body: RefCell::new(None),
            }
        }
    }
    impl XcHttp for FakeNode {
        fn get_mapping(&self, _: &str) -> Result<Value, String> {
            Ok(json!({}))
        }
        fn cat_indices_json(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn search(&self, _index: &str, body: &Value) -> Result<Value, String> {
            *self.last_body.borrow_mut() = Some(body.clone());
            Ok(self.response.clone())
        }
    }

    fn hit_row(symbol: &str, file: &str, line: u32, value: Option<&str>, score: f64) -> Value {
        json!({
            "_score": score,
            "_source": {
                "symbol": symbol,
                "symbol_text": symbol,
                "kind": "define",
                "lang": "c",
                "file": file,
                "line": line,
                "text": format!("#define {symbol} {}", value.unwrap_or("")),
                "value": value,
            }
        })
    }

    #[test]
    fn query_builds_a_term_plus_match_should() {
        let node = FakeNode::new(json!({"hits": {"hits": []}}));
        let _ = query_defs(&node, "xc-x-b1-defs", "FOO", 5).unwrap();
        let body = node.last_body.borrow().clone().unwrap();
        let should = body
            .pointer("/query/bool/should")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(should.len(), 2);
        assert_eq!(should[0].pointer("/term/symbol").unwrap(), &json!("FOO"));
        assert_eq!(
            should[1].pointer("/match/symbol_text").unwrap(),
            &json!("FOO")
        );
        assert_eq!(
            body.pointer("/query/bool/minimum_should_match").unwrap(),
            &json!(1)
        );
    }

    #[test]
    fn exact_symbol_sorts_before_fuzzy_then_by_file() {
        // A fuzzy match scores HIGHER than the exact one; exact must still win,
        // and ties break on file path.
        let resp = json!({"hits": {"hits": [
            hit_row("MINI_LIMIT_MAX", "z.c", 9, Some("1"), 99.0),   // fuzzy, high score
            hit_row("MINI_LIMIT", "b.c", 7, Some("128"), 2.0),      // exact
            hit_row("MINI_LIMIT", "a.c", 3, Some("64"), 1.0),       // exact, earlier file
        ]}});
        let node = FakeNode::new(resp);
        let hits = query_defs(&node, "xc-mini-c-b1-defs", "MINI_LIMIT", 5).unwrap();
        assert_eq!(hits.len(), 3);
        // exact first, by file path
        assert_eq!((hits[0].file.as_str(), hits[0].line), ("a.c", 3));
        assert_eq!(hits[0].value.as_deref(), Some("64"));
        assert_eq!((hits[1].file.as_str(), hits[1].line), ("b.c", 7));
        assert_eq!(hits[1].value.as_deref(), Some("128"));
        // fuzzy last
        assert_eq!(hits[2].file.as_str(), "z.c");
    }

    #[test]
    fn k_truncates_after_the_exact_first_sort() {
        let resp = json!({"hits": {"hits": [
            hit_row("A", "a.c", 1, None, 1.0),
            hit_row("A", "b.c", 2, None, 1.0),
            hit_row("A", "c.c", 3, None, 1.0),
        ]}});
        let node = FakeNode::new(resp);
        let hits = query_defs(&node, "idx", "A", 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].file, "a.c");
        assert_eq!(hits[1].file, "b.c");
    }

    #[test]
    fn missing_index_404_is_a_normal_empty_answer() {
        struct NotFound;
        impl XcHttp for NotFound {
            fn get_mapping(&self, _: &str) -> Result<Value, String> {
                Ok(json!({}))
            }
            fn cat_indices_json(&self, _: &str) -> Result<Vec<String>, String> {
                Ok(vec![])
            }
            fn search(&self, _: &str, _: &Value) -> Result<Value, String> {
                Err("__404__".to_string())
            }
        }
        let root = tempfile::tempdir().unwrap();
        // write a hubpack state so meta resolves
        state::write_hubpack_state(
            root.path(),
            "lz4",
            "http://x",
            "xc-lz4-b1",
            "abc",
            "2026-10-02",
            "BSD-2-Clause",
            "adapt-with-attribution",
        )
        .unwrap();
        let ans = lookup_in(&NotFound, root.path(), "lz4", "NOPE", 5);
        assert!(ans.hits.is_empty());
        assert_eq!(ans.mode, AnswerMode::Def);
        assert_eq!(ans.corpus, "lz4");
        assert_eq!(ans.answered_from, AnsweredFrom::LocalPack);
        // The shipped renderer prints the corpus + pin on the empty answer.
        let text = xerj_common::hub::render(&ans);
        assert!(text.contains("no match in lz4"), "{text}");
    }

    #[test]
    fn approach_only_corpus_tags_each_hit_and_answer_carries_pin() {
        let root = tempfile::tempdir().unwrap();
        state::write_hubpack_state(
            root.path(),
            "sonic",
            "http://x",
            "xc-sonic-b1",
            "deadbeef",
            "2026-10-01",
            "GPL-3.0-only",
            "approach-only",
        )
        .unwrap();
        let node = FakeNode::new(json!({"hits": {"hits": [
            hit_row("SEARCH_LIMIT", "store.c", 5, Some("1000"), 3.0),
        ]}}));
        let ans = lookup_in(&node, root.path(), "sonic", "SEARCH_LIMIT", 5);
        assert_eq!(ans.hits.len(), 1);
        assert_eq!(ans.licence_class, LicenceClass::ApproachOnly);
        assert_eq!(ans.pin_sha, "deadbeef");
        assert_eq!(ans.pin_date, "2026-10-01");
        assert_eq!(
            ans.hits[0].licence_warning.as_deref(),
            Some("approach-only licence (GPL-3.0-only): read, do not copy")
        );
    }

    #[test]
    fn not_pulled_corpus_warns_but_does_not_panic() {
        let node = FakeNode::new(json!({"hits": {"hits": []}}));
        let root = tempfile::tempdir().unwrap();
        let ans = lookup_in(&node, root.path(), "ghost", "X", 5);
        assert!(ans.hits.is_empty());
        assert!(ans
            .warnings
            .iter()
            .any(|w| w.contains("not pulled") && w.contains("xerj hub pull ghost")));
    }
}
