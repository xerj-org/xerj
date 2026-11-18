//! Correct `xerj def` answers from an index older than its folder (#3268).
//!
//! The `index_stale` hint (#2345) says a prefix is behind its folder. It did
//! not change the answer, so an agent still got the line a function sat on at
//! index time (138 when it is now 141) and "no definition found" for a
//! function added since. Agents then re-read every hit by hand, and where
//! only one owner may rebuild a shared index, they could not fix it.
//!
//! This module runs only when that hint fired, so a fresh tree reads no file
//! and its output is unchanged. It writes nothing to the index.
//!
//! - **Re-anchor.** A hit whose file is newer than the index stamp is re-read
//!   (at most [`crate::extract::code::CODE_CAP`] bytes). Its declaration line
//!   is looked up by text, first within [`WINDOW`] lines of the indexed line,
//!   then in the whole file; when the line text changed too, the extractor
//!   finds the symbol by name. The hit then carries the current line and
//!   `reanchored: true` with `indexed_line`. A hit whose file is gone, or
//!   whose symbol the extractor no longer finds, is dropped.
//! - **Unindexed definitions.** When no hit is an exact-name definition, the
//!   extractor runs read-only over the code files changed since the stamp
//!   (bounded by [`UNINDEXED_MAX_FILES`], [`UNINDEXED_MAX_BYTES`] and the
//!   walk limits) and exact-name matches are returned with `unindexed: true`.

use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::staleness::{self, StalenessHint, WalkLimits};

/// Lines either side of the indexed line searched before the whole file.
pub const WINDOW: usize = 64;
/// Changed code files the unindexed pass parses.
pub const UNINDEXED_MAX_FILES: usize = 256;
/// Total bytes the unindexed pass reads.
pub const UNINDEXED_MAX_BYTES: u64 = 32 << 20;
/// Walk limits for the unindexed pass: wider than the hint's 100 ms, since
/// it runs only on a stale prefix that found no exact definition.
pub const UNINDEXED_WALK: WalkLimits = WalkLimits {
    stat_cap: 50_000,
    wall: Duration::from_millis(1500),
    grace: staleness::MTIME_GRACE,
};

/// The folder an index was built from, and when.
#[derive(Debug, Clone)]
pub struct IndexedTree {
    pub root: PathBuf,
    pub indexed_at: SystemTime,
    pub grace: Duration,
}

impl IndexedTree {
    /// From a hint the staleness check produced. `None` when its root is not
    /// a directory here or its time does not parse.
    pub fn from_hint(hint: &StalenessHint) -> Option<Self> {
        let root = PathBuf::from(&hint.root);
        if !root.is_dir() {
            return None;
        }
        Some(Self {
            root,
            indexed_at: staleness::parse_time(&hint.indexed_at)?,
            grace: staleness::MTIME_GRACE,
        })
    }

    fn changed(&self, mtime: SystemTime) -> bool {
        mtime
            .duration_since(self.indexed_at)
            .is_ok_and(|age| age > self.grace)
    }
}

/// What [`correct_definition_hits`] did, for the CLI's summary line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Correction {
    pub reanchored: usize,
    pub dropped: usize,
    pub unindexed: usize,
}

impl Correction {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One line for `xerj def`, or `None` when nothing changed.
    pub fn cli_line(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if self.reanchored > 0 {
            parts.push(format!(
                "{} line(s) re-anchored to the file on disk",
                self.reanchored
            ));
        }
        if self.dropped > 0 {
            parts.push(format!(
                "{} hit(s) dropped (file or symbol no longer there)",
                self.dropped
            ));
        }
        if self.unindexed > 0 {
            parts.push(format!(
                "{} definition(s) found in changed files that are not indexed yet",
                self.unindexed
            ));
        }
        Some(format!(
            "xerj def: corrected against the folder: {}.",
            parts.join("; ")
        ))
    }
}

