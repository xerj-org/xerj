//! Token budgets for MCP search-tool responses (#1058).
//!
//! Six individual search tools share this output budget: `xerj_def`,
//! `xerj_search`, `xerj_hybrid_search`, `xerj_semantic_search`,
//! `xerj_vector_search` and `xerj_code_search`. `max_tokens` covers their
//! successful search payload, including keys and syntax. Errors and the
//! mandatory `xerj_search` staleness hint remain outside that budget.
//! The batch adapter shares the tokenizer and default but preserves every
//! item outcome using its own whole-response budgeting algorithm.
//!
//! ## The one tokenizer
//!
//! Tokens are counted with **XERJ's `StandardTokenizer`** — UAX #29 word
//! boundaries, `unicode_words()` — the same word split the engine's own
//! analyzer uses for BM25 analysis
//! (`engine/crates/xerj-fts/src/analyzer.rs`, `StandardTokenizer::tokenize`).
//! This crate calls `unicode-segmentation` directly rather than depending on
//! the whole FTS stack: `StandardTokenizer` *is* `input.unicode_words()`, so
//! the count is identical by construction (same crate, same function)
//! without dragging FST/roaring/zstd into a thin proxy. Honesty, stated in
//! the tool description too: UAX #29 words approximate model context tokens
//! and under-count subword splits (`snake_case_name` is ONE word here,
//! several BPE pieces); the budget is enforced in *these* tokens, the only
//! count that is deterministic and available offline.
//!
//! ## Enforcement model (both surfaces)
//!
//! 1. **Dedup before budgeting**: hits that resolve to an identifiable
//!    `file:line` span and *overlap* a higher-ranked hit whose passage is
//!    actually shown are dropped entirely (counted in the accounting block,
//!    never re-shown). Rendered code has no reliable source ranges: only
//!    identical locators with identical displayed bodies are deduped there.
//! 2. **Greedy keep in rank order** until the budget would break.
//! 3. **Over-budget hits become locators only** — the citation `file:line` /
//!    `_index`+`_id`, not the passage — while they still fit.
//! 4. Anything that cannot fit even as a locator is counted as dropped.
//!
//! Nothing binding (budget not reached, nothing to dedup) returns the engine
//! text byte-identical. The invariant the issue demands — *the response
//! never exceeds the budget* — is guaranteed by construction (a note reserve
//! is subtracted before any content is admitted) and re-checked as a final
//! guard; the tests pin it for every tool across a range of budgets. Errors
//! are NOT budgeted: a refusal the agent cannot read end-to-end is worse
//! than a long response.

use serde_json::{json, Value};
use unicode_segmentation::UnicodeSegmentation;

/// How the budget counts tokens, named in tool descriptions so an agent knows
/// what a "token" is before spending one. Kept as a constant so the shared
/// descriptions cannot drift.
pub const TOKENIZER_NAME: &str = "XERJ's StandardTokenizer (UAX #29 word boundaries)";

/// The short name for accounting blocks (`standard-uax29-words` is what
/// `_token_budget.tokenizer` reports).
pub const TOKENIZER_ID: &str = "standard-uax29-words";

/// Smallest accepted `max_tokens`. Below this not even the response envelope
/// (header + accounting note) fits, so the tool refuses rather than return a
/// budget it cannot honor.
pub const MIN_MAX_TOKENS: usize = 32;

/// Budget applied when the caller passes no `max_tokens` (#2491).
///
/// Six individual search tools, the batch tool and `xerj batch-search` apply
/// this UAX #29 word budget to successful output. Long identifiers and
/// punctuation can occupy many bytes while counting as few words: this is
/// neither a byte cap nor a model-context/BPE limit. Callers may request any
/// value at or above [`MIN_MAX_TOKENS`]; errors remain untruncated.
pub const DEFAULT_MAX_TOKENS: usize = 8_000;

/// Tokens reserved for the `_token_budget` accounting block before any hit is
/// admitted. The block's fixed fields (keys + counts) measure ~14 word tokens;
/// locator stubs inside it are charged against the content budget separately,
/// so 32 leaves margin without padding the response.
const NOTE_RESERVE: usize = 32;

/// What inserting `"tokens_used":<n>` adds to the serialized JSON, in word
/// tokens: the key is one word, the number is one. (serde_json's default map
/// is sorted, so insertion order cannot change the count.) Lets the
/// accounting report its own final size without a circular measurement.
const TOKENS_USED_INSERT: usize = 2;

/// Marker appended by the hard truncation fallback.
const TRUNCATION_MARKER: &str = "\n[truncated at the max_tokens budget]";

// ────────────────────────────── argument ────────────────────────────────────

/// Read the optional `max_tokens` argument. Present-but-mistyped or below the
/// floor is an error, never a silent drop — the `opt_typed` policy: an agent
/// that asked for a budget and silently got none would trust a response whose
/// size it cannot predict.
///
/// Absent or null means [`DEFAULT_MAX_TOKENS`] (#2491). The `Option` shape is
/// retained for internal response paths that bypass budgeting explicitly.
pub fn opt_max_tokens(args: &Value) -> Result<Option<usize>, String> {
    match args.get("max_tokens") {
        None | Some(Value::Null) => Ok(Some(DEFAULT_MAX_TOKENS)),
        Some(v) => match v.as_u64() {
            Some(n) if n as usize >= MIN_MAX_TOKENS => Ok(Some(n as usize)),
            _ => Err(format!(
                "`max_tokens` must be an integer >= {MIN_MAX_TOKENS} (tokens are counted as \
                 UAX #29 words by XERJ's StandardTokenizer — see the argument's description)"
            )),
        },
    }
}

// ────────────────────────────── counting ────────────────────────────────────

