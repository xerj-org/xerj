//! `xerj_references` (#1791, first part): every indexed line that names an
//! exact identifier, minus its definitions.
//!
//! Lexical, by design, and said so in the result: the engine finds the files
//! whose stored text (`body`, or `text` for plain-text blocks) holds the
//! identifier, as a phrase or inside a longer token ([`query_body`]), then
//! each file's stored text is scanned line by line for the identifier with
//! identifier boundaries on both sides, case-sensitive. The scan is what turns "this file mentions
//! it" into `path:line`, the same line coordinate `xerj_def` cites
//! (`start_line` of the stored text, 1 for a whole file). A line that is one
//! of the file's own declarations of that name (the `symbols` catalogue the
//! code extractor writes) is a definition and is listed apart, not as a
//! reference. The enclosing symbol is the narrowest catalogued declaration
//! whose `line..=end_line` holds the reference.
//!
//! Not a call graph: a mention in a comment or a string counts, a call
//! through an alias or a re-export under another name does not, and two
//! unrelated symbols that share a name are not told apart. Callers, callees
//! and impact need call edges from the tree-sitter parse; they are a
//! follow-up (#1791).
//!
//! The query is built here, not in `xerj_common::code_query`: it is not a
//! ranking question, and nothing else shares it.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::{engine_request, opt_u64, search_index, token_budget, tool_text, Ctx, Method};

/// Files fetched per engine page.
const FILE_PAGE: usize = 100;
/// Most files scanned for one call. Past this the result says `truncated`.
const MAX_FILES: usize = 2_000;
/// Longest line text returned per reference, in characters.
const MAX_LINE_CHARS: usize = 240;
/// Default and largest page of references.
const DEFAULT_SIZE: u64 = 50;
const MAX_SIZE: u64 = 500;

pub(crate) fn spec() -> Value {
    json!({
        "name": "xerj_references",
        "description": concat!(
            "Every indexed line that names an exact identifier, excluding its definitions: ",
            "path, line, the line's text and the enclosing symbol (narrowest declaration ",
            "around it). Use it for \"where is X used / who calls X\" after xerj_def found the ",
            "definition. Lexical, not a call graph: finds the files whose text holds `symbol` ",
            "as a phrase, then scans each file's stored text for it with identifier ",
            "boundaries, case-sensitive. Comments and strings count; calls through an alias ",
            "do not; same-named symbols are not told apart. Lines that declare `symbol` ",
            "(from the file's symbol catalogue) come back in `definitions`, not ",
            "`references`. A mention inside a section of a long text file that the index ",
            "stores without a line number comes back in `unlocated` (path, section, text; ",
            "first 50, counted in `unlocated_total`), never with a guessed line. ",
            "`total` counts every located reference line; `references` is the ",
            "page `from`..`from+size`, sorted by path then line. `truncated: true` means more ",
            "than 2000 matching files and the scan stopped. Indices that map neither `body` nor `text` are ",
            "skipped. Optional max_tokens trims the ",
            "page from its end and says so in `budget`."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "symbol": {"type":"string", "minLength":1,
                    "description":"Exact identifier, e.g. `activeDialog`, `SEND_PROTOCOL` or a dotted `ActiveUtils.activeDialog` / `Vec::new`. No spaces."},
                "prefix": {"type":"string",
                    "description":"Autoindex prefix: project searches project-*. Cannot be combined with a non-empty index. Defaults to ax when neither scope is given."},
                "index": {"type":"string",
                    "description":"Explicit index name or pattern instead of prefix."},
                "from": {"type":"integer", "minimum":0, "default":0,
                    "description":"First reference of the page (0-based)."},
                "size": {"type":"integer", "minimum":1, "maximum":500, "default":50,
                    "description":"References per page (1..=500, default 50)."},
                "max_tokens": max_tokens_schema()
            },
            "required":["symbol"]
        }
    })
}