/// Record a non-empty correction under `_xerj.freshness`, next to the
/// `index_stale` hint. A response that is not a JSON object is left alone.
pub fn attach(resp: &mut Value, fixed: &Correction) {
    let Some(obj) = resp.as_object_mut() else {
        return;
    };
    let xerj = obj.entry("_xerj").or_insert_with(|| json!({}));
    if let Some(xerj) = xerj.as_object_mut() {
        xerj.insert(
            "freshness".into(),
            json!({
                "reanchored": fixed.reanchored,
                "dropped": fixed.dropped,
                "unindexed": fixed.unindexed,
                "note": "hits were compared with the files on disk because the index is older than \
                         its folder; `reanchored` hits carry the current line and `indexed_line`, \
                         `unindexed` hits come from changed files the index does not hold yet",
            }),
        );
    }
}

fn hit_path(src: &Value) -> Option<&str> {
    src.get("ax_path")
        .or_else(|| src.get("path"))
        .and_then(Value::as_str)
}

fn read_capped(path: &Path, cap: u64) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > cap {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes).ok()?;
    Some(crate::sniff::decode_text(&bytes).0)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// `name` occurs in `line` with no identifier character on either side.
fn has_word(line: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(at) = line[from..].find(name).map(|i| from + i) {
        let end = at + name.len();
        let before = at == 0 || !is_word_byte(bytes[at - 1]);
        let after = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before && after {
            return true;
        }
        from = at + 1;
        while from < line.len() && !line.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

/// The declaration line to look for: the first line of `code` that names
/// the symbol, trimmed. `None` when `code` does not name it.
fn signature_line<'a>(code: &'a str, name: &str) -> Option<&'a str> {
    code.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && has_word(l, name))
}

/// 1-based line in `lines` whose trimmed text is `sig`, nearest to
/// `indexed` (1-based): within [`WINDOW`] first, then anywhere.
fn find_line(lines: &[&str], sig: &str, indexed: usize) -> Option<usize> {
    let matches = |i: usize| lines.get(i).is_some_and(|l| l.trim() == sig);
    let center = indexed.saturating_sub(1);
    for d in 0..=WINDOW.max(lines.len()) {
        if center >= d && matches(center - d) {
            return Some(center - d + 1);
        }
        if d > 0 && matches(center + d) {
            return Some(center + d + 1);
        }
        if center < d && center + d >= lines.len() {
            break;
        }
    }
    None
}

fn ext_of(rel: &str) -> String {
    Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Where the hit's symbol is now, by extractor: same name, same kind when
/// the hit has one, nearest the indexed line. `Err(())` when the extractor
/// cannot read this language, so absence proves nothing.
fn extractor_line(
    rel: &str,
    text: &str,
    name: &str,
    kind: Option<&str>,
    indexed: usize,
) -> Result<Option<(usize, String)>, ()> {
    let ext = ext_of(rel);
    let lang = crate::extract::code::language_for(&ext, text).ok_or(())?;
    let defs = crate::extract::code::definitions(rel, lang, text);
    let same_name: Vec<_> = defs.iter().filter(|d| d.symbol == name).collect();
    let same_kind: Vec<_> = same_name
        .iter()
        .filter(|d| kind.is_none_or(|k| d.kind == k))
        .copied()
        .collect();
    let pool = if same_kind.is_empty() {
        same_name
    } else {
        same_kind
    };
    Ok(pool
        .into_iter()
        .min_by_key(|d| (d.line as usize).abs_diff(indexed))
        .map(|d| (d.line as usize, d.text.clone())))
}

enum Fix {
    Keep,
    Drop,
    Move { line: usize, code: Option<String> },
}

fn fix_hit(tree: &IndexedTree, hit: &Value) -> Fix {
    let Some(src) = hit.get("_source") else {
        return Fix::Keep;
    };
    let (Some(name), Some(line), Some(rel)) = (
        src.get("name").and_then(Value::as_str),
        src.get("line").and_then(Value::as_u64),
        hit_path(src),
    ) else {
        return Fix::Keep;
    };
    let Some(rel) = staleness::safe_rel(rel) else {
        return Fix::Keep;
    };
    let path = tree.root.join(rel);
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Fix::Drop,
        Err(_) => return Fix::Keep,
    };
    if !tree.changed(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)) {
        return Fix::Keep;
    }
    let Some(text) = read_capped(&path, crate::extract::code::CODE_CAP) else {
        return Fix::Keep;
    };
    let lines: Vec<&str> = text.lines().collect();
    let indexed = line as usize;
    let code = src.get("code").and_then(Value::as_str).unwrap_or("");
    if let Some(sig) = signature_line(code, name) {
        if let Some(now) = find_line(&lines, sig, indexed) {
            return if now == indexed {
                Fix::Keep
            } else {
                Fix::Move {
                    line: now,
                    code: None,
                }
            };
        }
    }
    let kind = src.get("kind").and_then(Value::as_str);
    match extractor_line(rel, &text, name, kind, indexed) {
        Ok(Some((now, _))) if now == indexed => Fix::Keep,
        Ok(Some((now, decl))) => Fix::Move {
            line: now,
            code: Some(decl),
        },
        Ok(None) => Fix::Drop,
        Err(()) => Fix::Keep,
    }
}