/// Count tokens the way the budget counts them: UAX #29 words, punctuation
/// and whitespace dropped — `StandardTokenizer::tokenize` without the Token
/// structs.
pub fn count_tokens(text: &str) -> usize {
    text.unicode_words().count()
}

/// Hard floor: cut at a word boundary so the result fits `max_tokens`,
/// marking the cut when there is room for the marker. Every path in this
/// module funnels through either a budgeted assembly (which reserves
/// headroom) or this function, so the never-exceed invariant holds even for
/// shapes the budgeter was not built for (non-JSON bodies,
/// aggregation-only responses).
pub fn truncate_to_budget(text: &str, max_tokens: usize) -> String {
    if count_tokens(text) <= max_tokens {
        return text.to_string();
    }
    let marker_tokens = count_tokens(TRUNCATION_MARKER);
    let (allowed, with_marker) = if max_tokens > marker_tokens {
        (max_tokens - marker_tokens, true)
    } else {
        (max_tokens, false)
    };
    // Byte end of the last word that still fits (same offset trick
    // StandardTokenizer uses: unicode_words yields subslices of `text`).
    let mut kept_end = 0usize;
    for (i, word) in text.unicode_words().enumerate() {
        if i >= allowed {
            break;
        }
        kept_end = word.as_ptr() as usize + word.len() - text.as_ptr() as usize;
    }
    let mut out = text[..kept_end].to_string();
    if with_marker {
        out.push_str(TRUNCATION_MARKER);
    }
    out
}

// ───────────────────── JSON surface (the four `_search` tools) ─────────────

/// A relative path is meaningful only within its source index. Content IDs
/// distinguish versions at the same path; record locators distinguish rows
/// whose passage ordinals are local to each record, not the containing file.
#[derive(PartialEq)]
struct HitSource {
    index: Option<String>,
    file: String,
    content_id: Option<String>,
    record: Option<String>,
}

type HitSpan = (HitSource, u64, u64);

/// A hit's inclusive line span within one source. Mirrors the provenance
/// path order (`ax_path`, `ax_file`, `path`, `file`; `start_line`/`line`), then
/// falls back to the engine's `_passage` block. Its `ordinal` is relative to
/// the record, so those spans are compared only within the same locator.
/// `end` is `end_line` when present, else the start plus
/// the shown passage's own line count — an estimate, and labelled as one
/// wherever it is described: it exists to catch *overlap*, not to cite.
fn hit_span(hit: &Value) -> Option<HitSpan> {
    let src = hit.get("_source")?;
    let file = ["ax_path", "ax_file", "path", "file"]
        .iter()
        .find_map(|k| src.get(*k).and_then(Value::as_str))?
        .to_string();
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    let record = src
        .get("ax_locator")
        .and_then(Value::as_str)
        // Whole-file code and its symbol records share file-global lines.
        // Other locators (rows, pages, sections, ...) are independent spaces.
        .filter(|loc| {
            !loc.is_empty() && *loc != "file" && *loc != "code" && !loc.starts_with("code:")
        })
        .map(str::to_owned);
    let source = HitSource {
        index: text(hit.get("_index")),
        file,
        content_id: text(src.get("ax_file")),
        record,
    };

    let start = src
        .get("start_line")
        .or_else(|| src.get("line"))
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .or_else(|| {
            hit.pointer("/fields/_passage/0")?
                .get("ordinal")
                .and_then(Value::as_u64)
                .map(|o| o + 1)
        })?;
    let mut end = src
        .get("end_line")
        .and_then(Value::as_u64)
        .unwrap_or(start)
        .max(start);
    if let Some(lines) = passage_line_count(hit) {
        end = end.max(start + lines);
    }
    Some((source, start, end))
}

/// Lines in the hit's shown `_passage` text, when it has one.
fn passage_line_count(hit: &Value) -> Option<u64> {
    Some(
        hit.pointer("/fields/_passage/0")?
            .get("text")
            .and_then(Value::as_str)
            .map(|t| t.matches('\n').count() as u64)
            .unwrap_or(0),
    )
}

fn spans_overlap<T: PartialEq>(a: &(T, u64, u64), b: &(T, u64, u64)) -> bool {
    a.0 == b.0 && a.1 <= b.2 && b.1 <= a.2
}