/// This tool's own budget contract: it trims the reference page from its
/// end (never the totals, the definitions or the unlocated count), unlike the
/// search tools' locator-only demotion.
fn max_tokens_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": token_budget::MIN_MAX_TOKENS,
        "default": token_budget::DEFAULT_MAX_TOKENS,
        "description": format!(
            "Word budget for the successful JSON result, counted with {}. References are \
             dropped from the end of the page until it fits; `budget.trimmed` says how many \
             and `next_from` continues there. Totals, definitions and unlocated counts are \
             kept. Minimum {}; when omitted the budget is {} UAX #29 words. Not a byte cap \
             or a model-token count.",
            token_budget::TOKENIZER_NAME,
            token_budget::MIN_MAX_TOKENS,
            token_budget::DEFAULT_MAX_TOKENS,
        )
    })
}

/// `true` for a byte that continues a word, as `rg -w` sees one: ASCII
/// letters, digits, `_`, and any non-ASCII byte (a letter in practice).
/// `$` is a boundary, so PHP's `$name` names `name`, as grep counts it.
fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// A byte that may begin or end a symbol argument (`$` for JavaScript).
fn symbol_end_byte(b: u8) -> bool {
    ident_byte(b) || b == b'$'
}

/// A symbol is an identifier, optionally dotted or `::`-joined. No blanks:
/// a phrase is a search, not a reference lookup.
fn validate_symbol(symbol: &str) -> Result<&str, String> {
    let s = symbol.trim();
    if s.is_empty() {
        return Err("`symbol` is required, for example {\"symbol\":\"activeDialog\"}".into());
    }
    let ends_ok = s.bytes().next().is_some_and(symbol_end_byte)
        && s.bytes().last().is_some_and(symbol_end_byte);
    let chars_ok = s
        .bytes()
        .all(|b| symbol_end_byte(b) || matches!(b, b'.' | b':' | b'-' | b'>'));
    if !ends_ok || !chars_ok {
        return Err(format!(
            "`symbol` must be one identifier (letters, digits, _ and $, optionally joined by \
             `.`, `::` or `->`), got {s:?}; use xerj_search for a phrase"
        ));
    }
    Ok(s)
}

/// The declared name a definition of `symbol` carries: its last segment.
fn declared_name(symbol: &str) -> &str {
    symbol.rsplit(['.', ':', '>']).next().unwrap_or(symbol)
}