fn shift(value: Option<&Value>, delta: i64) -> Option<Value> {
    let n = value?.as_u64()? as i64 + delta;
    (n >= 1).then(|| json!(n))
}

/// Re-anchor or drop hits whose file changed after `tree.indexed_at`.
/// Returns (re-anchored, dropped).
pub fn reanchor_hits(hits: &mut Vec<Value>, tree: &IndexedTree) -> (usize, usize) {
    let mut moved = 0;
    let mut dropped = 0;
    let mut kept = Vec::with_capacity(hits.len());
    for mut hit in hits.drain(..) {
        match fix_hit(tree, &hit) {
            Fix::Keep => kept.push(hit),
            Fix::Drop => dropped += 1,
            Fix::Move { line, code } => {
                let Some(src) = hit.get_mut("_source").and_then(Value::as_object_mut) else {
                    kept.push(hit);
                    continue;
                };
                let old = src.get("line").and_then(Value::as_u64).unwrap_or(0);
                let delta = line as i64 - old as i64;
                src.insert("line".into(), json!(line));
                if src.contains_key("start_line") {
                    src.insert("start_line".into(), json!(line));
                }
                if let Some(end) = shift(src.get("end_line"), delta) {
                    src.insert("end_line".into(), end);
                }
                if let Some(code) = code {
                    src.insert("code".into(), json!(code));
                }
                if let Some(obj) = hit.as_object_mut() {
                    obj.insert("reanchored".into(), json!(true));
                    obj.insert("indexed_line".into(), json!(old));
                }
                moved += 1;
                kept.push(hit);
            }
        }
    }
    *hits = kept;
    (moved, dropped)
}

/// Exact-name definitions of `name` in code files changed since the index
/// stamp, as hits marked `unindexed: true`, sorted by path then line.
pub fn unindexed_definitions(
    tree: &IndexedTree,
    name: &str,
    kind: Option<&str>,
    lang: Option<&str>,
    limits: &WalkLimits,
) -> Vec<Value> {
    let scan = staleness::changed_files(&tree.root, tree.indexed_at, limits, usize::MAX);
    let mut out = Vec::new();
    let mut files = 0usize;
    let mut bytes = 0u64;
    for rel in &scan.files {
        let ext = ext_of(rel);
        if !crate::extract::code::is_code_ext(&ext) {
            continue;
        }
        if files >= UNINDEXED_MAX_FILES {
            break;
        }
        let path = tree.root.join(rel);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > crate::extract::code::CODE_CAP
            || bytes.saturating_add(meta.len()) > UNINDEXED_MAX_BYTES
        {
            continue;
        }
        let Some(text) = read_capped(&path, crate::extract::code::CODE_CAP) else {
            continue;
        };
        files += 1;
        bytes += meta.len();
        let Some(language) = crate::extract::code::language_for(&ext, &text) else {
            continue;
        };
        if lang.is_some_and(|l| l != language) {
            continue;
        }
        for d in crate::extract::code::definitions(rel, language, &text) {
            if d.symbol != name || kind.is_some_and(|k| d.kind != k) {
                continue;
            }
            out.push(json!({
                "_index": "",
                "_score": 0.0,
                "unindexed": true,
                "_source": {
                    "ax_path": rel,
                    "path": rel,
                    "line": d.line,
                    "start_line": d.line,
                    "kind": d.kind,
                    "language": language,
                    "name": d.symbol,
                    "code": d.text,
                }
            }));
        }
    }
    out.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                v.pointer("/_source/ax_path")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                v.pointer("/_source/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        };
        key(a).cmp(&key(b))
    });
    out
}