/// The locator-only form of a hit: index/id plus file:line when present.
/// Deliberately terse — locators are what survives a tight budget, so they
/// are the cheapest thing we emit.
fn locator_stub(hit: &Value) -> Value {
    let mut stub = serde_json::Map::new();
    for k in ["_index", "_id"] {
        if let Some(v) = hit.get(k).filter(|v| !v.is_null()) {
            stub.insert(k.to_string(), v.clone());
        }
    }
    if let Some(src) = hit.get("_source") {
        for k in [
            "ax_path",
            "ax_file",
            "ax_locator",
            "path",
            "start_line",
            "line",
        ] {
            if let Some(v) = src.get(k).filter(|v| !v.is_null()) {
                if v.is_string() || v.is_number() {
                    stub.insert(k.to_string(), v.clone());
                }
            }
        }
    }
    // #2266: the citation line is computed on the hit, not stored. A locator
    // that drops it puts the agent back to counting lines in a passage the
    // budget just removed.
    for k in ["match_line", "match_lines"] {
        if let Some(v) = hit.get(k).filter(|v| !v.is_null()) {
            stub.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(stub)
}

/// Budget an engine `_search` JSON response to `max_tokens` tokens. Returns
/// text (never a JSON error): parses, dedupes overlapping file:line passages
/// against passages actually shown, keeps hits greedily in rank order,
/// demotes the rest to locator stubs, and appends a `_token_budget`
/// accounting block. Nothing binding → the engine text verbatim. Non-JSON
/// bodies, JSON without a `hits.hits` array (aggregation-only), or an
/// envelope that alone cannot fit fall back to [`truncate_to_budget`] — the
/// invariant outranks pretty truncation.
pub fn budget_json_response(engine_text: &str, max_tokens: usize) -> String {
    let Ok(resp) = serde_json::from_str::<Value>(engine_text) else {
        return truncate_to_budget(engine_text, max_tokens);
    };
    let Some(hits) = resp
        .get("hits")
        .and_then(|h| h.get("hits"))
        .and_then(Value::as_array)
        .cloned()
    else {
        // Aggregation-only / ack shapes: nothing to demote by hit, so cut at
        // a word boundary instead.
        if count_tokens(engine_text) <= max_tokens {
            return engine_text.to_string();
        }
        return truncate_to_budget(engine_text, max_tokens);
    };
    if count_tokens(engine_text) <= max_tokens && !any_overlaps(&hits) {
        return engine_text.to_string();
    }

    // The envelope with an empty hit list is the unavoidable cost; it is
    // admitted first, then hits fill what is left.
    let mut header = resp.clone();
    if let Some(arr) = header
        .get_mut("hits")
        .and_then(|h| h.get_mut("hits"))
        .and_then(Value::as_array_mut)
    {
        arr.clear();
    }
    let header_tokens = count_tokens(&compact(&header));
    if header_tokens.saturating_add(NOTE_RESERVE) >= max_tokens {
        // Even an empty response cannot fit. Drop the hit list entirely; if
        // that is still not enough, word-truncate.
        let mut bare = header;
        if let Some(hits_obj) = bare.get_mut("hits").and_then(Value::as_object_mut) {
            hits_obj.remove("hits");
        }
        return truncate_to_budget(&compact(&bare), max_tokens);
    }
    let mut remaining = max_tokens - header_tokens - NOTE_RESERVE;

    // 1. Dedup: a hit overlapping the span of an already-SHOWN passage is
    //    skipped entirely. Locator-only hits show no passage, so they claim
    //    no span — a later overlap through them is not a duplicate read.
    let mut kept_spans: Vec<HitSpan> = Vec::new();
    let mut deduped = 0usize;
    let mut full: Vec<Value> = Vec::new();
    let mut locators: Vec<Value> = Vec::new();
    let mut dropped = 0usize;
    for hit in &hits {
        let span = hit_span(hit);
        if span
            .as_ref()
            .is_some_and(|s| kept_spans.iter().any(|k| spans_overlap(k, s)))
        {
            deduped += 1;
            continue;
        }
        let hit_tokens = count_tokens(&compact(hit));
        if hit_tokens <= remaining {
            full.push(hit.clone());
            if let Some(s) = span {
                kept_spans.push(s);
            }
            remaining -= hit_tokens;
        } else {
            let stub = locator_stub(hit);
            let stub_tokens = count_tokens(&compact(&stub));
            if stub_tokens <= remaining {
                locators.push(stub);
                remaining -= stub_tokens;
            } else {
                dropped += 1;
            }
        }
    }

    // 2. Assemble, measure once, then report our own final size (the insert
    //    costs a fixed TOKENS_USED_INSERT, covered by NOTE_RESERVE).
    let mut out = header;
    if let Some(arr) = out
        .get_mut("hits")
        .and_then(|h| h.get_mut("hits"))
        .and_then(Value::as_array_mut)
    {
        *arr = full;
    }
    let accounting = json!({
        "tokenizer": TOKENIZER_ID,
        "max_tokens": max_tokens,
        "hits_full": out.pointer("/hits/hits").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
        "hits_locator_only": locators,
        "deduped_overlapping": deduped,
        "hits_dropped": dropped,
    });
    if let Some(obj) = out.as_object_mut() {
        obj.insert("_token_budget".to_string(), accounting);
    }
    let measured = count_tokens(&compact(&out));
    if let Some(obj) = out.as_object_mut() {
        obj.get_mut("_token_budget")
            .and_then(Value::as_object_mut)
            .expect("just inserted")
            .insert(
                "tokens_used".to_string(),
                json!(measured + TOKENS_USED_INSERT),
            );
    }
    let final_text = compact(&out);
    // 3. The guarantee. Reserve accounting should make this unreachable; if
    //    a future edit breaks that, the marker is still honest.
    if count_tokens(&final_text) > max_tokens {
        return truncate_to_budget(&final_text, max_tokens);
    }
    final_text
}

/// Would any pair of hits in this list overlap? Used only for the
/// nothing-binding early exit, where every hit is shown, so pairwise overlap
/// among all of them is exactly the dedup condition.
fn any_overlaps(hits: &[Value]) -> bool {
    let mut spans: Vec<HitSpan> = Vec::new();
    for hit in hits {
        if let Some(s) = hit_span(hit) {
            if spans.iter().any(|k| spans_overlap(k, &s)) {
                return true;
            }
            spans.push(s);
        }
    }
    false
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

// ──────────────────── rendered-text surface (`xerj_code_search`) ────────────

/// One rendered hit block from the shared xccode renderer: the `─── ` header
/// line plus its passage body, bounded by the next header or the footer.
struct CodeBlock<'a> {
    lines: &'a [&'a str],
}

impl<'a> CodeBlock<'a> {
    fn header(&self) -> &'a str {
        self.lines[0]
    }

    /// A rendered citation is not a source range: it can point at the match
    /// inside a window, and labels/separators are not source lines. Only an
    /// identical locator AND displayed body prove a redundant read here.
    fn duplicates(&self, other: &Self) -> bool {
        let locator = |h: &'a str| h.split("  (").next().unwrap_or(h);
        let body = |lines: &'a [&'a str]| {
            let mut end = lines.len();
            // The renderer separates hits and the footer with blank lines.
            while end > 1 && lines[end - 1].is_empty() {
                end -= 1;
            }
            &lines[1..end]
        };
        let passage = body(self.lines);
        !passage.is_empty()
            && locator(self.header()) == locator(other.header())
            && passage == body(other.lines)
    }
}

