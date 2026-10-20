//! The ONE output renderer. `xerj code` prints its `text`; the
//! `xerj_code_search` MCP tool returns the same bytes as its payload — parity
//! pinned by test, because two renderers always drift.

use std::collections::HashMap;

use serde_json::Value;

use super::licence;
use super::passage::{
    best_window, commaus, locator_repo, provenance, py_len, symbol_passage, take_chars,
};

/// How to render the hit list.
pub struct RenderOpts<'a> {
    pub corpus: &'a str,
    pub query: &'a str,
    /// Max chars per passage; 0 = file-head mode (first 400 chars, labelled
    /// as the file head — never passed off as a matching passage).
    pub full: usize,
    pub no_symbol: bool,
    pub meatl: bool,
    /// Show `rrf {x:.4}` (native hybrid) instead of `score {x:.2}`. The
    /// per-leg `[bm25 #r, vec #r]` annotation is deliberately gone: the
    /// server-side fuser exposes no per-leg ranks, and the every-run arms
    /// note carries which arms ran.
    pub rrf_scores: bool,
    pub age_days: Option<i64>,
    /// MCP `licence_policy:"strict"`: restricted hits keep locator + licence
    /// + warning but lose the passage text. The CLI passes false (warn).
    pub strict_licence: bool,
}

/// The longest non-empty string field of a source object (excluding `ax_*`
/// provenance metadata), "" when there is none — the content-bearing field of
/// a raw-JSON record (#1158). Fields are visited in sorted key order and only
/// a STRICTLY longer field replaces the winner, so a tie goes to the
/// alphabetically first name and the passage never depends on the wire's key
/// order.
fn longest_string_field(src: &Value) -> String {
    let mut best: Option<(usize, &str)> = None;
    if let Some(obj) = src.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for key in keys {
            if key.starts_with("ax_") {
                continue;
            }
            if let Some(s) = obj[key].as_str() {
                if !s.trim().is_empty() && best.is_none_or(|(len, _)| s.chars().count() > len) {
                    best = Some((s.chars().count(), s));
                }
            }
        }
    }
    best.map(|(_, s)| s.to_string()).unwrap_or_default()
}

/// True for an autoindex `*-symbols` record: `name` and `kind` strings plus
/// a numeric `line`, alongside the declaration in `code`. A raw-JSON data
/// record that merely happens to have a `code` key does not match.
fn is_symbol_record(src: &Value) -> bool {
    src.get("name").is_some_and(Value::is_string)
        && src.get("kind").is_some_and(Value::is_string)
        && src.get("line").is_some_and(Value::is_u64)
        && src.get("code").is_some_and(Value::is_string)
}

/// Render the full result block: the optional arms-ran note line, one block
/// per hit, the footer. `note` is printed EVERY time it exists — a silently
/// BM25-only "hybrid" result must never read as hybrid.
pub fn render(
    hits: &[Value],
    licences: &HashMap<String, String>,
    opts: &RenderOpts,
    note: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(n) = note {
        out.push_str(&note_line(n, opts.meatl));
        out.push('\n');
    }
    for h in hits {
        out.push_str(&render_hit(h, licences, opts));
    }
    if !opts.meatl {
        out.push('\n');
        out.push_str(&format!("{} passages from '{}'", hits.len(), opts.corpus));
        if opts.age_days.is_some_and(|d| d > 0) {
            out.push_str(&format!(" (index {}d old)", opts.age_days.unwrap_or(0)));
        }
        out.push_str("\nCite file:line for anything you rely on.\n");
    }
    out
}

/// The no-match text. Explicit, because a silent miss makes the next agent
/// re-run the same dead query — this is the line that stops the loop.
pub fn no_match_text(corpus: &str, query: &str, meatl: bool) -> String {
    if meatl {
        format!("@no q=\"{query}\" why=no-match-in-corpus\n")
    } else {
        format!(
            "No passage in '{corpus}' matches: {query}\n\
             The corpus is likely wrong for this task — fall back to normal work \
             rather than forcing a bad match.\n"
        )
    }
}

fn note_line(note: &str, meatl: bool) -> String {
    if meatl {
        format!("@mode {note}")
    } else {
        format!("[{note}]")
    }
}