/// True when some hit is a declaration named exactly `name`.
fn has_exact_definition(hits: &[Value], name: &str) -> bool {
    hits.iter()
        .any(|h| h.pointer("/_source/name").and_then(Value::as_str) == Some(name))
}

/// Correct a ranked, de-duplicated definition list in place against the
/// folder `tree` describes, keeping at most `k`. Call only when the
/// staleness check fired; see the module docs.
pub fn correct_definition_hits(
    hits: &mut Vec<Value>,
    tree: &IndexedTree,
    query: &str,
    kind: Option<&str>,
    lang: Option<&str>,
    k: usize,
) -> Correction {
    let (reanchored, dropped) = reanchor_hits(hits, tree);
    let mut unindexed = 0;
    let name = query.trim();
    if crate::search::is_identifier(name) && !has_exact_definition(hits, name) {
        let found = unindexed_definitions(tree, name, kind, lang, &UNINDEXED_WALK);
        unindexed = found.len().min(k);
        if !found.is_empty() {
            let rest = std::mem::take(hits);
            hits.extend(found);
            hits.extend(rest);
        }
    }
    // Two indexed hits can land on one line once re-anchored.
    let mut seen = std::collections::BTreeSet::new();
    hits.retain(|h| seen.insert(crate::search::hit_location(h)));
    hits.truncate(k);
    Correction {
        reanchored,
        dropped,
        unindexed,
    }
}