/// Budget the shared renderer's text (`xerj_code_search`'s payload) to
/// `max_tokens` tokens. Same model as the JSON surface, adapted to the
/// renderer's shape: blocks start at `─── ` lines; the footer is the
/// `N passages from '…'` line and everything after it. Deduped blocks are
/// removed entirely only when locator and displayed body are identical;
/// over-budget blocks keep only their `─── file:line
/// (score …)` header — the locator the footer tells the agent to cite — with
/// a visible omission marker; the trailing `[token budget …]` line accounts
/// for every hit. Nothing to dedup and under budget → byte-identical text.
pub fn budget_code_text(rendered: &str, max_tokens: usize) -> String {
    let lines: Vec<&str> = rendered.lines().collect();
    let Some(first_hit) = lines.iter().position(|l| l.starts_with("─── ")) else {
        // No hit blocks (no-match text, warnings only) — still never exceed.
        return truncate_to_budget(rendered, max_tokens);
    };
    let footer_start = (first_hit..lines.len())
        .find(|&i| lines[i].contains(" passages from '"))
        .unwrap_or(lines.len());

    // Segment into blocks.
    let mut blocks: Vec<CodeBlock> = Vec::new();
    let mut i = first_hit;
    while i < footer_start {
        if lines[i].starts_with("─── ") {
            let start = i;
            i += 1;
            while i < footer_start && !lines[i].starts_with("─── ") {
                i += 1;
            }
            blocks.push(CodeBlock {
                lines: &lines[start..i],
            });
        } else {
            i += 1;
        }
    }

    let preamble = lines[..first_hit].join("\n");
    let footer = lines[footer_start..].join("\n");
    // Nothing binding → byte-identical passthrough (the leading blank line
    // lives in the preamble, the footer is untouched).
    let has_duplicates = blocks
        .iter()
        .enumerate()
        .any(|(i, block)| blocks[..i].iter().any(|earlier| block.duplicates(earlier)));
    if !has_duplicates && count_tokens(rendered) <= max_tokens {
        return rendered.to_string();
    }

    let fixed_tokens = count_tokens(&preamble) + count_tokens(&footer);
    // The trailing accounting line rides after the footer.
    if fixed_tokens.saturating_add(NOTE_RESERVE) >= max_tokens {
        return truncate_to_budget(rendered, max_tokens);
    }
    let mut remaining = max_tokens - fixed_tokens - NOTE_RESERVE;

    let mut out_lines: Vec<String> = vec![preamble];
    let mut deduped = 0usize;
    let mut locator_only = 0usize;
    let mut dropped = 0usize;
    let mut kept_blocks: Vec<&CodeBlock> = Vec::new();
    for block in &blocks {
        if kept_blocks.iter().any(|kept| block.duplicates(kept)) {
            deduped += 1;
            continue;
        }
        let block_text = block.lines.join("\n");
        let block_tokens = count_tokens(&block_text);
        if block_tokens <= remaining {
            out_lines.push(block_text);
            kept_blocks.push(block);
            remaining -= block_tokens;
        } else {
            let omission = format!(
                "    [passage omitted: over the max_tokens {max_tokens} budget — locator only]"
            );
            let loc_tokens = count_tokens(block.header()) + count_tokens(&omission);
            if loc_tokens <= remaining {
                out_lines.push(block.header().to_string());
                out_lines.push(omission);
                locator_only += 1;
                remaining -= loc_tokens;
            } else {
                dropped += 1;
            }
        }
    }
    let shown_full = blocks.len() - deduped - locator_only - dropped;
    if !footer.is_empty() {
        out_lines.push(footer);
    }
    out_lines.push(format!(
        "[token budget: {shown_full}/{} passages full, {locator_only} locator-only, \
         {deduped} deduped (identical locator and passage), {dropped} dropped; counted as \
         {TOKENIZER_ID}, max_tokens {max_tokens}]",
        blocks.len(),
    ));
    let final_text = format!("{}\n", out_lines.join("\n"));
    if count_tokens(&final_text) > max_tokens {
        return truncate_to_budget(&final_text, max_tokens);
    }
    final_text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The issue's hard rule: the response NEVER exceeds the budget. Run a
    /// payload through a budgeter at many budgets — including ones that bind
    /// mid-hit — and pin the invariant each time, not once.
    fn assert_never_exceeds(text: &str, budgets: &[usize], json_surface: bool) {
        for &n in budgets {
            let out = if json_surface {
                budget_json_response(text, n)
            } else {
                budget_code_text(text, n)
            };
            assert!(
                count_tokens(&out) <= n,
                "budget {n}: response used {} tokens\n---\n{out}\n---",
                count_tokens(&out)
            );
        }
    }

    fn search_response(hits: Vec<Value>) -> String {
        compact(&json!({
            "took": 12,
            "timed_out": false,
            "_shards": { "total": 1, "successful": 1, "skipped": 0, "failed": 0 },
            "hits": {
                "total": { "value": hits.len(), "relation": "eq" },
                "max_score": 12.34,
                "hits": hits
            }
        }))
    }

    fn code_hit(path: &str, line: u64, body_lines: usize) -> Value {
        let body: Vec<String> = (0..body_lines)
            .map(|i| format!("    let x{i} = compute_{i}();"))
            .collect();
        json!({
            "_index": "ax-code",
            "_id": format!("hit-{path}-{line}"),
            "_score": 10.0 - line as f64 / 100.0,
            "_source": {
                "ax_path": path,
                "line": line,
                "language": "rust",
                "kind": "function",
                "body": body.join("\n")
            },
            "fields": { "_passage": [ { "field": "body", "ordinal": line - 1, "text": body.join("\n") } ] }
        })
    }

    // ── the tokenizer ──────────────────────────────────────────────────────

    #[test]
    fn count_tokens_is_uax29_words() {
        // Same function StandardTokenizer uses (unicode_words): punctuation
        // and whitespace dropped, snake_case is one word.
        assert_eq!(count_tokens("hello, world!"), 2);
        assert_eq!(count_tokens("snake_case_name"), 1);
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("max_tokens\": 12345, \"hits_full"), 3);
    }

    #[test]
    fn truncate_to_budget_never_exceeds_and_marks_the_cut() {
        let text = "word ".repeat(500);
        for n in [1usize, 5, 6, 7, 32, 100] {
            let out = truncate_to_budget(&text, n);
            assert!(count_tokens(&out) <= n, "budget {n}: {out}");
            if n > count_tokens(TRUNCATION_MARKER) {
                assert!(out.contains("truncated at the max_tokens budget"));
            }
        }
        // Already fitting: byte-identical, no marker.
        let small = "only a few words";
        assert_eq!(truncate_to_budget(small, 100), small);
    }

    // ── argument parsing ───────────────────────────────────────────────────

    #[test]
    fn opt_max_tokens_validates() {
        assert_eq!(
            opt_max_tokens(&json!({})),
            Ok(Some(DEFAULT_MAX_TOKENS)),
            "an omitted budget must fall back to the default, not to unbounded"
        );
        assert_eq!(
            opt_max_tokens(&json!({ "max_tokens": null })),
            Ok(Some(DEFAULT_MAX_TOKENS))
        );
        assert_eq!(opt_max_tokens(&json!({ "max_tokens": 500 })), Ok(Some(500)));
        for bad in [
            json!({ "max_tokens": 0 }),
            json!({ "max_tokens": 31 }),
            json!({ "max_tokens": "500" }),
            json!({ "max_tokens": 4.5 }),
            json!({ "max_tokens": true }),
        ] {
            assert!(opt_max_tokens(&bad).is_err(), "should reject {bad}");
            let msg = opt_max_tokens(&bad).unwrap_err();
            assert!(msg.contains(">= 32"), "the floor must be named: {msg}");
        }
    }

    /// #2491: a search that passes no `max_tokens` used to return whatever the
    /// engine produced, and a response past the host's tool-output limit was
    /// spilled to a temp file instead of shown to the agent. Absent now means
    /// `DEFAULT_MAX_TOKENS`, so the same call stays within this word budget.
    #[test]
    fn an_unbudgeted_search_response_is_bounded_by_default() {
        // A response larger than the default word budget: 400 hits
        // carrying 400 lines of body each.
        let resp = search_response(
            (1..=400)
                .map(|i| code_hit(&format!("repo/src/module_{i}.rs"), 10 * i, 400))
                .collect(),
        );
        let unbounded = count_tokens(&resp);
        assert!(
            unbounded > DEFAULT_MAX_TOKENS,
            "the fixture must be big enough to need the default cap: {unbounded}"
        );

        let budget = opt_max_tokens(&json!({}))
            .expect("absent max_tokens is not an error")
            .expect("absent max_tokens must apply the default budget");
        assert_eq!(budget, DEFAULT_MAX_TOKENS);

        let out = budget_json_response(&resp, budget);
        let used = count_tokens(&out);
        assert!(
            used <= DEFAULT_MAX_TOKENS,
            "default budget {DEFAULT_MAX_TOKENS} exceeded: {used}\n---\n{out}\n---"
        );
        // Bounded by dropping content, not by dropping the answer: the
        // accounting block has to say what went, or the agent cannot tell a
        // short corpus from a truncated one.
        assert!(
            out.contains("_token_budget"),
            "the response must account for what the budget cut:\n{out}"
        );
    }

    #[test]
    fn word_budget_does_not_claim_a_byte_limit() {
        let resp = search_response(vec![json!({
            "_index": "docs", "_id": "long", "_source": {
                "body": "x".repeat(100_000)
            }
        })]);
        assert!(resp.len() > 100_000);
        assert!(count_tokens(&resp) < DEFAULT_MAX_TOKENS);
        assert_eq!(budget_json_response(&resp, DEFAULT_MAX_TOKENS), resp);
    }

    // ── JSON surface: per-tool budget enforcement ──────────────────────────

    /// xerj_search shape: definition-first hits over an autoindex corpus,
    /// `_source` carrying ax_path/line and a `fields._passage` projection.
    #[test]
    fn search_budget_holds_and_demotes_to_locators() {
        let resp = search_response(
            (1..=12)
                .map(|i| code_hit(&format!("repo/src/f{i}.rs"), 10 * i, 8))
                .collect(),
        );
        assert_never_exceeds(&resp, &[32, 48, 64, 120, 200, 400, 100_000], true);

        let out = budget_json_response(&resp, 300);
        let v: Value = serde_json::from_str(&out).expect("still one JSON document");
        let tb = &v["_token_budget"];
        assert_eq!(tb["max_tokens"], 300, "the budget is self-describing");
        // The tight regime: a budget too small for ANY full passage still
        // returns locators rather than nothing.
        let tight: Value = serde_json::from_str(&budget_json_response(&resp, 96)).unwrap();
        assert_eq!(tight["_token_budget"]["hits_full"], 0);
        assert!(
            !tight["_token_budget"]["hits_locator_only"]
                .as_array()
                .unwrap()
                .is_empty(),
            "tight budget → locator-only, not silence: {}",
            tight["_token_budget"]
        );
        assert_eq!(tb["tokenizer"], TOKENIZER_ID);
        let full = tb["hits_full"].as_u64().unwrap();
        assert!(
            full > 0 && full < 12,
            "binding budget keeps some, not all: {full}"
        );
        assert!(
            !tb["hits_locator_only"].as_array().unwrap().is_empty(),
            "the first over-budget hits must still be locators, not dropped"
        );
        for stub in tb["hits_locator_only"].as_array().unwrap() {
            assert!(
                stub.get("_index").is_some() && stub.get("_id").is_some(),
                "{stub}"
            );
            assert!(stub.get("body").is_none(), "a locator carries no passage");
        }
        // The kept hits are the TOP of the ranking, in order.
        let kept: Vec<&str> = v["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["_id"].as_str().unwrap())
            .collect();
        assert_eq!(kept.len() as u64, full);
        let all: Vec<String> = serde_json::from_str::<Value>(&resp).unwrap()["hits"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kept,
            all[..kept.len()]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            "hits are admitted in rank order"
        );
        assert_eq!(
            v["_token_budget"]["deduped_overlapping"], 0,
            "distinct files"
        );
    }

    /// #2266: `match_line` is how an agent cites the hit. A budget that keeps
    /// the passage keeps the field; a budget that reduces the hit to a
    /// locator keeps it there too.
    #[test]
    fn match_line_survives_token_budget_trimming() {
        let passage = "intro\nslot filling binds the token\noutro";
        // Fat stored body so the full hit cannot fit a locator-sized budget.
        // The stub copies match_line and drops the body.
        let body = format!("{passage}\n{}", "filler word ".repeat(400));
        let hit = json!({
            "_index": "docs",
            "_id": "guide",
            "_score": 3.0,
            "_source": {
                "ax_path": "guide.md",
                "start_line": 169,
                "body": body
            },
            "match_line": 170,
            "match_lines": [170],
            "fields": { "_passage": [{ "field": "body", "ordinal": 0, "text": passage }] }
        });
        let resp = search_response(vec![hit.clone()]);

        let kept: Value = serde_json::from_str(&budget_json_response(&resp, 100_000)).unwrap();
        assert_eq!(kept["hits"]["hits"][0]["match_line"], 170);
        assert_eq!(kept["hits"]["hits"][0]["match_lines"], json!([170]));

        let tight: Value = serde_json::from_str(&budget_json_response(&resp, 200)).unwrap();
        assert_eq!(tight["_token_budget"]["hits_full"], 0, "{tight}");
        let stubs = tight["_token_budget"]["hits_locator_only"]
            .as_array()
            .unwrap();
        assert_eq!(stubs.len(), 1, "{stubs:?}");
        assert_eq!(stubs[0]["match_line"], 170, "{stubs:?}");
        assert_eq!(stubs[0]["match_lines"], json!([170]));
        assert_eq!(stubs[0]["ax_path"], "guide.md");
        assert_eq!(stubs[0]["start_line"], 169);
        assert!(stubs[0].get("body").is_none(), "locator drops the passage");
        assert!(
            stubs[0].get("fields").is_none(),
            "locator drops the passage"
        );
    }

    /// xerj_semantic_search / xerj_vector_search shape: hits whose `_source`
    /// is a prose chunk (no file/line), so no span, no dedup — pure budget.
    #[test]
    fn prose_budget_holds_without_spans() {
        let hits: Vec<Value> = (0..10)
            .map(|i| {
                json!({
                    "_index": "kb", "_id": format!("doc-{i}"),
                    "_score": 0.9 - i as f64 / 100.0,
                    "_source": { "title": format!("doc {i}"), "text": "answer ".repeat(60) }
                })
            })
            .collect();
        let resp = search_response(hits);
        assert_never_exceeds(&resp, &[32, 64, 128, 512, 100_000], true);

        let out = budget_json_response(&resp, 128);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["_token_budget"]["deduped_overlapping"], 0,
            "no spans → no dedup"
        );
        assert!(v["_token_budget"]["hits_full"].as_u64().unwrap() < 10);
    }

    /// xerj_hybrid_search shape: fused hits from several sub-queries can
    /// return the SAME passage twice via different legs — the dedup case.
    #[test]
    fn overlapping_passages_are_deduped_before_budgeting() {
        let dup = code_hit("repo/src/same.rs", 100, 6);
        let mut hits = vec![dup, code_hit("repo/src/other.rs", 55, 6)];
        // Same file, start line INSIDE the first hit's shown passage.
        hits.push(json!({
            "_index": "ax-code", "_id": "hit-overlap", "_score": 9.0,
            "_source": { "ax_path": "repo/src/same.rs", "start_line": 102, "end_line": 104, "body": "overlap" }
        }));
        let resp = search_response(hits);

        // Budget deliberately NOT binding: dedup happens regardless.
        let out = budget_json_response(&resp, 100_000);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["_token_budget"]["deduped_overlapping"], 1);
        assert_eq!(
            v["hits"]["hits"].as_array().unwrap().len(),
            2,
            "the duplicate is not re-shown"
        );
        assert_never_exceeds(&resp, &[32, 64, 128, 100_000], true);
    }

    /// The `_passage`-only shape (no explicit line field): ordinal + text
    /// line count still yields a span, so the same passage returned twice
    /// dedupes.
    #[test]
    fn passage_ordinal_spans_dedupe_too() {
        let mk = |id: &str| {
            json!({
                "_index": "ax-code", "_id": id, "_score": 5.0,
                "_source": { "ax_path": "repo/lib.rs" },
                "fields": { "_passage": [ { "field": "body", "ordinal": 41,
                    "text": "line one\nline two\nline three" } ] }
            })
        };
        let resp = search_response(vec![mk("a"), mk("b")]);
        let out = budget_json_response(&resp, 100_000);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["_token_budget"]["deduped_overlapping"], 1,
            "same ordinal = same passage"
        );
    }

    #[test]
    fn json_budget_keeps_distinct_record_locators() {
        for (first, second) in [
            ("b0", "b100"),
            ("r1", "r2"),
            ("Sheet1!r1", "Sheet1!r2"),
            ("items:e0", "items:e1"),
            ("d0", "d1"),
        ] {
            let row = |id: &str, locator: &str| {
                json!({
                    "_index": "ax-data", "_id": id,
                    "_source": {"ax_path": "records.jsonl", "ax_file": "file-hash", "ax_locator": locator},
                    "fields": {"_passage": [{"field": "body", "ordinal": 0, "text": "independent record"}]}
                })
            };
            let resp = search_response(vec![row("1", first), row("2", second)]);
            assert_eq!(
                budget_json_response(&resp, 10_000),
                resp,
                "{first}, {second}"
            );
            assert_never_exceeds(&resp, &[32, 64, 128, 256, 10_000], true);
            let duplicate = search_response(vec![row("1", first), row("2", first)]);
            let out: Value =
                serde_json::from_str(&budget_json_response(&duplicate, 10_000)).unwrap();
            assert_eq!(out["_token_budget"]["deduped_overlapping"], 1);
        }
    }

    #[test]
    fn json_budget_scopes_relative_paths_to_their_source() {
        let first = code_hit("src/lib.rs", 10, 4);
        for field in ["_index", "ax_file"] {
            let mut a = first.clone();
            let mut b = first.clone();
            b["_id"] = json!("second");
            if field == "_index" {
                b[field] = json!("other-code");
            } else {
                a["_source"][field] = json!("content-a");
                b["_source"][field] = json!("content-b");
            }
            let resp = search_response(vec![a, b]);
            assert_eq!(budget_json_response(&resp, 10_000), resp, "{field}");
            assert_never_exceeds(&resp, &[32, 64, 128, 256, 10_000], true);
        }
    }

    #[test]
    fn json_budget_dedupes_file_and_symbol_records_in_the_same_line_space() {
        let mut file = code_hit("src/lib.rs", 10, 4);
        file["_source"]["ax_file"] = json!("content-a");
        file["_source"]["ax_locator"] = json!("code");
        let mut symbol = file.clone();
        symbol["_id"] = json!("symbol");
        symbol["_source"]["ax_locator"] = json!("code:10:example");
        let resp = search_response(vec![file, symbol]);
        let out: Value = serde_json::from_str(&budget_json_response(&resp, 10_000)).unwrap();
        assert_eq!(out["_token_budget"]["deduped_overlapping"], 1);
        assert_eq!(out["hits"]["hits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn non_json_and_hitless_responses_still_respect_the_budget() {
        let out = budget_json_response("not json at all, just a long stream of words ", 8);
        assert!(count_tokens(&out) <= 8, "{out}");
        assert!(out.contains("truncated at the max_tokens"));

        let agg = compact(&json!({
            "took": 1,
            "aggregations": { "by_status": { "buckets": [
                { "key": "open", "doc_count": 1 }, { "key": "closed", "doc_count": 2 }
            ] } }
        }));
        let out = budget_json_response(&agg, 8);
        assert!(count_tokens(&out) <= 8, "{out}");
        // And a hitless response that fits passes through byte-identical.
        assert_eq!(budget_json_response(&agg, 10_000), agg);
    }

    #[test]
    fn a_binding_response_that_already_fits_is_verbatim() {
        let resp = search_response(vec![code_hit("a.rs", 3, 2)]);
        assert_eq!(budget_json_response(&resp, 100_000), resp);
    }

    #[test]
    fn a_budget_at_the_floor_still_never_exceeds() {
        // MIN_MAX_TOKENS exists so the envelope always fits; the inner guard
        // is pinned anyway — a forced-tiny budget truncates, never overflows.
        let resp = search_response(vec![code_hit("a.rs", 1, 4)]);
        let out = budget_json_response(&resp, 32);
        assert!(count_tokens(&out) <= 32, "{out}");
    }

    // ── rendered-text surface (xerj_code_search) ───────────────────────────

    fn render_symbol_hits(specs: &[(&str, u64, &str)], full: usize) -> String {
        use std::collections::HashMap;
        use xerj_common::xccode::render::{render, RenderOpts};
        let hits: Vec<Value> = specs
            .iter()
            .enumerate()
            .map(|(i, (path, line, code))| {
                json!({"_index":"repo-symbols", "_id":i.to_string(), "_score":2.0,
                "_source":{"ax_path":path,"line":line,"name":"example",
                    "kind":"function","code":code}})
            })
            .collect();
        render(
            &hits,
            &HashMap::new(),
            &RenderOpts {
                corpus: "repo",
                query: "fn",
                full,
                no_symbol: true,
                meatl: false,
                rrf_scores: false,
                age_days: None,
                strict_licence: false,
                own: false,
                lexical_lines: true,
                checkout: None,
            },
            None,
        )
    }

    #[test]
    fn code_text_real_renderer_preserves_adjacent_definitions() {
        for path in ["repo/src/lib.rs", "C:\\repo\\src\\lib.rs"] {
            let rendered = render_symbol_hits(
                &[
                    (path, 10, "pub fn alpha() {}"),
                    (path, 11, "pub fn beta() {}"),
                ],
                4096,
            );
            assert!(rendered.contains("pub fn beta() {}"));
            assert_eq!(
                budget_code_text(&rendered, 10_000),
                rendered,
                "a nonbinding budget must preserve adjacent definitions"
            );
        }
    }

    #[test]
    fn code_text_same_citation_with_different_windows_is_not_a_duplicate() {
        let rendered = render_symbol_hits(
            &[
                ("repo/lib.rs", 10, "pub fn alpha() { first(); }"),
                ("repo/lib.rs", 10, "pub fn alpha() { second(); }"),
            ],
            4096,
        );
        assert_eq!(budget_code_text(&rendered, 10_000), rendered);
    }

    #[test]
    fn code_text_adjacent_definitions_survive_a_binding_budget() {
        let long = format!("pub fn large() {{ {} }}", "compute(); ".repeat(1000));
        let rendered = render_symbol_hits(
            &[
                ("repo/lib.rs", 10, "pub fn alpha() {}"),
                ("repo/lib.rs", 11, "pub fn beta() {}"),
                ("repo/large.rs", 1, &long),
            ],
            20_000,
        );
        let out = budget_code_text(&rendered, 160);
        assert!(out.contains("pub fn alpha() {}"), "{out}");
        assert!(out.contains("pub fn beta() {}"), "{out}");
        assert!(out.contains("passage omitted:"), "{out}");
        assert!(out.contains("0 deduped"), "{out}");
        assert_never_exceeds(&rendered, &[32, 64, 100, 160, 400, 10_000], false);
    }

    #[test]
    fn code_text_presentation_lines_do_not_claim_source_coverage() {
        for full in [0, 10, 4096] {
            let rendered = render_symbol_hits(
                &[
                    ("repo/lib.rs", 10, "pub fn alpha() { first(); }"),
                    ("repo/lib.rs", 11, "pub fn beta() { second(); }"),
                ],
                full,
            );
            assert_eq!(
                budget_code_text(&rendered, 10_000),
                rendered,
                "file heads and window annotations must not become source lines"
            );
        }
        // Partially overlapping source snippets carry different information.
        let rendered = render_symbol_hits(
            &[
                ("repo/lib.rs", 10, "pub fn alpha() {\n    first();\n}"),
                ("repo/lib.rs", 11, "    first();\n}\npub fn beta() {}"),
            ],
            4096,
        );
        assert_eq!(budget_code_text(&rendered, 10_000), rendered);
    }

    #[test]
    fn code_text_locator_only_does_not_claim_a_duplicate_was_shown() {
        let long = "compute(); ".repeat(1000);
        let rendered = render_symbol_hits(
            &[("repo/lib.rs", 10, &long), ("repo/lib.rs", 10, &long)],
            20_000,
        );
        let out = budget_code_text(&rendered, 200);
        assert_eq!(out.matches("─── repo/lib.rs:10").count(), 2, "{out}");
        assert!(out.contains("2 locator-only, 0 deduped"), "{out}");
        assert!(count_tokens(&out) <= 200);
    }

    fn rendered_code_search() -> String {
        let mut text = String::new();
        for (path, line) in [
            ("tantivy/src/reader.rs", 30u64),
            ("quickwit/src/search.rs", 210),
            // Same displayed passage and citation from a second retrieval leg.
            ("tantivy/src/reader.rs", 30),
            ("qdrant/src/hnsw.rs", 500),
        ] {
            text.push_str(&format!(
                "\n─── {path}:{line}  (score 9.{line}, Apache-2.0)\n"
            ));
            text.push_str("    [method run — 300 of 12,000 chars]\n");
            for i in 0..4 {
                text.push_str(&format!(
                    "pub fn run_{i}(query: &str) -> Result<{{}}> {{ warmup_{i}(); }}\n"
                ));
            }
        }
        text.push_str("\n4 passages from 'peer-engines' (index 2d old)\n");
        text.push_str("Cite file:line for anything you rely on.\n");
        text
    }

    #[test]
    fn code_text_budget_holds_and_keeps_locators() {
        let rendered = rendered_code_search();
        assert_never_exceeds(&rendered, &[32, 64, 100, 150, 200, 400, 100_000], false);

        let out = budget_code_text(&rendered, 120);
        assert!(
            out.contains("─── tantivy/src/reader.rs:30"),
            "the first locator stays"
        );
        assert!(out.contains("passage omitted: over the max_tokens 120 budget"));
        assert!(out.contains("[token budget:"));
        assert!(
            out.contains("Cite file:line for anything you rely on."),
            "footer survives"
        );
    }

    #[test]
    fn code_text_identical_passages_are_deduped() {
        let rendered = rendered_code_search();
        // Identical locator and body are deduped, even without budget pressure.
        let out = budget_code_text(&rendered, 100_000);
        assert!(
            out.matches("─── tantivy/src/reader.rs:30").count() == 1,
            "the duplicate is gone"
        );
        assert!(
            out.contains("1 deduped (identical locator and passage)"),
            "{out}"
        );
        assert_never_exceeds(&rendered, &[32, 64, 100, 100_000], false);
    }

    #[test]
    fn code_text_with_nothing_to_do_is_byte_identical() {
        let no_dups = rendered_code_search().replacen(
            "tantivy/src/reader.rs:30",
            "valkey/src/listpack.c:88",
            1,
        );
        let out = budget_code_text(&no_dups, 100_000);
        assert_eq!(out, no_dups, "no overlap, under budget → untouched");
    }

    #[test]
    fn code_text_without_blocks_still_respects_the_budget() {
        let no_match =
            "No passage in 'kv' matches: two-way substring\nThe corpus is likely wrong.\n";
        let out = budget_code_text(no_match, 8);
        assert!(count_tokens(&out) <= 8, "{out}");
    }
}