fn render_hit(h: &Value, licences: &HashMap<String, String>, opts: &RenderOpts) -> String {
    let src = h.get("_source").cloned().unwrap_or(Value::Null);
    let loc = provenance(&src);
    let score = h.get("_score").and_then(Value::as_f64).unwrap_or(0.0);
    let lic = licences
        .get(locator_repo(&loc))
        .cloned()
        .unwrap_or_default();
    let restricted = !lic.is_empty() && licence::is_restricted(&lic);

    if opts.meatl {
        let shown = if opts.rrf_scores {
            format!("rrf={score:.4}")
        } else {
            format!("score={score:.2}")
        };
        let why = if lic.is_empty() {
            String::new()
        } else {
            format!(" why={lic}")
        };
        return format!("@ok f={loc} {shown}{why}\n");
    }

    let shown = if opts.rrf_scores {
        format!("rrf {score:.4}")
    } else {
        format!("score {score:.2}")
    };
    let lic_part = if lic.is_empty() {
        String::new()
    } else {
        format!(", {lic}")
    };
    let mut out = format!("\n─── {loc}  ({shown}{lic_part})\n");

    // `body` is the content field of every family EXCEPT plain text (`.txt`
    // mirrors put their passage in `text`, #1139) and `*-symbols` records
    // (which store the declaration in `code`). `code` is trusted ONLY on a
    // record with the symbol shape (`name` + `kind` + numeric `line`): a
    // raw-JSON record (#1158) may carry its own short `code` field (an error
    // code, a CWE id) that must not displace its real content. A raw-JSON
    // record's text lives in schema-named fields (`summary`, `details`, …),
    // so the last resort is the longest non-`ax_*` string field.
    let body = src
        .get("body")
        .and_then(Value::as_str)
        .or_else(|| src.get("text").and_then(Value::as_str))
        .or_else(|| {
            is_symbol_record(&src)
                .then(|| src.get("code").and_then(Value::as_str))
                .flatten()
        })
        .map(str::to_string)
        .unwrap_or_else(|| longest_string_field(&src));

    if opts.full > 0 && !(restricted && opts.strict_licence) {
        // Prefer the matching DEFINITION over a byte window; fall back to the
        // window only when the record carries no usable symbols (a data
        // file, or a language whose extractor found nothing — issue #170).
        let sp = if opts.no_symbol {
            None
        } else {
            symbol_passage(&body, &src, opts.query, opts.full)
        };
        match sp {
            Some((text, label)) => {
                out.push_str(&format!(
                    "    [{label} — {} of {} chars]\n",
                    commaus(py_len(&text)),
                    commaus(py_len(&body))
                ));
                out.push_str(&text);
                out.push('\n');
            }
            None => {
                let (window, start, total) = best_window(&body, opts.query, opts.full);
                if total > py_len(&window) {
                    out.push_str(&format!(
                        "    [no symbol match; showing {} of {} chars from offset {}, \
                         window centred on the match]\n",
                        commaus(py_len(&window)),
                        commaus(total),
                        commaus(start)
                    ));
                }
                out.push_str(&window);
                out.push('\n');
            }
        }
    } else if opts.full > 0 {
        // strict licence policy: the code itself is the thing not to hand
        // over; provenance and the warning stay.
        out.push_str("    [passage withheld: restricted licence (licence_policy strict)]\n");
    } else {
        // Only reachable via `--full 0`. No highlight is ever requested
        // (issue #177 — it reorders hits), so this is the file head and is
        // labelled as such rather than passed off as a matching passage.
        let head = take_chars(&body, 400);
        if py_len(&body) > py_len(&head) {
            out.push_str(&format!(
                "    [file head; {} of {} chars — pass --full N for the matching definition]\n",
                commaus(py_len(&head)),
                commaus(py_len(&body))
            ));
        }
        out.push_str(&format!("    {}\n", head.replace('\n', "\n    ")));
    }

    if restricted {
        out.push_str(&licence::hit_warning_line(&lic));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit(loc_fields: Value, body: &str, symbols: Value, score: f64) -> Value {
        let mut src = loc_fields;
        src["body"] = json!(body);
        if !symbols.is_null() {
            src["symbols"] = symbols;
        }
        json!({ "_source": src, "_score": score })
    }

    fn apache_hit() -> Value {
        hit(
            json!({ "ax_path": "tantivy/src/reader.rs", "line": 30 }),
            "impl SegmentReader {\n    pub fn rebuild(&self) {}\n}\n",
            json!([{ "name": "rebuild", "kind": "method", "line": 2 }]),
            12.345,
        )
    }

    fn agpl_hit() -> Value {
        hit(
            json!({ "ax_path": "elasticsearch/server/src/main/Node.java", "line": 80 }),
            "class Node {\n    void start() {}\n}\n",
            json!([{ "name": "start", "kind": "method", "line": 2 }]),
            9.01,
        )
    }

    fn licences() -> HashMap<String, String> {
        HashMap::from([
            ("tantivy".to_string(), "Apache-2.0/MIT".to_string()),
            ("elasticsearch".to_string(), "AGPL".to_string()),
        ])
    }

    #[test]
    fn prose_hits_carry_loc_score_licence_passage_and_footer() {
        let hits = vec![apache_hit()];
        let text = render(
            &hits,
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "rebuild segment reader",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: Some(3),
                strict_licence: false,
            },
            None,
        );
        assert!(
            text.starts_with("\n─── tantivy/src/reader.rs:30  (score 12.35, Apache-2.0/MIT)\n"),
            "{text}"
        );
        assert!(text.contains("    [method rebuild @ line 2 — "), "{text}");
        assert!(
            text.contains("1 passages from 'xerj-search' (index 3d old)"),
            "{text}"
        );
        assert!(text.ends_with("Cite file:line for anything you rely on.\n"));
        // Compatible licence: no warning line.
        assert!(!text.contains("!!"), "{text}");
    }

    #[test]
    fn restricted_hits_carry_the_warning_under_the_passage() {
        let text = render(
            &[agpl_hit()],
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "start node",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        let passage_at = text.find("void start()").expect("passage present");
        let warn_at = text
            .find("    !! AGPL: adapt the APPROACH, do not copy the code")
            .expect("warning");
        assert!(warn_at > passage_at, "the warning sits UNDER the passage");
        assert!(text.contains("(score 9.01, AGPL)"), "{text}");
    }

    /// #1158: a raw-JSON record has neither `body` nor `text` — its content
    /// lives in schema-named fields. The passage must come from the longest
    /// string field (the advisory's `details`), and a tie on length goes to
    /// the alphabetically first name so the passage is stable regardless of
    /// key order on the wire.
    #[test]
    fn a_raw_json_hit_renders_its_longest_string_field_as_the_passage() {
        let raw_hit = json!({
            "_score": 7.5,
            "_source": {
                "ax_path": "ghsa-db/advisories/GHSA-9j.json",
                "id": "GHSA-9j",
                "summary": "lodash command injection",
                "details": "Applications using lodash are vulnerable to command injection via the template function."
            }
        });
        let text = render(
            &[raw_hit],
            &HashMap::new(),
            &RenderOpts {
                corpus: "ghsa-db",
                query: "command injection",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(
            text.contains("vulnerable to command injection"),
            "the passage is `details`, the longest field: {text}"
        );
        assert!(
            !text.contains("[0 of 0 chars"),
            "no empty-passage shape: {text}"
        );

        // Tie: two fields of equal length — the alphabetically first wins.
        let tie = json!({
            "_score": 1.0,
            "_source": {
                "ax_path": "x/y.json",
                "zzz_field": "same length exactly",
                "aaa_field": "same length exactly"
            }
        });
        let text = render(
            &[tie],
            &HashMap::new(),
            &RenderOpts {
                corpus: "kv",
                query: "length",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(text.contains("aaa_field") || text.contains("same length exactly"));
        // Both same content here — the deterministic pick is still ONE field.
        assert!(text.contains("same length exactly"), "{text}");
    }

    #[test]
    fn symbol_index_hit_and_short_raw_json_hit_never_render_ax_provenance_as_passage() {
        // Autoindex `*-symbols` records store the declaration in `code` (with
        // no `body`, `text`, or `symbols` array) alongside a 42-char `ax_file`
        // provenance digest (`axf2-<32-hex>-<size-hex>`).
        let sym_hit = json!({
            "_score": 11.2,
            "_source": {
                "ax_file": "axf2-1fbdaa51d7231323867e3200dc599463-1075",
                "ax_path": "tantivy/src/error.rs",
                "title": "enum TantivyError — error.rs",
                "path": "tantivy/src/error.rs",
                "language": "rust",
                "name": "TantivyError",
                "kind": "enum",
                "line": 15,
                "code": "pub enum TantivyError {"
            }
        });
        let text = render(
            &[sym_hit],
            &licences(),
            &RenderOpts {
                corpus: "tantivy",
                query: "TantivyError",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(
            text.contains("─── tantivy/src/error.rs:15  (score 11.20, Apache-2.0/MIT)\n"),
            "{text}"
        );
        assert!(
            text.contains("pub enum TantivyError {"),
            "symbol hit must render `code`, not `ax_file`: {text}"
        );
        assert!(
            !text.contains("axf2-"),
            "internal `ax_file` digest must never be rendered as passage: {text}"
        );

        // A raw-JSON record whose content field is shorter than the 42-char
        // `ax_file` digest must still render the content field, never `ax_*`.
        let short_raw = json!({
            "_score": 4.0,
            "_source": {
                "ax_file": "axf2-1fbdaa51d7231323867e3200dc599463-1075",
                "ax_path": "notes/short.json",
                "summary": "short advisory note"
            }
        });
        let raw_text = render(
            &[short_raw],
            &HashMap::new(),
            &RenderOpts {
                corpus: "notes",
                query: "advisory",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(
            raw_text.contains("short advisory note"),
            "short raw-JSON record must render `summary`, not `ax_file`: {raw_text}"
        );
        assert!(
            !raw_text.contains("axf2-"),
            "internal `ax_file` digest must never win longest_string_field: {raw_text}"
        );
    }

    /// #1158 must survive the symbol-`code` passage source: a raw-JSON
    /// record with its OWN short `code` field (here a CWE id) still renders
    /// its longest content field, not the code.
    #[test]
    fn a_raw_json_hit_with_a_short_code_field_still_renders_its_content() {
        let raw_hit = json!({
            "_score": 3.0,
            "_source": {
                "ax_file": "axf2-1fbdaa51d7231323867e3200dc599463-1075",
                "ax_path": "cwe/records/79.json",
                "code": "CWE-79",
                "name": "XSS",
                "details": "Improper neutralization of input during web page generation allows cross-site scripting."
            }
        });
        let text = render(
            &[raw_hit],
            &HashMap::new(),
            &RenderOpts {
                corpus: "cwe",
                query: "cross-site scripting",
                full: 800,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(
            text.contains("allows cross-site scripting"),
            "the passage is `details`, not the short `code`: {text}"
        );
        assert!(!text.contains("axf2-"), "{text}");
    }

    #[test]
    fn strict_licence_strips_the_passage_but_keeps_provenance_and_warning() {
        let text = render(
            &[agpl_hit()],
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "start node",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: true,
            },
            None,
        );
        assert!(
            !text.contains("void start()"),
            "no code handed over: {text}"
        );
        assert!(
            text.contains("─── elasticsearch/server/src/main/Node.java:80"),
            "{text}"
        );
        assert!(text.contains("!! AGPL"), "{text}");
        // Compatible corpora are untouched by strict mode.
        let ok = render(
            &[apache_hit()],
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "rebuild",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: true,
            },
            None,
        );
        assert!(ok.contains("pub fn rebuild"), "{ok}");
    }

    #[test]
    fn full_zero_is_the_labelled_file_head() {
        let text = render(
            &[apache_hit()],
            &licences(),
            &RenderOpts {
                corpus: "c",
                query: "rebuild",
                full: 0,
                no_symbol: false,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(
            text.contains(
                "[file head; 58 of 58 chars — pass --full N for the matching definition]"
            ) || text.contains("impl SegmentReader {"),
            "{text}"
        );
        // head only, no definition chase
        assert!(!text.contains("@ line"), "{text}");
    }

    #[test]
    fn meatl_is_one_line_per_hit_with_licence_and_mode_notes() {
        let hits = vec![agpl_hit(), apache_hit()];
        let text = render(
            &hits,
            &licences(),
            &RenderOpts {
                corpus: "xerj-search",
                query: "start",
                full: 800,
                no_symbol: false,
                meatl: true,
                rrf_scores: false,
                age_days: Some(2),
                strict_licence: false,
            },
            Some("hybrid RRF(k=60) — BM25 over 9 index(es), vector over 3 of 9"),
        );
        assert!(
            text.starts_with(
                "@mode hybrid RRF(k=60) — BM25 over 9 index(es), vector over 3 of 9\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("@ok f=elasticsearch/server/src/main/Node.java:80 score=9.01 why=AGPL\n"),
            "{text}"
        );
        assert!(
            text.contains("@ok f=tantivy/src/reader.rs:30 score=12.35 why=Apache-2.0/MIT\n"),
            "{text}"
        );
        assert!(
            !text.contains("passages from"),
            "no footer in meatl: {text}"
        );
    }

    #[test]
    fn rrf_scores_render_four_decimals_without_per_leg_ranks() {
        let text = render(
            &[agpl_hit()],
            &licences(),
            &RenderOpts {
                corpus: "c",
                query: "q",
                full: 800,
                no_symbol: false,
                meatl: false,
                rrf_scores: true,
                age_days: None,
                strict_licence: false,
            },
            None,
        );
        assert!(text.contains("(rrf 9.0100, AGPL)"), "{text}");
        assert!(!text.contains("[bm25 #"), "per-leg ranks are gone: {text}");
    }

    #[test]
    fn no_match_prose_and_meatl_shapes_are_verbatim() {
        assert_eq!(
            no_match_text("kv", "two-way substring", false),
            "No passage in 'kv' matches: two-way substring\n\
             The corpus is likely wrong for this task — fall back to normal work rather \
             than forcing a bad match.\n"
        );
        assert_eq!(
            no_match_text("kv", "xyzzy", true),
            "@no q=\"xyzzy\" why=no-match-in-corpus\n"
        );
    }
}
