//! # xccode — the reference-coding semantics shared by `xerj code`, the
//! `xerj corpus` lifecycle, and the `xerj_code_search` MCP tool
//!
//! Until issue #977 the reference-coding loop lived in three wrapper scripts
//! (`tools/xerj-code/scripts/xc.py`, `xc-index.sh`, `xc-corpus.sh`). They are
//! ported here as ONE pure implementation so the CLI and the MCP tool serve
//! byte-identical prose from the same renderer, and so every contract — the
//! 30-day staleness refusal, the restricted-licence warning, the
//! not-loaded-here diagnosis, the exit-code triangle — has exactly one home
//! and one test suite.
//!
//! Layout (all persistent paths parameterised by `root`; tests inject
//! tempdirs — that is test hygiene, NOT a layout change):
//!
//! * [`state`] — the `state/<corpus>.json` ledger: read, staleness,
//!   incomplete-coverage warning, atomic write
//! * [`licence`] — restricted-licence tuple + the clone-time detector
//! * [`manifest`] — `corpora/<corpus>/corpus.json` read/write + hub manifests
//! * [`pathgate`] — the hard gate every manifest-derived path passes before
//!   anything destructive runs inside it
//! * [`fields`] — mapping-resolved `multi_match` fields + semantic-capable
//!   index discovery
//! * [`passage`] — symbol-passage / best-window / file-head selection
//! * [`render`] — the one output renderer (prose + MEATL)
//!
//! HTTP is a 3-method shell ([`XcHttp`]) so the pure core stays free of any
//! client dependency: `xerj-autoindex` implements it over its blocking `Es`
//! client, `xerj-mcp` over reqwest. No fusion or query logic lives in either
//! — hybrid retrieval emits the engine's native top-level `hybrid` query and
//! lets the server fuse.

pub mod fields;
pub mod licence;
pub mod manifest;
pub mod passage;
pub mod pathgate;
pub mod render;
pub mod state;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

/// Corpora older than this are refused, not served: a stale index returns
/// code that no longer exists, with false confidence (`STALE_DAYS` in the
/// original `xc.py`).
pub const STALE_DAYS: i64 = 30;

/// Reciprocal Rank Fusion constant (Cormack, Clarke & Buettcher, SIGIR 2009).
/// k=60 is the paper's value and the universal default. It is NOT tuned here.
/// Fusion itself is SERVER-SIDE (native `hybrid` query, #943); this constant
/// only names k in the request we emit and in the arms-ran note.
pub const RRF_K: u64 = 60;

/// The retrieval arm. Default is [`Mode::Bm25`] — measured 12/12 top-3 across
/// both standing corpora (bm25 beat hybrid and semantic on the combined
/// metric; see `tools/xerj-code/SKILL.md`'s retrieval-mode table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bm25,
    Semantic,
    Hybrid,
}

impl Mode {
    /// Parse the `--mode` / MCP `mode` argument. Unknown values are usage
    /// errors (exit 2), never a silent default.
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "bm25" => Some(Mode::Bm25),
            "semantic" => Some(Mode::Semantic),
            "hybrid" => Some(Mode::Hybrid),
            _ => None,
        }
    }
}

/// Everything one `xerj code` invocation (or `xerj_code_search` tool call)
/// needs. Defaults mirror `xc.py` exactly: k=5, bm25, full=800, symbols on.
#[derive(Debug, Clone)]
pub struct CodeParams {
    pub corpus: String,
    pub query: String,
    pub k: usize,
    pub lang: Option<String>,
    pub mode: Mode,
    /// Max chars of each passage; 0 = file-head mode (first 400 chars,
    /// labelled as the file head — retrieving the definition IS the feature,
    /// issue #368, so this is a cap, never an opt-out).
    pub full: usize,
    pub no_symbol: bool,
    /// #1237: opt-in second BM25 leg over symbol-record `impl` fields, fused
    /// with file-level BM25 at the file level by RRF(k=60).
    pub fuse_symbols: bool,
    pub stale_ok: bool,
    pub meatl: bool,
    /// CLI `--json`: raw server response on stdout. MCP never sets this.
    pub as_json: bool,
    /// MCP `licence_policy:"strict"`: restricted-licence hits keep
    /// provenance + licence + warning but lose the passage text. The CLI
    /// stays `false` (warn) for `xc.py` parity.
    pub strict_licence: bool,
}

impl CodeParams {
    pub fn new(corpus: &str, query: &str) -> Self {
        CodeParams {
            corpus: corpus.to_string(),
            query: query.to_string(),
            k: 5,
            lang: None,
            mode: Mode::Bm25,
            full: 800,
            no_symbol: false,
            fuse_symbols: false,
            stale_ok: false,
            meatl: false,
            as_json: false,
            strict_licence: false,
        }
    }
}

/// The one HTTP shell the pure core needs. Implemented over the blocking `Es`
/// client in `xerj-autoindex` and over reqwest in `xerj-mcp`; fakes in tests.
///
/// `cat_indices_json` maps HTTP 404 to `Ok(vec![])` — a wildcard that matches
/// no index means "none", not an error — while every other failure is `Err`:
/// a node that cannot answer must never be reported as "0 live indices".
pub trait XcHttp {
    /// `GET /{pattern}/_mapping`. `Err` carries a human-readable reason.
    fn get_mapping(&self, pattern: &str) -> Result<Value, String>;
    /// `GET /_cat/indices/{pattern}?format=json&h=index` as index names.
    /// `Err` = unreachable/ambiguous (never a silent zero).
    fn cat_indices_json(&self, pattern: &str) -> Result<Vec<String>, String>;
    /// `POST /{index}/_search`. `Err` carries status + reason or transport.
    fn search(&self, index: &str, body: &Value) -> Result<Value, String>;
}

/// Everything a caller needs after the pipeline ran. The CLI prints `text`
/// (stdout, or stderr when [`Self::to_stderr`] — refusals are diagnostics)
/// and exits with [`Self::exit`]; MCP returns `text` as the tool payload with
/// [`Self::is_error`]. Warnings ride stderr on the CLI and the TOP of the
/// tool text on MCP — MCP has no stderr channel, so warnings are content.
#[derive(Debug, Clone)]
pub struct CodeOutcome {
    pub text: String,
    pub warnings: Vec<String>,
    /// 0 = hits, 1 = no match (a miss is an answer), 2 = usage / stale /
    /// transport, 3 = corpus in state/ but 0 live indices on this node.
    pub exit: i32,
    /// MCP `isError`: true exactly for the action-needed states (2 and 3).
    /// A no-match is `false` with the fall-back prose.
    pub is_error: bool,
    /// Raw server response when `as_json` was requested (CLI-only flag).
    pub json: Option<Value>,
    pub to_stderr: bool,
}

impl CodeOutcome {
    fn refused(text: String, exit: i32) -> Self {
        CodeOutcome {
            text,
            warnings: Vec::new(),
            exit,
            is_error: true,
            json: None,
            to_stderr: true,
        }
    }
}