/// Whether `line` holds `symbol` with identifier boundaries on both sides.
pub(crate) fn line_names(line: &str, symbol: &str) -> bool {
    let bytes = line.as_bytes();
    let mut start = 0;
    while let Some(pos) = line[start..].find(symbol) {
        let at = start + pos;
        let end = at + symbol.len();
        let before = at == 0 || !ident_byte(bytes[at - 1]);
        let after = end == bytes.len() || !ident_byte(bytes[end]);
        if before && after {
            return true;
        }
        start = at + line[at..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn clip(line: &str) -> String {
    let t = line.trim();
    match t.char_indices().nth(MAX_LINE_CHARS) {
        Some((cut, _)) => format!("{}…", &t[..cut]),
        None => t.to_string(),
    }
}

/// One catalogued declaration of a file.
struct Decl<'a> {
    name: &'a str,
    kind: &'a str,
    line: u64,
    end_line: u64,
}

fn decls(src: &Value) -> Vec<Decl<'_>> {
    src.get("symbols")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter_map(|s| {
                    let line = s.get("line").and_then(Value::as_u64)?;
                    Some(Decl {
                        name: s.get("name").and_then(Value::as_str)?,
                        kind: s.get("kind").and_then(Value::as_str).unwrap_or(""),
                        line,
                        end_line: s
                            .get("end_line")
                            .and_then(Value::as_u64)
                            .unwrap_or(line)
                            .max(line),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn hit_path(hit: &Value) -> String {
    let src = &hit["_source"];
    ["ax_path", "path", "title"]
        .iter()
        .find_map(|k| {
            src.get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| hit.get("_id").and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

/// A reference or definition found by the scan.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Found {
    pub path: String,
    pub line: u64,
    pub text: String,
    pub enclosing: Option<(String, String, u64)>,
}

impl Found {
    fn to_json(&self) -> Value {
        let mut v = json!({ "path": self.path, "line": self.line, "text": self.text });
        if let Some((name, kind, line)) = &self.enclosing {
            v["enclosing"] = json!({ "name": name, "kind": kind, "line": line });
        }
        v
    }
}

/// A mention in stored text that carries no line coordinate: a section of a
/// longer text file (`ax_locator` `s0`, `s1`, …) is stored without
/// `start_line`, so its own line 1 is not the file's line 1. Reporting it with
/// a guessed line would cite the wrong place (measured: wrong lines in
/// OPNsense `.volt` templates), so it is listed with its section instead.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Unlocated {
    pub path: String,
    pub section: String,
    pub text: String,
}

/// What one hit's scan found.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Scan {
    pub refs: Vec<Found>,
    pub defs: Vec<Found>,
    pub unlocated: Vec<Unlocated>,
}

/// Scan one hit's stored text.
pub(crate) fn scan_hit(hit: &Value, symbol: &str) -> Scan {
    let mut out = Scan::default();
    let src = &hit["_source"];
    let Some(body) = TEXT_FIELDS
        .iter()
        .find_map(|f| src.get(*f).and_then(Value::as_str))
    else {
        return out;
    };
    let path = hit_path(hit);
    // A stored text whose line count does not match its `start_line..=end_line`
    // span is lossy (`txt-lines` blocks indexed before the #3333 fix dropped
    // blank lines: 40 lines stored for a 1..=42 span), so its lines cannot be
    // mapped back either. A changed file, or `xerj autoindex --fresh`, stores
    // every line of the span.
    let start = src.get("start_line").and_then(Value::as_u64);
    let exact = match (start, src.get("end_line").and_then(Value::as_u64)) {
        (Some(s), Some(e)) => e.checked_sub(s).map(|d| d + 1) == Some(body.lines().count() as u64),
        _ => true,
    };
    let Some(first) = start.filter(|_| exact) else {
        let section = src
            .get("ax_locator")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        out.unlocated = body
            .lines()
            .filter(|text| line_names(text, symbol))
            .map(|text| Unlocated {
                path: path.clone(),
                section: section.clone(),
                text: clip(text),
            })
            .collect();
        return out;
    };
    let first = first.max(1);
    let decls = decls(src);
    let name = declared_name(symbol);
    for (i, text) in body.lines().enumerate() {
        if !line_names(text, symbol) {
            continue;
        }
        let line = first + i as u64;
        let is_def = decls.iter().any(|d| d.line == line && d.name == name);
        let enclosing = decls
            .iter()
            .filter(|d| d.line <= line && line <= d.end_line && !(is_def && d.line == line))
            .min_by_key(|d| (d.end_line - d.line, std::cmp::Reverse(d.line)))
            .map(|d| (d.name.to_string(), d.kind.to_string(), d.line));
        let found = Found {
            path: path.clone(),
            line,
            text: clip(text),
            enclosing,
        };
        if is_def {
            out.defs.push(found);
        } else {
            out.refs.push(found);
        }
    }
    out
}

/// Fields that hold a file's stored text: `body` for whole files and code,
/// `text` for the `txt-lines` blocks of plain-text files the code extractor
/// does not parse (OPNsense `.volt` templates are stored that way, each
/// block with its own `start_line`).
pub(crate) const TEXT_FIELDS: &[&str] = &["body", "text"];

/// Shortest symbol that also gets the infix wildcard clause. Below this a
/// `*x*` term scan matches most of the dictionary for nothing.
const WILDCARD_MIN_LEN: usize = 3;

/// The file query. `match_phrase` finds the identifier as its own token (or
/// token sequence, for `Vec::new`). It misses a mention glued to its
/// neighbours by UAX #29 word rules: `searcher.doc_freq(` is ONE token,
/// `searcher.doc_freq`, because `.` between letters does not break a word.
/// An infix wildcard over the term dictionary catches those; the line scan
/// then keeps only identifier-bounded, case-exact matches, so the wider net
/// costs no precision. The wildcard terms are the symbol as the index's own
/// field's own analyzer emits it (`terms`, from `_analyze`), because a
/// stemming analyzer stores `this.ajaxCall(` as `this.ajaxcal`, which
/// `*ajaxcall*` never matches. Measured: tantivy `doc_freq` recall 0.92 with
/// the phrase alone, OPNsense `ajaxCall` 0.13.
pub(crate) fn query_body(symbol: &str, fields: &[(String, Vec<String>)], from: usize) -> Value {
    let mut should = Vec::new();
    for (field, terms) in fields {
        should.push(json!({ "match_phrase": { field.as_str(): symbol } }));
        let mut wild: Vec<String> = terms.to_vec();
        wild.push(symbol.to_lowercase());
        wild.sort();
        wild.dedup();
        for term in wild {
            if term.chars().count() >= WILDCARD_MIN_LEN && !term.contains([':', '*', '?']) {
                should.push(json!({ "wildcard": { field.as_str(): format!("*{term}*") } }));
            }
        }
    }
    let mut source = vec![
        "ax_path",
        "path",
        "title",
        "start_line",
        "end_line",
        "ax_locator",
        "symbols",
    ];
    source.extend(TEXT_FIELDS);
    json!({
        "query": { "bool": { "should": should, "minimum_should_match": 1 } },
        "_source": source,
        "from": from,
        "size": FILE_PAGE,
        "track_total_hits": true,
    })
}

/// The concrete indices under `pattern` that map a stored-text field, with
/// those fields, from the `_mapping` response. Paging is done per concrete
/// index: on a pattern the node answered `from: 100` with no hits while
/// `hits.total` was 130 (rc.104, 2026-11-20, #3332), so a multi-index `from` cannot
/// be trusted to walk every file. `None` when the mapping could not be read.
pub(crate) fn text_indices(mapping: Option<&Value>) -> Option<Vec<(String, Vec<&'static str>)>> {
    let obj = mapping?.as_object()?;
    Some(
        obj.iter()
            .filter_map(|(name, m)| {
                let fields: Vec<&'static str> = TEXT_FIELDS
                    .iter()
                    .copied()
                    .filter(|f| m.pointer(&format!("/mappings/properties/{f}")).is_some())
                    .collect();
                (!fields.is_empty()).then(|| (name.clone(), fields))
            })
            .collect(),
    )
}

/// The one term `field`'s analyzer makes of `symbol`, or nothing when it
/// makes several (then an infix wildcard per term would match half the
/// dictionary) or the analysis fails.
async fn analyzed_term(ctx: &Ctx, index: &str, field: &str, symbol: &str) -> Vec<String> {
    let result = engine_request(
        ctx,
        Method::Post,
        &format!("/{}/_analyze", crate::enc(index)),
        Some(json!({ "field": field, "text": symbol })),
        None,
        None,
        None,
        true,
    )
    .await;
    if result["isError"] == true {
        return Vec::new();
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let tokens: Vec<String> = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            v.get("tokens").and_then(Value::as_array).map(|t| {
                t.iter()
                    .filter_map(|t| t.get("token").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
        })
        .unwrap_or_default();
    if tokens.len() == 1 {
        tokens
    } else {
        Vec::new()
    }
}

/// Most unlocated mentions listed (they are counted in full).
const MAX_UNLOCATED: usize = 50;

pub(crate) async fn run(ctx: &Ctx, args: &Value) -> Value {
    let validated = (|| {
        let symbol = match args.get("symbol") {
            Some(Value::String(s)) => validate_symbol(s)?,
            None | Some(Value::Null) => return Err(validate_symbol("").unwrap_err()),
            Some(_) => return Err("`symbol` must be a string".to_string()),
        };
        let index = search_index(args)?;
        let from = opt_u64(args, "from")?.unwrap_or(0);
        let size = opt_u64(args, "size")?.unwrap_or(DEFAULT_SIZE);
        if !(1..=MAX_SIZE).contains(&size) {
            return Err(format!("`size` must be an integer from 1 to {MAX_SIZE}"));
        }
        let budget = token_budget::opt_max_tokens(args)?;
        Ok((symbol, index, from as usize, size as usize, budget))
    })();
    let (symbol, index, from, size, budget) = match validated {
        Ok(v) => v,
        Err(e) => return tool_text(e, true),
    };

    let mapping = crate::fetch_mapping(ctx, &index).await;
    let targets = match text_indices(mapping.as_ref()) {
        Some(found) if found.is_empty() => {
            return tool_text(
                format!(
                    "no index matching `{index}` maps a `body` or `text` field, so there is no stored \
                     text to scan for references. xerj_references reads autoindexed files \
                     (`xerj autoindex <folder> --prefix <name>`); xerj_status lists what is indexed."
                ),
                true,
            )
        }
        Some(found) => found,
        None => vec![(index.clone(), TEXT_FIELDS.to_vec())],
    };

    let mut refs: BTreeSet<Found> = BTreeSet::new();
    let mut defs: BTreeSet<Found> = BTreeSet::new();
    let mut unlocated: BTreeSet<Unlocated> = BTreeSet::new();
    let mut scanned = 0usize;
    let mut matched: u64 = 0;
    'indices: for (target, text_fields) in &targets {
        let mut fields = Vec::new();
        for field in text_fields {
            let terms = analyzed_term(ctx, target, field, symbol).await;
            fields.push((field.to_string(), terms));
        }
        let mut from_file = 0usize;
        loop {
            let result = engine_request(
                ctx,
                Method::Post,
                &format!("/{}/_search", crate::enc(target)),
                Some(query_body(symbol, &fields, from_file)),
                None,
                None,
                None,
                true,
            )
            .await;
            if result["isError"] == true {
                return result;
            }
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            let resp: Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => {
                    return tool_text("XERJ returned invalid JSON for the reference lookup", true)
                }
            };
            let Some(hits) = resp.pointer("/hits/hits").and_then(Value::as_array) else {
                return tool_text("XERJ returned a reference response without hits.hits", true);
            };
            if from_file == 0 {
                matched += resp
                    .pointer("/hits/total/value")
                    .and_then(Value::as_u64)
                    .or_else(|| resp.pointer("/hits/total").and_then(Value::as_u64))
                    .unwrap_or(hits.len() as u64);
            }
            for hit in hits {
                let found = scan_hit(hit, symbol);
                refs.extend(found.refs);
                defs.extend(found.defs);
                unlocated.extend(found.unlocated);
            }
            scanned += hits.len();
            from_file += hits.len();
            if scanned >= MAX_FILES {
                break 'indices;
            }
            if hits.len() < FILE_PAGE {
                break;
            }
        }
    }
    let truncated = (scanned as u64) < matched;
    let total = refs.len();
    let mut page: Vec<Value> = refs
        .iter()
        .skip(from)
        .take(size)
        .map(Found::to_json)
        .collect();
    let files: BTreeSet<&str> = refs.iter().map(|f| f.path.as_str()).collect();
    let build = |page: &[Value], trimmed: usize| {
        let mut out = json!({
            "symbol": symbol,
            "total": total,
            "files": files.len(),
            "from": from,
            "size": page.len(),
            "references": page,
            "definitions": defs.iter().map(Found::to_json).collect::<Vec<_>>(),
            "unlocated_total": unlocated.len(),
            "unlocated": unlocated
                .iter()
                .take(MAX_UNLOCATED)
                .map(|u| json!({ "path": u.path, "section": u.section, "text": u.text }))
                .collect::<Vec<_>>(),
            "scanned_files": scanned,
            "engine_files_matched": matched,
            "truncated": truncated,
            "method": "lexical: files whose stored text holds the identifier (phrase, or inside a \
                       longer token), then an identifier-bounded, case-sensitive line scan of the \
                       stored text; not a call graph",
        });
        if from + page.len() < total {
            out["next_from"] = json!(from + page.len());
        }
        if trimmed > 0 {
            out["budget"] = json!({
                "trimmed": trimmed,
                "note": "references dropped from the end of the page to fit max_tokens; \
                         continue at next_from",
            });
        }
        out
    };
    let mut out = build(&page, 0);
    if let Some(max) = budget {
        let mut trimmed = 0;
        while !page.is_empty() && token_budget::count_tokens(&out.to_string()) > max {
            page.pop();
            trimmed += 1;
            out = build(&page, trimmed);
        }
    }
    let hint = crate::search_staleness(ctx, "xerj_references", args).await;
    crate::apply_staleness(tool_text(out.to_string(), false), hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_boundaries_are_respected() {
        assert!(line_names("x = activeDialog;", "activeDialog"));
        assert!(line_names("activeDialog()", "activeDialog"));
        assert!(line_names(
            "ActiveUtils.activeDialog(a)",
            "ActiveUtils.activeDialog"
        ));
        assert!(!line_names("x = activeDialogs;", "activeDialog"));
        assert!(!line_names("x = myactiveDialog;", "activeDialog"));
        assert!(
            line_names("$activeDialog", "activeDialog"),
            "as grep -w counts it"
        );
        assert!(
            !line_names("ActiveDialog", "activeDialog"),
            "case-sensitive"
        );
        assert!(line_names(
            "a_b activeDialog_x activeDialog",
            "activeDialog"
        ));
        assert!(line_names("é activeDialog", "activeDialog"));
        assert!(!line_names("éactiveDialog", "activeDialog"));
    }

    #[test]
    fn symbols_must_be_one_identifier() {
        assert!(validate_symbol("SEND_PROTOCOL").is_ok());
        assert!(validate_symbol("Vec::new").is_ok());
        assert!(validate_symbol("ActiveUtils.activeDialog").is_ok());
        assert!(validate_symbol(" spaced ").is_ok());
        assert!(validate_symbol("two words").is_err());
        assert!(validate_symbol("").is_err());
        assert!(validate_symbol(".x").is_err());
        assert!(validate_symbol("f(").is_err());
        assert_eq!(declared_name("ActiveUtils.activeDialog"), "activeDialog");
        assert_eq!(declared_name("Vec::new"), "new");
        assert_eq!(declared_name("plain"), "plain");
    }

    #[test]
    fn definitions_are_split_out_and_enclosing_is_the_narrowest_declaration() {
        let hit = json!({
            "_id": "x",
            "_source": {
                "ax_path": "src/app.js",
                "start_line": 1,
                "body": "class App {\n  activeDialog() {}\n  open() {\n    this.activeDialog();\n  }\n}\nactiveDialogs();\n",
                "symbols": [
                    {"name": "App", "kind": "class", "line": 1, "end_line": 6},
                    {"name": "activeDialog", "kind": "method", "line": 2, "end_line": 2},
                    {"name": "open", "kind": "method", "line": 3, "end_line": 5}
                ]
            }
        });
        let Scan {
            refs,
            defs,
            unlocated,
        } = scan_hit(&hit, "activeDialog");
        assert!(unlocated.is_empty());
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 2);
        assert_eq!(defs[0].enclosing.as_ref().unwrap().0, "App");
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0].path, "src/app.js");
        assert_eq!(refs[0].line, 4);
        assert_eq!(refs[0].text, "this.activeDialog();");
        assert_eq!(
            refs[0].enclosing,
            Some(("open".to_string(), "method".to_string(), 3))
        );
    }

    #[test]
    fn a_chunk_keeps_its_own_line_numbers() {
        let block = json!({"_source": {"path": "t.volt", "start_line": 30, "end_line": 31,
            "text": "x\n  ajaxCall();"}});
        assert_eq!(scan_hit(&block, "ajaxCall").refs[0].line, 31);
        // Blank lines dropped: 2 stored lines for a 3-line span cannot be mapped.
        let lossy = json!({"_source": {"path": "t.volt", "start_line": 30, "end_line": 32,
            "ax_locator": "b9", "text": "x\n  ajaxCall();"}});
        let found = scan_hit(&lossy, "ajaxCall");
        assert!(found.refs.is_empty(), "{found:?}");
        assert_eq!(found.unlocated[0].section, "b9");
        let hit =
            json!({"_source": {"path": "big.txt", "start_line": 41, "body": "a\nSEND_PROTOCOL\n"}});
        let found = scan_hit(&hit, "SEND_PROTOCOL");
        assert!(found.defs.is_empty());
        assert_eq!(found.refs[0].line, 42);
        assert!(found.refs[0].enclosing.is_none());
        let none = json!({"_source": {"name": "SEND_PROTOCOL", "code": "SEND_PROTOCOL = 1"}});
        assert_eq!(scan_hit(&none, "SEND_PROTOCOL"), Scan::default());
    }

    /// A section of a long text file has no `start_line`: its mentions are
    /// unlocated, never cited at the section's own line 1.
    #[test]
    fn a_section_without_start_line_is_unlocated_not_guessed() {
        let hit = json!({"_source": {
            "ax_path": "views/alias_util.volt", "ax_locator": "s1",
            "body": "x\n  ajaxCall('/api/x');\n"
        }});
        let found = scan_hit(&hit, "ajaxCall");
        assert!(found.refs.is_empty() && found.defs.is_empty());
        assert_eq!(
            found.unlocated,
            vec![Unlocated {
                path: "views/alias_util.volt".into(),
                section: "s1".into(),
                text: "ajaxCall('/api/x');".into(),
            }]
        );
    }

    #[test]
    fn the_query_adds_the_infix_wildcard_for_glued_mentions() {
        let body = |terms: &[&str]| {
            vec![(
                "body".to_string(),
                terms.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
            )]
        };
        let q = query_body("doc_freq", &body(&[]), 100);
        let should = q["query"]["bool"]["should"].as_array().unwrap();
        assert_eq!(should[0]["match_phrase"]["body"], "doc_freq");
        assert_eq!(should[1]["wildcard"]["body"], "*doc_freq*");
        assert_eq!(q["from"], 100);
        let lower = query_body("ActiveUtils.activeDialog", &body(&[]), 0);
        assert_eq!(
            lower["query"]["bool"]["should"][1]["wildcard"]["body"],
            "*activeutils.activedialog*"
        );
        for short_or_path in ["ab", "Vec::new"] {
            let q = query_body(short_or_path, &body(&[]), 0);
            assert_eq!(q["query"]["bool"]["should"].as_array().unwrap().len(), 1);
        }
        // A stemming analyzer's form is searched too, once.
        let stemmed = query_body("ajaxCall", &body(&["ajaxcal"]), 0);
        let wild: Vec<&str> = stemmed["query"]["bool"]["should"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.pointer("/wildcard/body").and_then(Value::as_str))
            .collect();
        assert_eq!(wild, vec!["*ajaxcal*", "*ajaxcall*"]);
        let same = query_body("ajaxCall", &body(&["ajaxcall"]), 0);
        assert_eq!(same["query"]["bool"]["should"].as_array().unwrap().len(), 2);
        // Every mapped text field is queried, and its stored text fetched.
        let both = query_body(
            "ajaxCall",
            &[("body".to_string(), vec![]), ("text".to_string(), vec![])],
            0,
        );
        let should = both["query"]["bool"]["should"].as_array().unwrap();
        assert_eq!(should.len(), 4);
        assert_eq!(should[2]["match_phrase"]["text"], "ajaxCall");
        let source = both["_source"].as_array().unwrap();
        for field in [
            "text",
            "body",
            "start_line",
            "end_line",
            "ax_locator",
            "symbols",
        ] {
            assert!(source.contains(&json!(field)), "{field}");
        }
    }

    #[test]
    fn only_indices_that_map_stored_text_are_paged() {
        let mapping = json!({
            "p-docs": {"mappings": {"properties": {
                "body": {"type": "semantic_text"}, "text": {"type": "text"}}}},
            "p-gh": {"mappings": {"properties": {"id": {"type": "keyword"}}}},
            "p-logs": {"mappings": {"properties": {"body": {"type": "text"}}}}
        });
        assert_eq!(
            text_indices(Some(&mapping)).unwrap(),
            vec![
                ("p-docs".to_string(), vec!["body", "text"]),
                ("p-logs".to_string(), vec!["body"]),
            ]
        );
        assert_eq!(text_indices(None), None);
        assert_eq!(text_indices(Some(&json!({}))), Some(Vec::new()));
    }

    #[test]
    fn long_lines_are_clipped_on_a_char_boundary() {
        let long = "é".repeat(300);
        let c = clip(&long);
        assert_eq!(c.chars().count(), MAX_LINE_CHARS + 1);
        assert!(c.ends_with('…'));
    }
}