/// [`correct_definition_hits`] on `response["hits"]["hits"]`. Totals are
/// left as the engine reported them, as for the location de-duplication.
pub fn correct_definition_response(
    response: &mut Value,
    hint: &StalenessHint,
    query: &str,
    kind: Option<&str>,
    lang: Option<&str>,
    k: usize,
) -> Correction {
    let Some(tree) = IndexedTree::from_hint(hint) else {
        return Correction::default();
    };
    let Some(slot) = response.pointer_mut("/hits/hits") else {
        return Correction::default();
    };
    let Some(hits) = slot.as_array_mut() else {
        return Correction::default();
    };
    correct_definition_hits(hits, &tree, query, kind, lang, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn touch(path: &Path, when: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn symbol_hit(rel: &str, name: &str, kind: &str, line: u64, code: &str) -> Value {
        json!({
            "_index": "ax-code",
            "_score": 10.0,
            "_source": {
                "ax_path": rel, "path": rel, "name": name, "kind": kind,
                "line": line, "start_line": line, "end_line": line + 2, "code": code
            }
        })
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        tree: IndexedTree,
    }

    fn fixture(files: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let stamp = SystemTime::now() - Duration::from_secs(3600);
        for (rel, text) in files {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            touch(&path, stamp - Duration::from_secs(60));
        }
        Fixture {
            tree: IndexedTree {
                root: dir.path().to_path_buf(),
                indexed_at: stamp,
                grace: Duration::from_secs(2),
            },
            _dir: dir,
        }
    }

    fn edit(fx: &Fixture, rel: &str, text: &str) {
        let path = fx.tree.root.join(rel);
        fs::write(&path, text).unwrap();
        touch(&path, SystemTime::now());
    }

    const LIB_C: &str = "#include <stdio.h>\n\nint helper(int x)\n{\n    return x + 1;\n}\n\nint target(int y)\n{\n    return helper(y);\n}\n";

    /// #3268 acceptance: three lines inserted above a function. The hit
    /// reports the line the function is on now, flagged as re-anchored.
    #[test]
    fn three_inserted_lines_move_the_reported_line() {
        let fx = fixture(&[("src/lib.c", LIB_C)]);
        let mut hits = vec![symbol_hit(
            "src/lib.c",
            "target",
            "function",
            8,
            "int target(int y)",
        )];
        edit(
            &fx,
            "src/lib.c",
            &LIB_C.replacen("\nint target", "\n/* a */\n/* b */\n/* c */\nint target", 1),
        );
        let fixed = correct_definition_hits(&mut hits, &fx.tree, "target", None, None, 5);
        assert_eq!(fixed.reanchored, 1, "{hits:?}");
        assert_eq!(hits[0]["_source"]["line"], 11);
        assert_eq!(hits[0]["_source"]["start_line"], 11);
        assert_eq!(hits[0]["_source"]["end_line"], 13);
        assert_eq!(hits[0]["reanchored"], true);
        assert_eq!(hits[0]["indexed_line"], 8);
    }

    /// An unchanged file is not read and the hit is byte-identical.
    #[test]
    fn an_unchanged_file_keeps_the_hit_as_is() {
        let fx = fixture(&[("src/lib.c", LIB_C)]);
        let hit = symbol_hit("src/lib.c", "target", "function", 8, "int target(int y)");
        let mut hits = vec![hit.clone()];
        let fixed = correct_definition_hits(&mut hits, &fx.tree, "target", None, None, 5);
        assert!(fixed.is_empty());
        assert_eq!(hits, vec![hit]);
    }

    /// A changed signature is found by the extractor; a removed function and
    /// a removed file are dropped.
    #[test]
    fn a_changed_signature_moves_and_a_removed_symbol_drops() {
        let fx = fixture(&[
            ("src/lib.c", LIB_C),
            ("src/gone.c", "int gone(void) { return 0; }\n"),
        ]);
        edit(
            &fx,
            "src/lib.c",
            "\n\nint target(long y, long z)\n{\n    return (int)(y + z);\n}\n",
        );
        fs::remove_file(fx.tree.root.join("src/gone.c")).unwrap();
        let mut hits = vec![
            symbol_hit("src/lib.c", "target", "function", 8, "int target(int y)"),
            symbol_hit("src/lib.c", "helper", "function", 3, "int helper(int x)"),
            symbol_hit("src/gone.c", "gone", "function", 1, "int gone(void)"),
        ];
        let fixed = correct_definition_hits(&mut hits, &fx.tree, "target", None, None, 5);
        assert_eq!((fixed.reanchored, fixed.dropped), (1, 2), "{hits:?}");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["_source"]["line"], 3);
        assert_eq!(hits[0]["_source"]["code"], "int target(long y, long z)");
    }

    /// #3268 acceptance: a function added after the build, with no reindex,
    /// is found and flagged unindexed. A name defined nowhere stays empty.
    #[test]
    fn a_new_function_is_found_unindexed() {
        let fx = fixture(&[
            ("src/lib.c", LIB_C),
            ("src/other.py", "def other():\n    pass\n"),
        ]);
        edit(
            &fx,
            "src/lib.c",
            &format!("{LIB_C}\nstatic int brand_new(void)\n{{\n    return 7;\n}}\n"),
        );
        let mut hits = Vec::new();
        let fixed = correct_definition_hits(&mut hits, &fx.tree, "brand_new", None, None, 5);
        assert_eq!(fixed.unindexed, 1);
        assert_eq!(hits[0]["unindexed"], true);
        assert_eq!(hits[0]["_source"]["ax_path"], "src/lib.c");
        assert_eq!(hits[0]["_source"]["line"], 13);
        assert_eq!(hits[0]["_source"]["kind"], "function");

        let mut none = Vec::new();
        let fixed = correct_definition_hits(&mut none, &fx.tree, "nowhere_at_all", None, None, 5);
        assert!(none.is_empty() && fixed.is_empty());

        // `--lang python` filters the changed C file out.
        let mut py = Vec::new();
        correct_definition_hits(&mut py, &fx.tree, "brand_new", None, Some("python"), 5);
        assert!(py.is_empty());
    }

    #[test]
    fn find_line_prefers_the_nearest_match() {
        let lines = ["a", "foo:", "b", "c", "foo:", "d"];
        assert_eq!(find_line(&lines, "foo:", 5), Some(5));
        assert_eq!(find_line(&lines, "foo:", 1), Some(2));
        assert_eq!(find_line(&lines, "zzz", 3), None);
        assert!(has_word("int target(int y)", "target"));
        assert!(!has_word("int targets(int y)", "target"));
    }
}