/// The whole query pipeline, pure: ledger → staleness → live-count → mapping
/// discovery → query → passage selection → licence map → render.
///
/// `url` is the resolved node URL (for diagnostics); `stale_hint` is the
/// override spelling the caller's surface accepts — "`--stale-ok`" on the
/// CLI, "`stale_ok:true`" on MCP — so the one refusal text names the fix the
/// caller can actually type.
pub fn run_code_query(
    root: &Path,
    http: &impl XcHttp,
    url: &str,
    params: &CodeParams,
    stale_hint: &str,
) -> CodeOutcome {
    let corpus = params.corpus.clone();
    let query = params.query.clone();

    // 1. The ledger. Not indexed is a usage-class refusal naming the command
    //    that fixes it (exit 2 — the caller's driver script should stop).
    let st = match state::load_state(root, &corpus) {
        Ok(st) => st,
        Err(msg) => return CodeOutcome::refused(msg, 2),
    };

    // 2. Staleness: refuse BEFORE any query. A stale index returns code that
    //    no longer exists; --stale-ok / stale_ok:true is the only override
    //    (no env bypass — a wrapper must not be able to de-fang this).
    let age_days = match state::check_fresh(&st, params.stale_ok, stale_hint) {
        Ok(age) => age,
        Err(refusal) => return CodeOutcome::refused(refusal, 2),
    };

    let prefix = state::query_prefix(&st);
    // Legacy state (indexed before builds existed) names no verified build,
    // so the query widens to the whole `xc-<corpus>` namespace — and whatever
    // stale generations live there are mixed into the answers (#1136). Say
    // so at the point it happens; stderr, because stdout is the parsed
    // hit list. `corpus index <name> --fresh` pins one verified build.
    if st.index_prefix.is_none() {
        eprintln!(
            "xerj code: notice: corpus '{corpus}' has no verified build recorded — querying \
             the whole xc-{corpus} namespace; `xerj corpus index {corpus} --fresh` pins one \
             verified build"
        );
    }

    // 3. Not-loaded-here: in state/ yet 0 live indices on THIS node. A
    //    distinct, actionable diagnosis (exit 3) — collapsing it into
    //    "no match" is the state-ledger trust trap this guard closes.
    //    An unreachable node is reported honestly (exit 2) by the search
    //    path below, never as "0 live indices".
    match http.cat_indices_json(&format!("{prefix}*")) {
        Ok(indices) if indices.is_empty() => {
            return CodeOutcome::refused(state::not_loaded_message(&st, &prefix, url), 3)
        }
        // Unreachable/ambiguous: fall through; the search itself will report
        // the transport failure honestly.
        _ => {}
    }

    let mut warnings = Vec::new();
    if let Some(w) = state::incomplete_coverage(&st) {
        warnings.push(w);
    }

    // 4. Mapping: one read serves both field resolution and capable-set
    //    discovery. Unreadable is data, not an error (bm25 degrades to the
    //    full field list; hybrid degrades to BM25-only).
    let pattern = format!("{prefix}*");
    let mapping = http.get_mapping(&format!("/{pattern}/_mapping")).ok();
    let mut fields = fields::resolve_fields(mapping.as_ref());
    if fields.is_empty() {
        // resolve_fields returns empty only when every candidate, including
        // `body`, is numeric on some index. Sending the historical floor
        // would be the #1284 rejection.
        return CodeOutcome::refused(
            format!(
                "no text field under '{pattern}' can take this query — a mapped \
                 field is numeric, and a text query against it is rejected"
            ),
            2,
        );
    }
    // #1254: a corpus may DECLARE the weight of its `text` recall leg. The
    // 0.5 default is the #1238 calibration, measured on the live exploit
    // group where `text` carries sibling-CVE mirror demos that must not
    // outrank code-family `body` hits — but a corpus whose PRIMARY content
    // rides the plain-text family is invisible at 0.5. Measured on the
    // rebuilt otel-proto corpus (every `.proto` definition is a txt-lines
    // record): all four proto-needled G7 queries missed with the needle at
    // rank 19-39, and all four returned rank 1 at weight 1.0 with the index
    // byte-identical — only the weight moved. The discriminator between the
    // two corpus shapes is invisible at query time (mixed datasets, mixed
    // index mappings look identical from the client), so it is author
    // knowledge: declared once in the corpus's own corpus.json
    // (`query.text_weight`), read here, honoured within honest bounds, and
    // a bad declaration warns and falls back to the default instead of
    // silently guessing.
    match manifest::query_text_weight(root, &corpus) {
        Some(Ok(w)) => {
            if let Some(slot) = fields.iter_mut().find(|f| f.starts_with("text^")) {
                *slot = format!("text^{w}");
            }
        }
        Some(Err(msg)) => warnings.push(format!("corpus.json {msg}; using the default text^0.5")),
        None => {}
    }

    let mut note: Option<String> = None;
    let mut rrf_scores = false;
    // Any leg that comes back `timed_out` makes the page partial. The flag is
    // on the JSON body (HTTP 200), so it is invisible unless we read it.
    let mut deadline_hit = false;

    // #1137: fetch deeper than the page so the per-file cap has candidates
    // to draw on; the page is cut back to `params.k` after diversification.
    let fetch = diversity_fetch(params.k);

    let (mut resp, hits) = match params.mode {
        Mode::Bm25 => match http.search(&pattern, &bm25_body(&query, fetch, &params.lang, &fields))
        {
            Ok(mut r) => {
                note_deadline(&r, &mut deadline_hit);
                let mut h = hit_list(&r);
                if params.fuse_symbols
                    && !params.no_symbol
                    && fields::has_symbol_impl(mapping.as_ref())
                {
                    if let Ok(sym_resp) =
                        http.search(&pattern, &symbol_bm25_body(&query, fetch, &params.lang))
                    {
                        note_deadline(&sym_resp, &mut deadline_hit);
                        let sym_hits = hit_list(&sym_resp);
                        if !sym_hits.is_empty() {
                            h = fuse_file_and_symbol_hits(
                                http,
                                &pattern,
                                h,
                                sym_hits,
                                fetch,
                                &mut deadline_hit,
                            );
                            if let Some(hits_arr) =
                                r.pointer_mut("/hits/hits").and_then(Value::as_array_mut)
                            {
                                *hits_arr = h.clone();
                            }
                            rrf_scores = true;
                            note = Some(format!("BM25 file+symbol RRF(k={RRF_K})"));
                        }
                    }
                }
                (r, h)
            }
            Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
        },
        Mode::Semantic => {
            // Standalone semantic is fatal on mapping/search failure: it has
            // no BM25 result to keep.
            let mapping = match http.get_mapping(&format!("/{pattern}/_mapping")) {
                Ok(m) => m,
                Err(e) => {
                    return CodeOutcome::refused(
                        format!("semantic mapping lookup failed at {url}: {e}"),
                        2,
                    )
                }
            };
            // Group by the field each index actually embeds. One request
            // cannot name two semantic fields, and naming `body` at an index
            // whose embedded field is `code` or `text` 400s every index in
            // the comma-join.
            let (groups, total) = fields::semantic_groups(Some(&mapping));
            if groups.is_empty() {
                // No semantic arm is possible: do NOT post the query. An
                // empty comma-join is an empty index segment, which the
                // server reads as `/_search` — every index on the node,
                // corpus or not. The empty answer with its note is the
                // honest result.
                note = Some(format!(
                    "no index under '{pattern}' maps a field as semantic_text"
                ));
                (Value::Null, Vec::new())
            } else {
                let indexed: usize = groups.iter().map(|(_, names)| names.len()).sum();
                let described = groups
                    .iter()
                    .map(|(field, names)| format!("{field} ({})", names.len()))
                    .collect::<Vec<_>>()
                    .join(", ");
                note = Some(format!(
                    "vector only over {indexed} of {total} index(es): {described}"
                ));
                let mut merged: Option<Value> = None;
                for (field, names) in &groups {
                    let body = semantic_on_field(field, &query, fetch, &params.lang);
                    match http.search(&names.join(","), &body) {
                        Ok(response) => {
                            note_deadline(&response, &mut deadline_hit);
                            merged = Some(match merged {
                                Some(prev) => merge_by_score(prev, &response, fetch),
                                None => response,
                            });
                        }
                        Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
                    }
                }
                let response = merged.unwrap_or(Value::Null);
                (response.clone(), hit_list(&response))
            }
        }
        Mode::Hybrid => {
            let (capable, total) = fields::semantic_capable(mapping.as_ref());
            if capable.is_empty() {
                // No vector arm possible: degrade, and SAY so.
                note = Some(format!(
                    "BM25 only — no usable semantic_text mapping for `body` could be \
                     discovered under '{pattern}'"
                ));
                match http.search(&pattern, &bm25_body(&query, fetch, &params.lang, &fields)) {
                    Ok(r) => {
                        note_deadline(&r, &mut deadline_hit);
                        (r.clone(), hit_list(&r))
                    }
                    Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
                }
            } else {
                // BM25 preflight: the lexical embedder returns confident
                // neighbours for ANY input, so a BM25 miss is the corpus's
                // only honest "no" — vector hits must not launder it.
                match http.search(&pattern, &bm25_body(&query, 1, &params.lang, &fields)) {
                    Ok(pre)
                        if {
                            note_deadline(&pre, &mut deadline_hit);
                            hit_list(&pre).is_empty()
                        } =>
                    {
                        note = Some(
                            "no lexical match in this corpus — vector nearest-neighbours \
                             are not evidence of a match, so this is reported as a miss"
                                .to_string(),
                        );
                        (Value::Null, Vec::new())
                    }
                    Ok(pre) => {
                        note_deadline(&pre, &mut deadline_hit);
                        // The engine's native top-level `hybrid` (RRF k=60) —
                        // no client-side fusion. Aimed at the comma-joined
                        // CAPABLE set when only some indices map `body` as
                        // semantic_text: a semantic leg posted at a wildcard
                        // covering plain-text indices 400s the WHOLE request.
                        let target = if capable.len() == total {
                            pattern.clone()
                        } else {
                            capable.join(",")
                        };
                        let body = hybrid_body(&query, fetch, &params.lang, &fields);
                        match http.search(&target, &body) {
                            Ok(r) => {
                                note_deadline(&r, &mut deadline_hit);
                                rrf_scores = true;
                                let mut n = format!(
                                    "hybrid RRF(k={RRF_K}) — BM25 over {total} index(es), \
                                     vector over {} of {total}",
                                    capable.len()
                                );
                                let lexical_only: Vec<String> = if capable.len() < total {
                                    mapping
                                        .as_ref()
                                        .and_then(Value::as_object)
                                        .map(|obj| {
                                            obj.keys()
                                                .filter(|k| !capable.contains(k))
                                                .cloned()
                                                .collect()
                                        })
                                        .unwrap_or_default()
                                } else {
                                    Vec::new()
                                };
                                if lexical_only.is_empty() {
                                    note = Some(n);
                                    (r.clone(), hit_list(&r))
                                } else {
                                    // #1146: the lexical-only indices still
                                    // owe the BM25 leg. The engine fuses PER
                                    // INDEX and merges indices by `_score`,
                                    // so the same `hybrid` with its BM25 leg
                                    // alone returns RRF scores on the same
                                    // scale — merging the two responses by
                                    // score is the engine's own cross-index
                                    // step, not a second fusion.
                                    let lex_body =
                                        hybrid_bm25_leg_body(&query, fetch, &params.lang, &fields);
                                    match http.search(&lexical_only.join(","), &lex_body) {
                                        Ok(lex) => {
                                            note_deadline(&lex, &mut deadline_hit);
                                            n.push_str(&format!(
                                                "; lexical-only (BM25 leg only, merged by \
                                                 score): {}",
                                                lexical_only.join(", ")
                                            ));
                                            note = Some(n);
                                            let merged = merge_by_score(r, &lex, fetch);
                                            let hits = hit_list(&merged);
                                            (merged, hits)
                                        }
                                        Err(e) => {
                                            return CodeOutcome::refused(
                                                format!("search failed: {e}"),
                                                2,
                                            )
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                // The vector arm failed, not the corpus:
                                // degrade to plain BM25, never abort.
                                note = Some(format!(
                                    "BM25 only — vector search failed or returned no hits \
                                     from the {} semantic_text index(es)",
                                    capable.len()
                                ));
                                match http.search(
                                    &pattern,
                                    &bm25_body(&query, fetch, &params.lang, &fields),
                                ) {
                                    Ok(r) => {
                                        note_deadline(&r, &mut deadline_hit);
                                        (r.clone(), hit_list(&r))
                                    }
                                    Err(e) => {
                                        return CodeOutcome::refused(
                                            format!("search failed: {e}"),
                                            2,
                                        )
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => return CodeOutcome::refused(format!("search failed: {e}"), 2),
                }
            }
        }
    };

    // #1504: a vector leg ran. The note already says how many indices it
    // covered; it did not say which embedder produced the vectors. The
    // default is lexical feature-hashing, and a caller who read "vector
    // over N of M" has guessed neural before. The line states the default
    // and the flag that changes it. It does not claim the running node is
    // or is not on that default — the client cannot see `--embed-mode`.
    if let Some(n) = note.as_mut() {
        if n.contains("vector over") || n.contains("vector only") {
            n.push_str(
                "; vector leg uses the node's embedder (default is lexical \
                 feature-hashing, not neural — start the node with --embed-mode neural \
                 for the built-in BERT)",
            );
        }
    }

    // 5. #1137: cut the overfetched window back to the page, capping records
    //    per source file (#1137) and per source index (#1238), and patch the
    //    diversified page into the response so `--json` consumers see the
    //    SAME list the prose renderer does (the G7 grader is one). A page
    //    that shrank says so in the note, naming only the caps that fired.
    // #1504: translation catalogs and minified bundles repeat every UI phrase
    // and win natural-language queries. They stay searchable, behind source.
    let hits = demote_catalog_noise(hits);
    let (hits, file_dropped, index_dropped) = diversify(hits, params.k);
    if file_dropped > 0 || index_dropped > 0 {
        let mut caps = Vec::new();
        if file_dropped > 0 {
            caps.push(format!(
                "≤{MAX_PER_FILE} records per source file ({file_dropped} same-file neighbours)"
            ));
        }
        if index_dropped > 0 {
            caps.push(format!(
                "≤{MAX_PER_INDEX} per source index ({index_dropped} same-index neighbours)"
            ));
        }
        let cap = format!("top-k diversified: {}", caps.join(", "));
        note = Some(match note.take() {
            Some(n) => format!("{n}; {cap}"),
            None => cap,
        });
    }
    // A missed deadline is HTTP 200 with a prefix of the ranking. Say so on
    // every surface: stderr warnings, the prose the CLI prints, and the JSON
    // body (`timed_out`, the same field the node set). Exit stays 0 when
    // there are hits — the page is partial, not a transport failure.
    if deadline_hit {
        warnings.push(DEADLINE_WARNING.to_string());
        match resp.as_object_mut() {
            Some(obj) => {
                obj.insert("timed_out".into(), Value::Bool(true));
            }
            None => {
                resp = serde_json::json!({"timed_out": true, "hits": {"hits": []}});
            }
        }
    }
    if params.as_json {
        if let Some(hits_arr) = resp.pointer_mut("/hits/hits").and_then(Value::as_array_mut) {
            *hits_arr = hits.clone();
        }
        return CodeOutcome {
            text: String::new(),
            warnings,
            exit: if hits.is_empty() { 1 } else { 0 },
            is_error: false,
            json: Some(resp),
            to_stderr: false,
        };
    }

    if hits.is_empty() {
        // Say so explicitly. A silent miss makes the next agent re-run the
        // same dead query; this is the line that stops the loop.
        return CodeOutcome {
            text: render::no_match_text(&corpus, &query, params.meatl),
            warnings,
            exit: 1,
            is_error: false,
            json: None,
            to_stderr: false,
        };
    }

    // 6. Licence map: ALWAYS from the corpus's own corpus.json — never
    //    re-derived at query time, never from prose.
    let licences = manifest::licence_map(root, &corpus);

    let text = render::render(
        &hits,
        &licences,
        &render::RenderOpts {
            corpus: &corpus,
            query: &query,
            full: params.full,
            no_symbol: params.no_symbol,
            meatl: params.meatl,
            rrf_scores,
            age_days,
            strict_licence: params.strict_licence,
        },
        note.as_deref(),
    );

    CodeOutcome {
        text,
        warnings,
        exit: 0,
        is_error: false,
        json: None,
        to_stderr: false,
    }
}

/// The engine answers a missed search deadline with HTTP 200, the hits it
/// gathered, and `timed_out: true`. A caller that only reads `hits` reports
/// that prefix as the whole ranking.
pub(crate) fn response_timed_out(resp: &Value) -> bool {
    resp.get("timed_out") == Some(&Value::Bool(true))
}

fn note_deadline(resp: &Value, deadline_hit: &mut bool) {
    if response_timed_out(resp) {
        *deadline_hit = true;
    }
}

/// What `xerj code` says when the node stopped early. Kept in one place so
/// the prose warning and the regression test cannot drift.
pub(crate) const DEADLINE_WARNING: &str = "\
search deadline hit: the node returned HTTP 200 with timed_out true and a partial \
hit list. These are the hits gathered before the deadline, not a complete ranking.";

/// Hits of a search response, missing containers tolerated as empty.
pub(crate) fn hit_list(resp: &Value) -> Vec<Value> {
    resp.pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Top-k diversity cap (#1137): at most this many records per source FILE in
/// a result page. The autoindex chunker (`SECTION_CHARS` = 2 KB, 200-char
/// overlap) turns one lexically dominant file into a wall of near-identical
/// neighbours that fills the whole top-k — measured 3–4 of 5 slots on the
/// zalando and otel G7 suites while the expected file never ranked.
/// Reference coding wants five FILES more than five passages of one.
pub(crate) const MAX_PER_FILE: usize = 2;

/// Top-k diversity cap (#1238): at most this many records per `_index` (per
/// source REPO in a corpus group) once the fan-out is wide enough to fill the
/// page without any one index. Measured on the live exploit group (query
/// "MCPJam inspector 23744", 36 needle PoC repos across 5,644 indices): one
/// sibling-CVE demo repo took 6 of the top 10 slots — every file a DIFFERENT
/// file, so the #1137 per-file cap could not see the wall — and the client's
/// first page held 1 needle record where the per-index cap holds 5.
pub(crate) const MAX_PER_INDEX: usize = 1;

/// Fetch depth that gives the cap candidates to draw on: 10×k, bounded so a
/// k=50 page does not become a 500-hit fetch, and never below k itself.
pub(crate) fn diversity_fetch(k: usize) -> usize {
    k.saturating_mul(10).min(200).max(k)
}

/// Cap records per `_source.ax_file` at [`MAX_PER_FILE`], and — when the hit
/// set spans at least `want` distinct indices — per `_index` at
/// [`MAX_PER_INDEX`], truncate to `want`, preserving rank order. Returns the
/// page plus how many neighbours EACH cap skipped while filling it (a cap
/// that never engaged reports 0, so the reader-facing note names only the
/// caps that fired). A hit with no `ax_file` (or no `_index`) is never
/// grouped on that key: missing provenance must not cost a slot.
///
/// The distinct-index capacity guard keeps homogeneous corpora exactly as
/// they are: a corpus whose whole fan-out is a handful of shard indices
/// (the blogposts pack is 4) has fewer indices than slots, the per-index
/// cap would only shrink the page, and so it never engages.
/// Stable partition: hits whose path is a translation catalog or a minified
/// bundle move behind every other hit, keeping their relative order. A page
/// that is only catalogs is unchanged. Hits with no path stay where they are.
pub(crate) fn demote_catalog_noise(hits: Vec<Value>) -> Vec<Value> {
    let mut primary = Vec::with_capacity(hits.len());
    let mut catalogs = Vec::new();
    for hit in hits {
        if hit_path(&hit).is_some_and(is_catalog_noise) {
            catalogs.push(hit);
        } else {
            primary.push(hit);
        }
    }
    primary.extend(catalogs);
    primary
}

/// Translation catalogs and minified bundles (#1504). The patterns are the
/// ones a multilingual repo uses for key→string tables, not a general
/// "prose file" filter: `lang/*.php`, `values*/strings.xml`, `*.lproj/*.strings`,
/// `*.po` / `*.pot`, and `*.min.js` / `*.min.mjs` / `*.min.css`.
pub(crate) fn is_catalog_noise(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if name.ends_with(".min.js") || name.ends_with(".min.mjs") || name.ends_with(".min.css") {
        return true;
    }
    if name.ends_with(".po") || name.ends_with(".pot") {
        return true;
    }
    let Some(parent) = parent_segment(path) else {
        return false;
    };
    if parent == "lang" && name.ends_with(".php") {
        return true;
    }
    if name == "strings.xml" && (parent == "values" || parent.starts_with("values-")) {
        return true;
    }
    parent.ends_with(".lproj") && name.ends_with(".strings")
}

fn parent_segment(path: &str) -> Option<&str> {
    let cut = path.rfind(['/', '\\'])?;
    let head = &path[..cut];
    let start = head.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let parent = &head[start..];
    if parent.is_empty() {
        None
    } else {
        Some(parent)
    }
}

fn hit_path(hit: &Value) -> Option<&str> {
    hit.pointer("/_source/ax_path")
        .and_then(Value::as_str)
        .or_else(|| hit.pointer("/_source/ax_file").and_then(Value::as_str))
        .or_else(|| hit.pointer("/_source/path").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

pub(crate) fn diversify(hits: Vec<Value>, want: usize) -> (Vec<Value>, usize, usize) {
    // Keys are owned: a borrowed &str would pin every hit it came from and
    // forbid the move into `out` below.
    let distinct_indices = hits
        .iter()
        .filter_map(|h| h.pointer("/_index").and_then(Value::as_str))
        .collect::<HashSet<&str>>()
        .len();
    let cap_index = distinct_indices >= want;
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut per_index: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(want.min(hits.len()));
    let mut file_dropped = 0usize;
    let mut index_dropped = 0usize;
    for h in hits {
        // Peek before consuming: a hit one cap skips must not burn the other
        // cap's slot.
        let file = h
            .pointer("/_source/ax_file")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let index = h
            .pointer("/_index")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let file_full = file
            .as_ref()
            .is_some_and(|f| per_file.get(f).is_some_and(|n| *n >= MAX_PER_FILE));
        let index_full = cap_index
            && index
                .as_ref()
                .is_some_and(|i| per_index.get(i).is_some_and(|n| *n >= MAX_PER_INDEX));
        if file_full || index_full {
            if file_full {
                file_dropped += 1;
            } else {
                index_dropped += 1;
            }
            continue;
        }
        if let Some(f) = file {
            *per_file.entry(f).or_insert(0) += 1;
        }
        if cap_index {
            if let Some(i) = index {
                *per_index.entry(i).or_insert(0) += 1;
            }
        }
        out.push(h);
        if out.len() == want {
            break;
        }
    }
    (out, file_dropped, index_dropped)
}

/// The measured BM25 body: flat `multi_match` over mapping-resolved fields
/// (`FIELDS`'s flat weights were measured, not chosen — 12/12 top-3). NEVER
/// a highlight block (issue #177: highlight changes ranking on this engine),
/// and no `_source` projection on the corpus path — passage selection needs
/// `body` and `symbols`.
pub(crate) fn bm25_body(query: &str, k: usize, lang: &Option<String>, fields: &[String]) -> Value {
    let mut must = vec![serde_json::json!({
        "multi_match": { "query": query, "fields": fields }
    })];
    if let Some(lg) = lang {
        must.push(serde_json::json!({ "match": { "language": lg } }));
    }
    serde_json::json!({ "size": k, "query": { "bool": { "must": must } } })
}

/// #1237: second BM25 leg over symbol-record `impl` fields. Excludes `impl`
/// and `body` from `_source` so the symbol leg never transfers symbol body
/// text over the wire — only provenance (`ax_path`, `ax_file`, `path`) and
/// the winning symbol span (`name`, `kind`, `line`, `end_line`).
pub(crate) fn symbol_bm25_body(query: &str, k: usize, lang: &Option<String>) -> Value {
    let mut must = vec![serde_json::json!({
        "multi_match": { "query": query, "fields": ["impl"] }
    })];
    if let Some(lg) = lang {
        must.push(serde_json::json!({ "match": { "language": lg } }));
    }
    serde_json::json!({
        "size": k,
        "_source": { "excludes": ["impl", "body"] },
        "query": { "bool": { "must": must } }
    })
}

fn hit_file_key(hit: &Value) -> Option<String> {
    hit.pointer("/_source/ax_path")
        .and_then(Value::as_str)
        .or_else(|| hit.pointer("/_source/ax_file").and_then(Value::as_str))
        .or_else(|| hit.pointer("/_source/path").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// #1237: roll both BM25 legs up to the file level first (each file gets its
/// best 1-indexed file-leg rank and its best 1-indexed symbol-leg rank so a
/// file with ten matching functions does not get ten RRF votes), fuse by
/// `1 / (60 + rank_file) + 1 / (60 + rank_sym)`, and stamp the winning
/// symbol's `line..=end_line` onto `_source.symbols` for passage selection.
pub(crate) fn fuse_file_and_symbol_hits(
    http: &impl XcHttp,
    pattern: &str,
    file_hits: Vec<Value>,
    sym_hits: Vec<Value>,
    limit: usize,
    deadline_hit: &mut bool,
) -> Vec<Value> {
    struct FileEntry {
        file_rank: Option<usize>,
        sym_rank: Option<usize>,
        file_hit: Option<Value>,
        sym_hit: Option<Value>,
        win_sym: Option<(String, String, i64, Option<i64>)>,
    }

    let mut by_file: HashMap<String, FileEntry> = HashMap::new();
    let mut unkeyed: Vec<Value> = Vec::new();
    let mut next_file_rank = 0usize;

    for hit in file_hits {
        let Some(key) = hit_file_key(&hit) else {
            unkeyed.push(hit);
            continue;
        };
        let has_body = hit
            .pointer("/_source/body")
            .and_then(Value::as_str)
            .is_some();
        let entry = by_file.entry(key).or_insert_with(|| {
            next_file_rank += 1;
            FileEntry {
                file_rank: Some(next_file_rank),
                sym_rank: None,
                file_hit: None,
                sym_hit: None,
                win_sym: None,
            }
        });
        if entry.file_hit.is_none()
            || (has_body
                && entry
                    .file_hit
                    .as_ref()
                    .and_then(|h| h.pointer("/_source/body"))
                    .and_then(Value::as_str)
                    .is_none())
        {
            entry.file_hit = Some(hit);
        }
    }

    let mut next_sym_rank = 0usize;
    for hit in sym_hits {
        let Some(key) = hit_file_key(&hit) else {
            continue;
        };
        let sym_span = hit.get("_source").and_then(|src| {
            let name = src.get("name")?.as_str()?.to_string();
            let line = src.get("line")?.as_i64()?;
            let kind = src
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let end_line = src
                .get("end_line")
                .and_then(Value::as_i64)
                .filter(|&e| e >= line);
            Some((name, kind, line, end_line))
        });
        let entry = by_file.entry(key).or_insert_with(|| FileEntry {
            file_rank: None,
            sym_rank: None,
            file_hit: None,
            sym_hit: None,
            win_sym: None,
        });
        if entry.sym_rank.is_none() {
            next_sym_rank += 1;
            entry.sym_rank = Some(next_sym_rank);
            entry.sym_hit = Some(hit);
            entry.win_sym = sym_span;
        }
    }

    let mut ranked: Vec<(String, f64, usize, FileEntry)> = by_file
        .into_iter()
        .map(|(key, entry)| {
            let f_score = entry
                .file_rank
                .map(|r| 1.0 / (RRF_K as f64 + r as f64))
                .unwrap_or(0.0);
            let s_score = entry
                .sym_rank
                .map(|r| 1.0 / (RRF_K as f64 + r as f64))
                .unwrap_or(0.0);
            let best_rank = entry
                .file_rank
                .unwrap_or(usize::MAX)
                .min(entry.sym_rank.unwrap_or(usize::MAX));
            (key, f_score + s_score, best_rank, entry)
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(limit);

    let mut out = Vec::with_capacity(ranked.len());
    for (key, score, _, mut entry) in ranked {
        let has_body = entry
            .file_hit
            .as_ref()
            .and_then(|h| h.pointer("/_source/body"))
            .and_then(Value::as_str)
            .is_some();
        if !has_body {
            // File entered via the symbol leg alone: fetch its file record so
            // passage selection has `body` to slice.
            let lookup = serde_json::json!({
                "size": 1,
                "query": {
                    "bool": {
                        "must": [
                            { "exists": { "field": "body" } },
                            { "match_phrase": { "ax_path": key } }
                        ]
                    }
                }
            });
            let target = entry
                .sym_hit
                .as_ref()
                .and_then(|h| h.get("_index"))
                .and_then(Value::as_str)
                .unwrap_or(pattern);
            if let Ok(resp) = http.search(target, &lookup) {
                note_deadline(&resp, deadline_hit);
                if let Some(fetched) = hit_list(&resp)
                    .into_iter()
                    .find(|h| hit_file_key(h).as_deref() == Some(key.as_str()))
                {
                    entry.file_hit = Some(fetched);
                }
            }
        }
        let Some(mut hit) = entry.file_hit.or(entry.sym_hit) else {
            continue;
        };
        if let Some((name, kind, line, end_line)) = entry.win_sym {
            if let Some(src) = hit.get_mut("_source").and_then(Value::as_object_mut) {
                let syms = src
                    .entry("symbols")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(arr) = syms.as_array_mut() {
                    let mut updated = false;
                    for s in arr.iter_mut() {
                        if s.get("name").and_then(Value::as_str) == Some(name.as_str())
                            && s.get("line").and_then(Value::as_i64) == Some(line)
                        {
                            if let (Some(obj), Some(end)) = (s.as_object_mut(), end_line) {
                                obj.insert("end_line".to_string(), serde_json::json!(end));
                            }
                            updated = true;
                            break;
                        }
                    }
                    if !updated {
                        let mut sym_obj = serde_json::Map::new();
                        sym_obj.insert("name".to_string(), Value::String(name));
                        sym_obj.insert("kind".to_string(), Value::String(kind));
                        sym_obj.insert("line".to_string(), serde_json::json!(line));
                        if let Some(end) = end_line {
                            sym_obj.insert("end_line".to_string(), serde_json::json!(end));
                        }
                        arr.push(Value::Object(sym_obj));
                    }
                }
            }
        }
        if let Some(obj) = hit.as_object_mut() {
            obj.insert("_score".to_string(), serde_json::json!(score));
        }
        out.push(hit);
    }
    for hit in unkeyed {
        if out.len() >= limit {
            break;
        }
        out.push(hit);
    }
    out
}

/// The vector-only body aimed at `field`. A semantic query against a plain
/// `text` field 400s the whole request, so callers pass only indices that
/// embed this field. Hybrid still hardcodes `body`; semantic mode passes
/// the field [`fields::semantic_groups`] elected.
///
/// The clause carries `k` = the fetch window: without it the parser cuts the
/// vector pool to 10 whatever `size` says (#1145).
pub(crate) fn semantic_on_field(
    field: &str,
    query: &str,
    k: usize,
    lang: &Option<String>,
) -> Value {
    let q = serde_json::json!({ "semantic": { "field": field, "query": query, "k": k } });
    // `--lang` rides in `filter`: the engine sends a bool to the vector path
    // only when the semantic clause is its sole must/should clause (#1148).
    let query = match lang {
        Some(lg) => serde_json::json!({
            "bool": { "must": [q], "filter": [{ "match": { "language": lg } }] }
        }),
        None => q,
    };
    serde_json::json!({ "size": k, "query": query })
}

/// ONE native top-level `hybrid` request: BM25 clause + semantic clause,
/// fused server-side by RRF(k=60). Hybrid anywhere else is a 400 (#943), so
/// this shape is the whole contract — and there is deliberately NO
/// client-side fusion left to drift from the engine's.
pub(crate) fn hybrid_body(
    query: &str,
    k: usize,
    lang: &Option<String>,
    fields: &[String],
) -> Value {
    // `k` on the semantic clause: the vector leg's pool is cut to its own
    // `k` (parser default 10), not to `size`, so without it the #1137
    // overfetch widens the BM25 leg alone (#1145).
    let sem = with_lang(
        serde_json::json!({ "semantic": { "field": "body", "query": query, "k": k } }),
        lang,
    );
    hybrid_request(k, vec![bm25_leg(query, lang, fields), sem])
}

/// [`hybrid_body`] with its BM25 leg alone, for the lexical-only indices of
/// a mixed corpus (#1146). Still a native `hybrid`, so the engine stamps the
/// same per-index RRF score (`1/(k+rank)`) the full request does and the two
/// responses merge by `_score` exactly as the engine merges indices.
pub(crate) fn hybrid_bm25_leg_body(
    query: &str,
    k: usize,
    lang: &Option<String>,
    fields: &[String],
) -> Value {
    hybrid_request(k, vec![bm25_leg(query, lang, fields)])
}

fn bm25_leg(query: &str, lang: &Option<String>, fields: &[String]) -> Value {
    with_lang(
        serde_json::json!({ "multi_match": { "query": query, "fields": fields } }),
        lang,
    )
}

/// `--lang` must constrain BOTH legs (xc.py semantics): a language filter
/// on BM25 only lets the vector leg surface docs the user filtered out.
/// It is a `filter`, not a second `must`: a semantic clause beside a
/// sibling in `must` falls through to the lexical path and matches
/// nothing (#1148). Both legs take the same shape.
fn with_lang(q: Value, lang: &Option<String>) -> Value {
    match lang {
        Some(lg) => serde_json::json!({
            "bool": { "must": [q], "filter": [{ "match": { "language": lg } }] }
        }),
        None => q,
    }
}

fn hybrid_request(k: usize, legs: Vec<Value>) -> Value {
    let queries: Vec<Value> = legs
        .into_iter()
        .map(|q| serde_json::json!({ "query": q }))
        .collect();
    serde_json::json!({
        "size": k,
        "query": { "hybrid": {
            "queries": queries,
            "fusion": { "method": "rrf", "k": RRF_K }
        }}
    })
}

/// Merge two search responses the way the engine merges the indices of one
/// multi-index search: `_score` descending, stable — so each response keeps
/// its own rank order on a tie — with `_index` breaking cross-response ties
/// so the page does not depend on which request was sent first. Cut to
/// `size`; `hits.total` is the sum of both; the rest is `first`'s.
pub(crate) fn merge_by_score(mut first: Value, second: &Value, size: usize) -> Value {
    let mut hits = hit_list(&first);
    hits.extend(hit_list(second));
    let score = |h: &Value| h.get("_score").and_then(Value::as_f64).unwrap_or(f64::MIN);
    let index = |h: &Value| {
        h.get("_index")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    hits.sort_by(|a, b| {
        score(b)
            .total_cmp(&score(a))
            .then_with(|| index(a).cmp(&index(b)))
    });
    hits.truncate(size);

    let total_of = |r: &Value| r.pointer("/hits/total/value").and_then(Value::as_u64);
    let gte = |r: &Value| r.pointer("/hits/total/relation").and_then(Value::as_str) == Some("gte");
    let total = match (total_of(&first), total_of(second)) {
        (Some(a), Some(b)) => Some(serde_json::json!({
            "value": a + b,
            "relation": if gte(&first) || gte(second) { "gte" } else { "eq" }
        })),
        _ => None,
    };
    let max_score = hits.first().and_then(|h| h.get("_score")).cloned();
    if let Some(h) = first.get_mut("hits").and_then(Value::as_object_mut) {
        h.insert("hits".into(), Value::Array(hits));
        if let Some(t) = total {
            h.insert("total".into(), t);
        }
        if let Some(m) = max_score {
            h.insert("max_score".into(), m);
        }
    }
    first
}

/// `xerj corpus list` needs per-corpus live counts without the full pipeline;
/// this is the honest tri-state the ledger listing shares with `require_loaded`.
pub fn live_count_summary(http: &impl XcHttp, prefix: &str) -> Result<usize, String> {
    http.cat_indices_json(&format!("{prefix}*"))
        .map(|v| v.len())
}

/// Licence map for a corpus, exposed for `xerj corpus list`'s review.use line.
pub fn corpus_review_uses(root: &Path, corpus: &str) -> HashMap<String, String> {
    manifest::read_corpus_manifest(&root.join("corpora").join(corpus).join("corpus.json"))
        .map(|m| {
            m.repos
                .iter()
                .filter_map(|r| {
                    r.review
                        .as_ref()
                        .and_then(|v| v.get("use"))
                        .and_then(Value::as_str)
                        .map(|u| (r.repo.clone(), u.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake node: records every request, answers from canned data.
    struct FakeHttp {
        mapping: Value,
        hits: Vec<Value>,
        // (index, body) per search, in order — pins the EXACT wire shape.
        searches: std::sync::Mutex<Vec<(String, Value)>>,
        live: Vec<String>,
        fail_search: bool,
        // Per-target answers (#1146); a target not listed gets `hits`.
        hits_by_target: HashMap<String, Vec<Value>>,
        /// The node stopped at its deadline: HTTP 200, `timed_out: true`,
        /// and only the hits gathered so far.
        timed_out: bool,
    }

    impl FakeHttp {
        fn new() -> Self {
            FakeHttp {
                mapping: serde_json::json!({
                    "xc-kv-b1-000": { "mappings": { "properties": {
                        "body": { "type": "text" }, "defs": { "type": "text" },
                        "title": { "type": "text" }
                    }}}
                }),
                hits: vec![serde_json::json!({
                    "_score": 4.2,
                    "_source": {
                        "ax_path": "valkey/src/networking.c", "line": 1460,
                        "body": "void addReplyNull(client *c) {\n    addReplyProto(c, \"$-1\\r\\n\", 5);\n}\n",
                        "symbols": [ { "name": "addReplyNull", "kind": "function", "line": 1 } ]
                    }
                })],
                searches: std::sync::Mutex::new(Vec::new()),
                live: vec!["xc-kv-b1-000".to_string()],
                fail_search: false,
                hits_by_target: HashMap::new(),
                timed_out: false,
            }
        }
    }

    impl XcHttp for FakeHttp {
        fn get_mapping(&self, pattern: &str) -> Result<Value, String> {
            // The real adapter hands `path` to `Es::get_json`, which sends it
            // verbatim after `base` — a path without the leading `/` is a
            // malformed URL, not a 404. Assert the shape here so a regression
            // in the caller fails the tests instead of silently passing.
            assert!(
                pattern.starts_with('/'),
                "get_mapping path must be request-shaped, got {pattern:?}"
            );
            Ok(self.mapping.clone())
        }
        fn cat_indices_json(&self, _pattern: &str) -> Result<Vec<String>, String> {
            Ok(self.live.clone())
        }
        fn search(&self, index: &str, body: &Value) -> Result<Value, String> {
            self.searches
                .lock()
                .unwrap()
                .push((index.to_string(), body.clone()));
            if self.fail_search {
                return Err("transport: connection refused".to_string());
            }
            let hits = self.hits_by_target.get(index).unwrap_or(&self.hits);
            let mut response = serde_json::json!({ "hits": { "hits": hits } });
            if self.timed_out {
                response["timed_out"] = serde_json::json!(true);
            }
            Ok(response)
        }
    }

    fn root_with_state() -> std::path::PathBuf {
        let root = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(
            root.join("state/kv.json"),
            format!(
                "{{\"corpus\":\"kv\",\"indexed_at\":\"{}\",\"prefix\":\"xc-kv\",\"url\":\"u\",\
                 \"autoindex_exit\":0,\"salvaged\":false,\"build\":\"b1\",\
                 \"index_prefix\":\"xc-kv-b1\",\"state_dir\":\"/tmp/s\"}}",
                chrono::Utc::now().format(state::STAMP_FORMAT)
            ),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("corpora/kv/valkey")).unwrap();
        std::fs::write(
            root.join("corpora/kv/corpus.json"),
            "{\"corpus\":\"kv\",\"cloned_at\":\"t\",\"repos\":[{\"repo\":\"valkey\",\"url\":\"u\",\
             \"licence\":\"BSD-3-Clause\",\"sha\":\"s\"}]}",
        )
        .unwrap();
        root
    }

    #[test]
    fn bm25_query_uses_the_star_direct_glob_and_never_highlights() {
        let root = root_with_state();
        let http = FakeHttp::new();
        let out = run_code_query(
            &root,
            &http,
            "http://localhost:9200",
            &CodeParams::new("kv", "addReplyNull"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 0);
        let (idx, body) = http.searches.lock().unwrap()[0].clone();
        assert_eq!(idx, "xc-kv-b1*", "STAR-direct glob, not the dash form");
        assert_eq!(
            body,
            serde_json::json!({
                "size": 50,
                "query": { "bool": { "must": [
                    { "multi_match": { "query": "addReplyNull",
                        "fields": ["body", "defs", "title"] } }
                ]}}
            }),
            "mapping-resolved fields (defs_expanded dropped); no highlight, no _source \
             projection; size is the #1137 diversity fetch (10x k=5), cut back to k after"
        );
        assert!(out
            .text
            .contains("─── valkey/src/networking.c:1460  (score 4.20, BSD-3-Clause)"));
        assert!(out.text.contains("function addReplyNull @ line 1"));
        assert!(out.text.contains("Cite file:line"));
        assert!(!out.is_error);
        // No field-report nudge marker was ever part of this port (dropped
        // 2026-09-18): the prose must not ask the agent to report back.
        assert!(!out.text.to_lowercase().contains("report back"));
        assert!(
            out.warnings.iter().all(|w| !w.contains("timed_out")),
            "a complete response must not be marked partial: {:?}",
            out.warnings
        );
    }

    /// The engine's 30s search deadline answers HTTP 200 with `timed_out:
    /// true` and a prefix of the hits. `xerj code` used to print that prefix
    /// as a finished ranking.
    #[test]
    fn a_search_deadline_marks_the_result_partial() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.timed_out = true;
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "addReplyNull"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 0, "partial hits are still a successful page");
        assert!(
            out.warnings.iter().any(|w| w.contains("timed_out")),
            "the deadline must be a warning, not a silent 200: {:?}",
            out.warnings
        );
        assert!(
            out.text.contains("valkey/src/networking.c"),
            "the hits gathered before the deadline are still shown: {}",
            out.text
        );

        let mut params = CodeParams::new("kv", "addReplyNull");
        params.as_json = true;
        let out = run_code_query(&root, &http, "u", &params, "`--stale-ok`");
        assert_eq!(
            out.json.as_ref().and_then(|v| v.get("timed_out")),
            Some(&serde_json::json!(true)),
            "json output keeps the ES timed_out flag: {:?}",
            out.json
        );
    }

    #[test]
    fn restricted_licence_warns_under_the_passage_end_to_end() {
        let root = root_with_state();
        std::fs::write(
            root.join("corpora/kv/corpus.json"),
            "{\"corpus\":\"kv\",\"repos\":[{\"repo\":\"valkey\",\"url\":\"u\",\"licence\":\"AGPL\"}]}",
        )
        .unwrap();
        let http = FakeHttp::new();
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert!(out
            .text
            .contains("!! AGPL: adapt the APPROACH, do not copy the code"));
    }

    #[test]
    fn not_in_state_not_loaded_and_no_match_are_three_different_answers() {
        let root = root_with_state();

        // Not in the ledger at all: usage-class refusal naming the fix (exit 2).
        let out = run_code_query(
            &root,
            &FakeHttp::new(),
            "u",
            &CodeParams::new("ghost", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 2);
        assert!(out.text.contains("corpus 'ghost' is not indexed"));
        assert!(out.text.contains("xerj corpus index ghost"));

        // In the ledger, 0 live indices: the DISTINCT exit-3 diagnosis.
        let mut http = FakeHttp::new();
        http.live = vec![];
        let out = run_code_query(
            &root,
            &http,
            "http://localhost:9200",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 3, "not-loaded is its own exit code");
        assert!(out.text.contains("This is NOT a 'no match'"));

        // Loaded, zero hits: exit 1 with the verbatim fall-back prose.
        let mut http = FakeHttp::new();
        http.hits = vec![];
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "xyzzy plugh"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 1);
        assert!(!out.is_error, "a miss is an answer, not an MCP error");
        assert!(
            out.text.contains(
                "The corpus is likely wrong for this task — fall back to normal work rather \
                 than forcing a bad match."
            ),
            "verbatim fall-back prose: {}",
            out.text
        );

        // --json with zero hits: still exit 1, empty array on stdout.
        let mut p = CodeParams::new("kv", "xyzzy plugh");
        p.as_json = true;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 1);
        assert!(out.json.is_some());
    }

    #[test]
    fn a_stale_index_is_refused_and_stale_ok_is_the_only_override() {
        let root = root_with_state();
        let old = (chrono::Utc::now() - chrono::Duration::days(31))
            .format(state::STAMP_FORMAT)
            .to_string();
        std::fs::write(
            root.join("state/kv.json"),
            format!(
                "{{\"corpus\":\"kv\",\"indexed_at\":\"{old}\",\"prefix\":\"xc-kv\",\
                 \"autoindex_exit\":0}}"
            ),
        )
        .unwrap();
        let out = run_code_query(
            &root,
            &FakeHttp::new(),
            "u",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 2);
        assert!(out.text.ends_with("or pass `--stale-ok`."));
        assert!(out.is_error);

        let mut p = CodeParams::new("kv", "q");
        p.stale_ok = true;
        let out = run_code_query(&root, &FakeHttp::new(), "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);
    }

    #[test]
    fn hybrid_is_one_native_top_level_request_with_capable_targeting() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } },
            "xc-kv-b1-001": { "mappings": { "properties": { "body": { "type": "text" } } } }
        });
        let mut p = CodeParams::new("kv", "addReplyNull");
        p.mode = Mode::Hybrid;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);

        let reqs = http.searches.lock().unwrap();
        // [0] = BM25 preflight over the wildcard; [1] = the fused request,
        // aimed ONLY at the capable index (the plain-text sibling would 400
        // the whole wildcard).
        assert_eq!(reqs[0].0, "xc-kv-b1*");
        assert_eq!(reqs[1].0, "xc-kv-b1-000");
        let hybrid = &reqs[1].1;
        assert!(
            hybrid.pointer("/query/hybrid").is_some(),
            "ONE native top-level hybrid: {hybrid}"
        );
        assert_eq!(
            hybrid.pointer("/query/hybrid/fusion").unwrap(),
            &serde_json::json!({ "method": "rrf", "k": 60 })
        );
        assert_eq!(
            hybrid
                .pointer("/query/hybrid/queries")
                .unwrap()
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        // #1137: the hybrid leg overfetches like every other mode (10x k=5).
        assert_eq!(hybrid.pointer("/size"), Some(&serde_json::json!(50)));
        // The arms-ran note says BOTH arms and the lexical-only exclusion.
        assert!(
            out.text
                .contains("[hybrid RRF(k=60) — BM25 over 2 index(es), vector over 1 of 2"),
            "{}",
            out.text
        );
        // #1504: the mode line names the default embedder. It must not claim
        // the running node is neural — only what the default is and the flag
        // that changes it.
        assert!(
            out.text.contains("lexical feature-hashing")
                && out.text.contains("--embed-mode neural"),
            "vector mode line must name the default embedder: {}",
            out.text
        );
        assert!(
            out.text.contains("rrf 4.20"),
            "hybrid scores render as rrf: {}",
            out.text
        );
    }

    /// #1504: standalone semantic prints the same embedder default. BM25 does
    /// not — there is no vector leg to misread.
    #[test]
    fn semantic_mode_names_the_default_embedder_and_bm25_does_not() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } },
            "xc-kv-b1-001": { "mappings": { "properties": { "body": { "type": "text" } } } }
        });
        http.live = vec!["xc-kv-b1-000".into(), "xc-kv-b1-001".into()];
        let mut p = CodeParams::new("kv", "addReplyNull");
        p.mode = Mode::Semantic;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0, "{}", out.text);
        assert!(
            out.text.contains("vector only over 1 of 2")
                && out.text.contains("lexical feature-hashing")
                && out.text.contains("--embed-mode neural"),
            "{}",
            out.text
        );

        let bm25 = run_code_query(
            &root,
            &FakeHttp::new(),
            "u",
            &CodeParams::new("kv", "addReplyNull"),
            "`--stale-ok`",
        );
        assert_eq!(bm25.exit, 0, "{}", bm25.text);
        assert!(
            !bm25.text.contains("--embed-mode neural"),
            "bm25 has no vector leg: {}",
            bm25.text
        );
    }

    /// #1504: lang catalogs, Android strings, Apple .strings, gettext, and
    /// minified bundles sort behind an implementing source file. A page that
    /// is only catalogs keeps its order. A PHP service file is not a catalog
    /// just because the repo also has `lang/`.
    #[test]
    fn catalog_and_minified_hits_rank_behind_source() {
        let hit = |path: &str, score: f64| {
            serde_json::json!({
                "_index": "xc-aipbx-000",
                "_score": score,
                "_source": { "ax_path": path, "ax_file": path, "body": "phrase" }
            })
        };
        let hits = vec![
            hit("web/lang/en.php", 9.0),
            hit("assets/jssip.min.js", 8.0),
            hit("res/values-da/strings.xml", 7.0),
            hit("ios/en.lproj/Localizable.strings", 6.0),
            hit("locale/app.po", 5.5),
            hit("locale/app.pot", 5.4),
            hit("android/service/PbxForegroundService.kt", 4.0),
            hit("web/src/services/phones/NecTemplate.php", 3.0),
            hit("docs/phones.md", 2.0),
        ];
        let demoted = demote_catalog_noise(hits);
        let paths: Vec<&str> = demoted
            .iter()
            .map(|h| h["_source"]["ax_path"].as_str().unwrap())
            .collect();
        assert_eq!(
            &paths[..3],
            [
                "android/service/PbxForegroundService.kt",
                "web/src/services/phones/NecTemplate.php",
                "docs/phones.md",
            ]
        );
        assert!(paths[3..].iter().all(|p| is_catalog_noise(p)), "{paths:?}");

        let (page, _, _) = diversify(demoted, 3);
        let page_paths: Vec<&str> = page
            .iter()
            .map(|h| h["_source"]["ax_path"].as_str().unwrap())
            .collect();
        assert_eq!(
            page_paths,
            [
                "android/service/PbxForegroundService.kt",
                "web/src/services/phones/NecTemplate.php",
                "docs/phones.md",
            ]
        );

        let only = demote_catalog_noise(vec![
            hit("web/lang/hr.php", 9.0),
            hit("web/lang/en.php", 8.0),
        ]);
        assert_eq!(only[0]["_source"]["ax_path"], "web/lang/hr.php");
        assert_eq!(only[1]["_source"]["ax_path"], "web/lang/en.php");

        assert!(!is_catalog_noise("src/main.rs"));
        assert!(!is_catalog_noise("res/layout/activity.xml"));
        assert!(is_catalog_noise("web/lang/en.php"));
        assert!(is_catalog_noise("res/values/strings.xml"));
        assert!(is_catalog_noise("assets/app.min.css"));
    }

    /// #1146: with a mixed mapping the fused request may only go to the
    /// semantic_text indices (a semantic leg 400s on plain text), but the
    /// lexical-only indices must still answer the BM25 leg — before this fix
    /// hybrid returned 1 hit where `--mode bm25` returned 20, while the note
    /// claimed "BM25 over 2 index(es)".
    #[test]
    fn hybrid_keeps_lexical_only_indices_in_the_bm25_leg() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } },
            "xc-kv-b1-001": { "mappings": { "properties": { "body": { "type": "text" } } } }
        });
        // Per-index RRF scores as the engine stamps them: 1/(60+rank).
        let hit = |idx: &str, file: &str, score: f64| {
            serde_json::json!({ "_index": idx, "_score": score,
                "_source": { "ax_path": file, "ax_file": file, "body": "retry handler" } })
        };
        http.hits_by_target.insert(
            "xc-kv-b1-000".into(),
            vec![hit("xc-kv-b1-000", "corpus.json", 2.0 / 61.0)],
        );
        http.hits_by_target.insert(
            "xc-kv-b1-001".into(),
            vec![
                hit("xc-kv-b1-001", "m1.py", 1.0 / 61.0),
                hit("xc-kv-b1-001", "m2.py", 1.0 / 62.0),
            ],
        );
        let mut p = CodeParams::new("kv", "retry handler");
        p.mode = Mode::Hybrid;
        p.as_json = true;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);

        let reqs = http.searches.lock().unwrap().clone();
        // [0] preflight, [1] fused over the capable set, [2] the BM25 leg
        // alone over the lexical-only set — still a native `hybrid` so its
        // scores are RRF on the same scale as [1]'s.
        assert_eq!(reqs.len(), 3, "{reqs:?}");
        assert_eq!(reqs[1].0, "xc-kv-b1-000");
        assert_eq!(reqs[2].0, "xc-kv-b1-001");
        let lex = &reqs[2].1;
        let legs = lex
            .pointer("/query/hybrid/queries")
            .and_then(Value::as_array);
        assert_eq!(legs.map(Vec::len), Some(1), "BM25 leg only: {lex}");
        assert!(legs.unwrap()[0].pointer("/query/multi_match").is_some());
        assert_eq!(
            lex.pointer("/query/hybrid/fusion"),
            reqs[1].1.pointer("/query/hybrid/fusion"),
            "same fusion as the capable request"
        );
        assert_eq!(lex.pointer("/size"), Some(&serde_json::json!(50)));

        // Merged by _score descending, the engine's own cross-index order.
        let files: Vec<&str> = out.json.as_ref().unwrap()["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["_source"]["ax_file"].as_str().unwrap())
            .collect();
        assert_eq!(files, ["corpus.json", "m1.py", "m2.py"]);

        // The prose path says which leg ran where.
        p.as_json = false;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert!(
            out.text.contains(
                "BM25 over 2 index(es), vector over 1 of 2; lexical-only (BM25 leg only, \
                 merged by score): xc-kv-b1-001"
            ),
            "{}",
            out.text
        );
    }

    /// #1158: a raw-JSON corpus (ghsa-db advisories) maps none of
    /// body/defs/title/text. The multi_match must go to the corpus's OWN
    /// text-typed fields — the old `["body"]` floor is a field no index
    /// maps, which collapses a multi-token query to zero hits — and the
    /// hit must render a real passage from its longest string field, not
    /// an empty one.
    #[test]
    fn a_raw_json_corpus_queries_its_own_text_fields_and_renders_a_passage() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": {
                "id": { "type": "keyword" },
                "summary": { "type": "text" },
                "details": { "type": "text" },
                "ax_path": { "type": "keyword" }
            }}}
        });
        http.hits = vec![serde_json::json!({
            "_score": 5.1,
            "_source": {
                "ax_path": "ghsa-db/advisories/GHSA-9j.json",
                "id": "GHSA-9j",
                "summary": "lodash command injection",
                "details": "Applications using lodash are vulnerable to command \
                            injection via the template function."
            }
        })];
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "command injection"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 0, "{}", out.text);
        let reqs = http.searches.lock().unwrap().clone();
        assert!(!reqs.is_empty());
        assert_eq!(
            reqs[0].1.pointer("/query/bool/must/0/multi_match/fields"),
            Some(&serde_json::json!(["details", "summary"])),
            "the corpus's own text fields, sorted — not the silent-zero body floor",
        );
        assert!(
            out.text.contains("vulnerable to command injection"),
            "a real passage from the longest string field: {}",
            out.text
        );
    }

    /// #1145: a `semantic` clause without `k` is cut to the parser default
    /// (10), so the vector leg must carry the same window as `size` — else
    /// `--mode semantic -k 20` returns 10 hits and hybrid fuses BM25@50 with
    /// vector@10 (the #1137 overfetch widened the BM25 leg alone).
    #[test]
    fn the_vector_leg_carries_the_fetch_window_as_k() {
        let sem = semantic_on_field("body", "q", 50, &None);
        assert_eq!(
            sem.pointer("/query/semantic/field"),
            Some(&serde_json::json!("body"))
        );
        let on_code = semantic_on_field("code", "q", 50, &None);
        assert_eq!(
            on_code.pointer("/query/semantic/field"),
            Some(&serde_json::json!("code"))
        );
        assert_eq!(sem.pointer("/size"), Some(&serde_json::json!(50)));
        assert_eq!(
            sem.pointer("/query/semantic/k"),
            Some(&serde_json::json!(50))
        );

        let fields = vec!["body".to_string()];
        let hy = hybrid_body("q", 50, &None, &fields);
        assert_eq!(
            hy.pointer("/query/hybrid/queries/1/query/semantic/k"),
            Some(&serde_json::json!(50)),
            "{hy}"
        );
    }

    /// #1148: the engine dispatches a bool to the vector path only when the
    /// semantic clause is its ONE must/should clause (bool.filter is merged
    /// into the semantic filter); a `match` beside it in `must` fell through
    /// to the lexical path and answered 200 with zero hits. `--lang` must
    /// therefore ride in `filter`, on both hybrid legs.
    #[test]
    fn lang_is_a_filter_so_the_semantic_clause_stays_alone_in_must() {
        let lang = Some("rust".to_string());
        let want_filter = serde_json::json!([{ "match": { "language": "rust" } }]);

        let sem = semantic_on_field("body", "q", 50, &lang);
        let must = sem.pointer("/query/bool/must").and_then(Value::as_array);
        assert_eq!(must.map(Vec::len), Some(1), "{sem}");
        assert!(must.unwrap()[0].get("semantic").is_some(), "{sem}");
        assert_eq!(sem.pointer("/query/bool/filter"), Some(&want_filter));

        let fields = vec!["body".to_string()];
        let hy = hybrid_body("q", 50, &lang, &fields);
        for (leg, clause) in [(0, "multi_match"), (1, "semantic")] {
            let q = hy
                .pointer(&format!("/query/hybrid/queries/{leg}/query"))
                .unwrap();
            let must = q.pointer("/bool/must").and_then(Value::as_array);
            assert_eq!(must.map(Vec::len), Some(1), "leg {leg}: {q}");
            assert!(must.unwrap()[0].get(clause).is_some(), "leg {leg}: {q}");
            assert_eq!(q.pointer("/bool/filter"), Some(&want_filter), "leg {leg}");
        }
    }

    #[test]
    fn hybrid_without_a_capable_index_degrades_to_bm25_and_says_so() {
        let root = root_with_state();
        let http = FakeHttp::new(); // mapping has plain text only
        let mut p = CodeParams::new("kv", "q");
        p.mode = Mode::Hybrid;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(out.exit, 0);
        assert!(
            out.text.contains("[BM25 only —"),
            "degradation is SAID: {}",
            out.text
        );
        assert_eq!(
            http.searches.lock().unwrap().len(),
            1,
            "no fused request was sent"
        );
    }

    #[test]
    fn hybrid_reports_an_honest_miss_when_the_bm25_preflight_is_empty() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.mapping = serde_json::json!({
            "xc-kv-b1-000": { "mappings": { "properties": { "body": { "type": "semantic_text" } } } }
        });
        http.hits = vec![]; // preflight AND any vector hit: nothing
        let mut p = CodeParams::new("kv", "xyzzy plugh");
        p.mode = Mode::Hybrid;
        let out = run_code_query(&root, &http, "u", &p, "`--stale-ok`");
        assert_eq!(
            out.exit, 1,
            "vector confidence must not launder a lexical miss"
        );
        assert!(
            out.text.contains("fall back to normal work"),
            "{}",
            out.text
        );
    }

    #[test]
    fn transport_failure_is_exit_2_never_fake_zero_indices() {
        let root = root_with_state();
        let mut http = FakeHttp::new();
        http.fail_search = true;
        let out = run_code_query(
            &root,
            &http,
            "u",
            &CodeParams::new("kv", "q"),
            "`--stale-ok`",
        );
        assert_eq!(out.exit, 2);
        assert!(out.text.contains("search failed"), "{}", out.text);
    }

    /// #1137: one dominant file's adjacent 2 KB chunks must not fill the
    /// whole page. 8 hits from `compatibility` + 3 from other files, k=5:
    /// at most 2 compatibility records survive, the page fills with the
    /// next-ranked files, and the note says the cap ran.
    #[test]
    fn one_file_may_not_fill_the_page() {
        let mk = |file: &str, i: usize| {
            serde_json::json!({
                "_score": 10.0 - i as f64,
                "_source": { "ax_file": file, "ax_path": file,
                             "body": format!("passage {i} of {file}") }
            })
        };
        let mut hits: Vec<Value> = (0..8).map(|i| mk("compatibility", i)).collect();
        hits.push(mk("pagination", 8));
        hits.push(mk("security", 9));
        hits.push(mk("http-headers", 10));

        let (page, file_dropped, index_dropped) = diversify(hits, 5);
        let files: Vec<&str> = page
            .iter()
            .map(|h| {
                h.pointer("/_source/ax_file")
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            files,
            vec![
                "compatibility",
                "compatibility",
                "pagination",
                "security",
                "http-headers"
            ]
        );
        assert_eq!(
            file_dropped, 6,
            "8 compatibility hits -> 2 kept, 6 skipped, rank order kept"
        );
        assert_eq!(
            index_dropped, 0,
            "no _index on these hits: cap never engages"
        );
    }

    /// A page that cannot fill is returned SHORT, not padded back up with the
    /// very neighbours the cap exists to suppress.
    #[test]
    fn a_single_files_wall_yields_a_short_page() {
        let hits: Vec<Value> = (0..10)
            .map(|i| {
                serde_json::json!({
                    "_score": 10.0 - i as f64,
                    "_source": { "ax_file": "spec.md", "body": format!("chunk {i}") }
                })
            })
            .collect();
        let (page, file_dropped, index_dropped) = diversify(hits, 5);
        assert_eq!(page.len(), 2);
        assert_eq!(file_dropped, 8);
        assert_eq!(index_dropped, 0);
    }

    /// Missing provenance is not a reason to drop a hit: hits without
    /// `ax_file` never group together.
    #[test]
    fn hits_without_ax_file_never_group() {
        let hits: Vec<Value> = (0..4)
            .map(|i| serde_json::json!({ "_score": 4.0 - i as f64, "_source": { "body": "b" } }))
            .collect();
        let (page, file_dropped, index_dropped) = diversify(hits, 5);
        assert_eq!(page.len(), 4, "no ax_file -> no cap applies");
        assert_eq!(file_dropped, 0);
        assert_eq!(index_dropped, 0);
    }

    /// 10x k, capped at 200, never below k.
    #[test]
    fn fetch_depth_bounds() {
        assert_eq!(diversity_fetch(5), 50);
        assert_eq!(diversity_fetch(50), 200);
        assert_eq!(diversity_fetch(1), 10);
    }

    /// #1238: one REPO (one `_index` in a corpus group) may not fill the page
    /// when the fan-out has enough indices to fill it without any one of
    /// them — the per-index wall the per-FILE cap cannot see, because every
    /// hit is a different file. Measured shape: the sibling-CVE demo repo
    /// took 6 of the top 10 slots on the exploit group while 36 needle
    /// repos waited below.
    #[test]
    fn one_repo_may_not_fill_the_page_when_the_fanout_is_wide() {
        let mk = |index: &str, i: usize| {
            serde_json::json!({
                "_index": index,
                "_score": 10.0 - i as f64,
                "_source": { "ax_file": format!("{index}/file{i}.py"),
                             "ax_path": format!("{index}/file{i}.py"),
                             "body": format!("passage {i} of {index}") }
            })
        };
        // 6 wall hits from the demo repo, then one hit from each of 5
        // distinct repos — distinct indices (6) >= want (5), so the cap
        // engages and the page is one-per-repo.
        let mut hits: Vec<Value> = (0..6).map(|i| mk("demo-repo", i)).collect();
        for repo in ["needle-a", "needle-b", "needle-c", "needle-d", "needle-e"] {
            hits.push(mk(repo, 6));
        }
        let (page, file_dropped, index_dropped) = diversify(hits, 5);
        let indexes: Vec<&str> = page
            .iter()
            .map(|h| h.pointer("/_index").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(
            indexes,
            vec!["demo-repo", "needle-a", "needle-b", "needle-c", "needle-d"],
            "rank order kept, one slot per repo"
        );
        assert_eq!(index_dropped, 5, "6 demo-repo hits -> 1 kept, 5 skipped");
        assert_eq!(file_dropped, 0, "every hit is a different file");
    }

    /// The capacity guard: a fan-out with FEWER indices than slots must keep
    /// the pre-#1238 behaviour — the per-index cap would only shrink the
    /// page (a homogeneous corpus's handful of shard indices is not a wall).
    #[test]
    fn a_narrow_fanout_keeps_the_per_file_cap_only() {
        let hit = |index: &str, file: &str, i: usize| {
            serde_json::json!({
                "_index": index,
                "_score": 10.0 - i as f64,
                "_source": { "ax_file": file, "ax_path": file,
                             "body": format!("chunk {i}") }
            })
        };
        // 2 distinct indices < want 5: the per-FILE cap still applies
        // (third `file0` neighbour dropped), the per-index cap must not.
        let hits = vec![
            hit("xc-shard-000", "file0.md", 0),
            hit("xc-shard-000", "file0.md", 1),
            hit("xc-shard-000", "file0.md", 2),
            hit("xc-shard-000", "file1.md", 3),
            hit("xc-shard-001", "other0.md", 4),
            hit("xc-shard-001", "other1.md", 5),
        ];
        let (page, file_dropped, index_dropped) = diversify(hits, 5);
        assert_eq!(page.len(), 5, "5 fill from 2 indices without the index cap");
        assert_eq!(
            file_dropped, 1,
            "only the third same-file neighbour is capped"
        );
        assert_eq!(index_dropped, 0, "2 indices < 5 slots: the guard holds");
    }

    /// A hit skipped by the per-index cap must not burn its file's slot: the
    /// per-file counters count EMITTED records only.
    #[test]
    fn an_index_capped_hit_does_not_consume_its_file_slot() {
        let mk = |index: &str, file: &str, i: usize| {
            serde_json::json!({
                "_index": index,
                "_score": 10.0 - i as f64,
                "_source": { "ax_file": file, "ax_path": file,
                             "body": format!("passage {i}") }
            })
        };
        // repo-a slot already spent; this hit is a NEW file in repo-a, so
        // only the index cap skips it — and fileX's slot must stay untouched
        // for the later hit from repo-b/fileX.
        let hits = vec![
            mk("repo-a", "file1", 0),
            mk("repo-a", "fileX", 1),
            mk("repo-b", "fileX", 2),
        ];
        let (page, file_dropped, index_dropped) = diversify(hits, 2);
        assert_eq!(index_dropped, 1, "the repo-a/fileX hit is index-capped");
        assert_eq!(file_dropped, 0);
        let files: Vec<&str> = page
            .iter()
            .map(|h| {
                h.pointer("/_source/ax_file")
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect();
        assert_eq!(files, vec!["file1", "fileX"], "fileX's slot was not burned");
    }

    #[test]
    fn fuse_file_and_symbol_hits_rolls_up_per_file_and_stamps_winning_symbol_span() {
        struct NoopHttp;
        impl XcHttp for NoopHttp {
            fn get_mapping(&self, _: &str) -> Result<Value, String> {
                Err("unused".into())
            }
            fn cat_indices_json(&self, _: &str) -> Result<Vec<String>, String> {
                Ok(vec![])
            }
            fn search(&self, _: &str, _: &Value) -> Result<Value, String> {
                Ok(serde_json::json!({ "hits": { "hits": [] } }))
            }
        }

        // File leg ranks monolith.rs #1 and Target.rs #2; symbol leg ranks two
        // functions in Target.rs at #1 and #2 (must roll up to ONE vote at
        // rank 1, not two votes) and monolith.rs absent from symbol leg.
        // RRF(k=60):
        //   Target.rs   = 1/(60+2) + 1/(60+1) = 1/62 + 1/61 = 0.032522...
        //   monolith.rs = 1/(60+1) + 0        = 1/61        = 0.016393...
        let file_hits = vec![
            serde_json::json!({
                "_index": "xc-test-000",
                "_score": 12.0,
                "_source": {
                    "ax_path": "src/monolith.rs",
                    "ax_file": "src/monolith.rs",
                    "body": "fn big() {}\n",
                    "symbols": [{ "name": "big", "kind": "function", "line": 1 }]
                }
            }),
            serde_json::json!({
                "_index": "xc-test-000",
                "_score": 9.0,
                "_source": {
                    "ax_path": "src/target.rs",
                    "ax_file": "src/target.rs",
                    "body": "fn helper() {\n    let x = 1;\n}\n",
                    "symbols": [{ "name": "helper", "kind": "function", "line": 1 }]
                }
            }),
        ];
        let sym_hits = vec![
            serde_json::json!({
                "_index": "xc-test-000",
                "_score": 18.0,
                "_source": {
                    "ax_path": "src/target.rs",
                    "ax_file": "src/target.rs",
                    "name": "helper",
                    "kind": "function",
                    "line": 1,
                    "end_line": 3
                }
            }),
            serde_json::json!({
                "_index": "xc-test-000",
                "_score": 17.0,
                "_source": {
                    "ax_path": "src/target.rs",
                    "ax_file": "src/target.rs",
                    "name": "second_fn",
                    "kind": "function",
                    "line": 10,
                    "end_line": 15
                }
            }),
        ];
        let fused =
            fuse_file_and_symbol_hits(&NoopHttp, "xc-test*", file_hits, sym_hits, 5, &mut false);
        assert_eq!(fused.len(), 2);
        assert_eq!(
            fused[0].pointer("/_source/ax_path").and_then(Value::as_str),
            Some("src/target.rs")
        );
        let expected_rrf = 1.0 / 62.0 + 1.0 / 61.0;
        let got_score = fused[0].get("_score").and_then(Value::as_f64).unwrap();
        assert!((got_score - expected_rrf).abs() < 1e-12);
        assert_eq!(
            fused[0]
                .pointer("/_source/symbols/0/end_line")
                .and_then(Value::as_i64),
            Some(3),
            "winning symbol end_line stamped onto file record's symbols entry"
        );
    }
}
