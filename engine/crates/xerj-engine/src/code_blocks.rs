//! AST-aligned code passages (#174 follow-up).
//!
//! `_passage` normally returns an arbitrary byte window (a 512-char
//! index-time chunk for semantic hits, a line-snapped query-time window for
//! lexical hits). Neither knows about syntax, so a code hit's passage can
//! open mid-token — an observed real example began `"\t}\n\n\tif ( isset( "`,
//! a dangling brace with no idea what function it belongs to.
//!
//! This module resolves, at RESPONSE time (not index time — no reindex, no
//! migration, works on every existing index), the smallest syntactic node
//! enclosing a byte range that counts as a semantic unit (function, method,
//! class, ...) for the document's language. The caller (`index.rs`) widens
//! the passage to that span when one is found; otherwise the existing window
//! is left untouched.
//!
//! ## Node-kind policy tables
//!
//! Which node kinds count as an acceptable enclosing block is inherently
//! per-language and per-grammar. The lists below are adapted from
//! [probelabs/probe](https://github.com/probelabs/probe)'s
//! `is_acceptable_parent` policy (`src/language/*.rs`, dual Apache-2.0/MIT),
//! reduced to just the node-kind data and re-expressed for XERJ's
//! tree-sitter 0.25 bindings and the language set `xerj-autoindex` already
//! parses (`xerj-autoindex/src/extract/code.rs`). No probe source is
//! depended on or copied wholesale — only these kind lists, and narrowed:
//! probe's tables also include generic containers (`block`, `statement`,
//! `module`, `object`, ...) it uses to keep code-context expansion from
//! jumping too far; those are deliberately dropped here so "no acceptable
//! enclosing node" (e.g. a top-level statement) falls back to the caller's
//! original window instead of returning a bare block.
//!
//! Every kind name below was checked against the installed grammar's
//! `node-types.json` for the exact pinned version (see `Cargo.toml`).
//!
//! Bash has no probe table (probe does not support it); its single kind,
//! `function_definition`, is picked directly from `tree-sitter-bash`'s own
//! grammar, analogously to the other C-family languages.

use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::sync::OnceLock;
use tree_sitter::{Language, Parser, Tree};

/// Default cap on the byte size of a resolved block. Past this we fall back
/// to the caller's original window rather than hand back a giant function —
/// probe itself has no such cap and returned a 64,120-byte / 26,906-token
/// function in one observed case, which is not a "passage" any more.
/// Sized generously above a typical function/method/class (a few hundred
/// bytes to a few KB) while still bounding the worst case. Overridable via
/// `XERJ_CODE_PASSAGE_MAX_BYTES` for deployments with different tolerances.
pub const DEFAULT_MAX_BLOCK_BYTES: usize = 16_384;

/// A call-argument callback larger than this is not a passage (#2327).
///
/// The same bound as the lexical `_passage` window (`LEXICAL_PASSAGE_MAX_BYTES`
/// in `index.rs`). A nested `it(() => { ... })` body under the 16 KiB block
/// cap used to come back whole. Past this size the caller keeps that
/// line-snapped window instead. A top-level arrow (`const f = () => {}`) is
/// not a call argument and keeps [`DEFAULT_MAX_BLOCK_BYTES`].
pub const NESTED_CALLBACK_MAX_BYTES: usize = 2_048;

const CALLBACK_KINDS: &[&str] = &["arrow_function", "function_expression"];

fn max_block_bytes() -> usize {
    std::env::var("XERJ_CODE_PASSAGE_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_BLOCK_BYTES)
}

/// Bound on how many parsed trees the in-process cache holds. XERJ is a
/// long-lived server (unlike probe's short-lived CLI process, whose
/// equivalent cache simply dies with it), so an unbounded cache would leak
/// memory for every distinct code document ever hit. Overridable via
/// `XERJ_CODE_PASSAGE_CACHE_CAP`.
const DEFAULT_CACHE_CAPACITY: usize = 128;

fn cache_capacity() -> usize {
    std::env::var("XERJ_CODE_PASSAGE_CACHE_CAP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_CACHE_CAPACITY)
}

// ── Node-kind policy tables ─────────────────────────────────────────────────

const RUST_KINDS: &[&str] = &[
    "function_item",
    "struct_item",
    "impl_item",
    "trait_item",
    "enum_item",
    "mod_item",
    "macro_definition",
];

const PHP_KINDS: &[&str] = &[
    "function_definition",
    "method_declaration",
    "class_declaration",
    "interface_declaration",
    "trait_declaration",
];

const PYTHON_KINDS: &[&str] = &[
    "function_definition",
    "class_definition",
    "decorated_definition",
];

const JS_KINDS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "method_definition",
    "class_declaration",
    "arrow_function",
    "class",
];

// TypeScript-specific additions on top of the JS set. `internal_module` and
// `module` are the actual grammar node kinds for `namespace Foo {}` /
// `declare module "foo" {}` in tree-sitter-typescript 0.23 — probe's own
// list names them `namespace_declaration`/`module_declaration`, which do not
// exist in this grammar version; verified against the pinned crate's
// `node-types.json`.
const TS_EXTRA_KINDS: &[&str] = &[
    "interface_declaration",
    "type_alias_declaration",
    "enum_declaration",
    "internal_module",
    "module",
];

const GO_KINDS: &[&str] = &[
    "function_declaration",
    "method_declaration",
    "type_declaration",
    "struct_type",
    "interface_type",
    "type_spec",
    // Package-level `const`/`var` blocks are declarations a reader looks up by
    // name, the same as a type or a func — XERJ's own code extractor already
    // captures them as symbols, so the passage resolver must agree.
    "const_declaration",
    "const_spec",
    "var_declaration",
    "var_spec",
];

const JAVA_KINDS: &[&str] = &[
    "method_declaration",
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "constructor_declaration",
    "static_initializer",
    // A class field is a named member a reader looks up, not a statement.
    // (`variable_declaration` is deliberately absent: that is a local inside a
    // method body, and returning one as a "block" would be noise.)
    "field_declaration",
];

const C_KINDS: &[&str] = &["function_definition", "struct_specifier", "enum_specifier"];

const CPP_KINDS: &[&str] = &[
    "function_definition",
    "struct_specifier",
    "class_specifier",
    "enum_specifier",
    "namespace_definition",
];

const RUBY_KINDS: &[&str] = &["class", "module", "method", "singleton_method"];

const CSHARP_KINDS: &[&str] = &[
    "method_declaration",
    "class_declaration",
    "struct_declaration",
    "interface_declaration",
    "enum_declaration",
    "namespace_declaration",
    "property_declaration",
    "constructor_declaration",
    "delegate_declaration",
    "event_declaration",
];

const BASH_KINDS: &[&str] = &["function_definition"];

/// One JS+TS combined slice, built once. TypeScript and TSX both accept the
/// JS kinds plus the TS-only ones.
fn ts_kinds() -> &'static [&'static str] {
    static KINDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    KINDS
        .get_or_init(|| {
            let mut v = JS_KINDS.to_vec();
            v.extend_from_slice(TS_EXTRA_KINDS);
            v
        })
        .as_slice()
}

/// Resolve a `language` field value (as `xerj-autoindex/src/extract/code.rs`
/// writes it) to its tree-sitter grammar and acceptable-block kind table.
/// `None` means "not a language this module can parse" — the caller falls
/// back to its existing window.
fn registry_entry(language: &str) -> Option<(Language, &'static [&'static str])> {
    Some(match language {
        "python" => (tree_sitter_python::LANGUAGE.into(), PYTHON_KINDS),
        "javascript" => (tree_sitter_javascript::LANGUAGE.into(), JS_KINDS),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ts_kinds(),
        ),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), ts_kinds()),
        "rust" => (tree_sitter_rust::LANGUAGE.into(), RUST_KINDS),
        "go" => (tree_sitter_go::LANGUAGE.into(), GO_KINDS),
        "java" => (tree_sitter_java::LANGUAGE.into(), JAVA_KINDS),
        "c" => (tree_sitter_c::LANGUAGE.into(), C_KINDS),
        "cpp" => (tree_sitter_cpp::LANGUAGE.into(), CPP_KINDS),
        "ruby" => (tree_sitter_ruby::LANGUAGE.into(), RUBY_KINDS),
        "php" => (tree_sitter_php::LANGUAGE_PHP.into(), PHP_KINDS),
        "csharp" => (tree_sitter_c_sharp::LANGUAGE.into(), CSHARP_KINDS),
        "bash" => (tree_sitter_bash::LANGUAGE.into(), BASH_KINDS),
        _ => return None,
    })
}

// ── Bounded LRU tree cache ───────────────────────────────────────────────────

struct TreeCache {
    capacity: usize,
    map: FxHashMap<u64, Tree>,
    // Recency order, oldest first. `key` may appear only once; a re-touch
    // removes and re-appends it. Capacity is small (default 128) and this
    // runs on the query path, not a hot per-token loop, so O(n) touch is
    // fine — a hand-rolled intrusive doubly-linked list would buy nothing
    // observable here.
    order: VecDeque<u64>,
}

impl TreeCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            map: FxHashMap::default(),
            order: VecDeque::new(),
        }
    }

    fn touch(&mut self, key: u64) {
        if let Some(pos) = self.order.iter().position(|&k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }

    /// Cheap ref-counted clone on a cache hit (tree-sitter `Tree` wraps a
    /// shared node pool); a fresh parse on a miss.
    fn get_or_parse(&mut self, key: u64, language: &Language, body: &str) -> Option<Tree> {
        if let Some(tree) = self.map.get(&key) {
            let tree = tree.clone();
            self.touch(key);
            return Some(tree);
        }
        let mut parser = Parser::new();
        parser.set_language(language).ok()?;
        let tree = parser.parse(body.as_bytes(), None)?;
        if self.map.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(key, tree.clone());
        self.touch(key);
        Some(tree)
    }
}

fn cache() -> &'static parking_lot::Mutex<TreeCache> {
    static CACHE: OnceLock<parking_lot::Mutex<TreeCache>> = OnceLock::new();
    CACHE.get_or_init(|| parking_lot::Mutex::new(TreeCache::new(cache_capacity())))
}

/// Content-addressed cache key: language name mixed into the seed so two
/// languages never collide on the same source bytes (which cannot happen in
/// practice — a document has one `language` — but costs nothing to guard).
fn cache_key(language: &str, body: &str) -> u64 {
    let seed = xxhash_rust::xxh3::xxh3_64(language.as_bytes());
    xxhash_rust::xxh3::xxh3_64_with_seed(body.as_bytes(), seed)
}

/// Syntactic wrappers that carry a declaration's modifiers rather than being
/// blocks in their own right. A resolved block is widened through these so the
/// passage shows the declaration as it is actually written — `export function
/// f()`, not a bare `function f()` that reads as un-exported.
///
/// Language-agnostic by construction: a kind absent from a grammar simply never
/// matches, so one table is safe for every language in the registry.
const WRAPPER_KINDS: &[&str] = &[
    // JavaScript / TypeScript
    "export_statement",
    "export_default_declaration",
    // Python
    "decorated_definition",
];

// ── Public API ────────────────────────────────────────────────────────────

/// An arrow or function expression passed as a call argument (`it(() => {})`,
/// `items.map(function () {})`). Wrappers between the function and `arguments`
/// (`await`, parentheses) still count. A `const f = () => {}` declarator does
/// not.
fn is_call_callback(node: tree_sitter::Node<'_>) -> bool {
    if !CALLBACK_KINDS.contains(&node.kind()) {
        return false;
    }
    let mut cur = node.parent();
    for _ in 0..4 {
        let Some(parent) = cur else {
            return false;
        };
        match parent.kind() {
            "arguments" => return true,
            "parenthesized_expression" | "await_expression" | "unary_expression" => {
                cur = parent.parent();
            }
            _ => return false,
        }
    }
    false
}

/// Find the smallest node enclosing byte range `[start, end)` in `body`
/// whose kind counts as an acceptable semantic block for `language`, and
/// return its own `[start_byte, end_byte)` span.
///
/// Returns `None` — meaning "keep the caller's existing window" — when:
/// - `language` has no registered grammar/kind table here;
/// - `[start, end)` is out of range for `body`;
/// - parsing fails (bad grammar/language mismatch; tree-sitter's `parse`
///   itself is infallible on well-formed UTF-8 input short of a hard
///   internal error, but the API is fallible and we honor that);
/// - no ancestor of the smallest node containing the range has an
///   acceptable kind before the tree root (e.g. the match sits in a
///   top-level `use`/`import` statement, not inside any function/class);
/// - the smallest acceptable block exceeds [`max_block_bytes`] — since any
///   larger enclosing block is only bigger, this stops at the first
///   candidate rather than searching further up;
/// - that block is a call-argument callback bigger than
///   [`NESTED_CALLBACK_MAX_BYTES`] (#2327).
pub fn enclosing_block(
    language: &str,
    body: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    if start > end || end > body.len() {
        return None;
    }
    let (grammar, kinds) = registry_entry(language)?;
    let key = cache_key(language, body);
    let tree = {
        let mut guard = cache().lock();
        guard.get_or_parse(key, &grammar, body)?
    };
    let root = tree.root_node();
    let node = root.descendant_for_byte_range(start, end)?;
    let mut cur = Some(node);
    while let Some(n) = cur {
        if kinds.contains(&n.kind()) {
            // A nested test callback (`it("…", () => { … })`) is an argument,
            // not the function a reader asked for. One larger than a passage
            // falls back to the caller's window (#2327). Walking further up
            // only finds a bigger callback.
            let callback_cap = if is_call_callback(n) {
                NESTED_CALLBACK_MAX_BYTES.min(max_block_bytes())
            } else {
                max_block_bytes()
            };
            // Widen through modifier wrappers so the passage carries the
            // declaration as written. Returning the bare `function_declaration`
            // inside `export function f() {}` would show an agent a function
            // that appears un-exported, and the same applies to a Python
            // definition whose decorators are part of its contract.
            let mut outer = n;
            while let Some(p) = outer.parent() {
                if WRAPPER_KINDS.contains(&p.kind()) && p.start_byte() <= outer.start_byte() {
                    outer = p;
                } else {
                    break;
                }
            }
            let n = outer;
            let (s, e) = (n.start_byte(), n.end_byte());
            if e.saturating_sub(s) > callback_cap {
                return None;
            }
            if s > e || e > body.len() || !body.is_char_boundary(s) || !body.is_char_boundary(e) {
                // Defensive only: tree-sitter node boundaries are always
                // aligned to the UTF-8 byte stream it was fed, so this
                // should be unreachable for valid input.
                return None;
            }
            return Some((s, e));
        }
        cur = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_function_resolves_to_its_own_declaration() {
        let body = "fn before() {\n    1\n}\n\nfn target(x: i32) -> i32 {\n    let y = x + 1;\n    y * 2\n}\n\nfn after() {\n    2\n}\n";
        let needle = "y * 2";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) =
            enclosing_block("rust", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.starts_with("fn target"),
            "expected block to start at the function declaration, got: {block:?}"
        );
        assert!(block.contains(needle));
        assert!(!block.contains("before"));
        assert!(!block.contains("after"));
    }

    #[test]
    fn php_method_resolves_to_its_own_declaration() {
        let body = "<?php\nclass Foo {\n    public function bar() {\n        if (isset($x)) {\n            return 1;\n        }\n    }\n}\n";
        let needle = "isset($x)";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) = enclosing_block("php", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.starts_with("public function bar"),
            "expected block to start at the method declaration, got: {block:?}"
        );
    }

    #[test]
    fn python_function_resolves_to_its_own_declaration() {
        let body = "def before():\n    pass\n\n\ndef target(x):\n    y = x + 1\n    return y\n";
        let needle = "return y";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) =
            enclosing_block("python", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.starts_with("def target"),
            "expected block to start at the function declaration, got: {block:?}"
        );
    }

    #[test]
    fn javascript_function_resolves_to_its_own_declaration() {
        let body = "function before() {\n  return 1;\n}\n\nfunction target(x) {\n  const y = x + 1;\n  return y * 2;\n}\n\nfunction after() {\n  return 2;\n}\n";
        let needle = "y * 2";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) =
            enclosing_block("javascript", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.starts_with("function target"),
            "expected block to start at the function declaration, got: {block:?}"
        );
    }

    #[test]
    fn typescript_namespace_resolves_via_internal_module_kind() {
        // Exercises the `internal_module` kind (TS's actual grammar node for
        // `namespace X {}`, not `namespace_declaration` as probe's own table
        // names it — see the module-level doc comment).
        let body = "namespace Widgets {\n  export function make(x: number): number {\n    return x + 1;\n  }\n}\n";
        let needle = "x + 1";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) =
            enclosing_block("typescript", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.starts_with("export function make"),
            "expected the smallest enclosing block (the function, nested inside the namespace), got: {block:?}"
        );
    }

    #[test]
    fn go_package_level_const_resolves_to_its_declaration() {
        let body =
            "package p\n\nconst (\n\tDefaultTimeout = 30\n\tMaxRetries     = 5\n)\n\nfunc f() {}\n";
        let needle = "MaxRetries";
        let start = body.find(needle).unwrap();
        let (s, e) = enclosing_block("go", body, start, start + needle.len())
            .expect("expected an enclosing block");
        // The innermost entity wins, so a grouped `const (...)` yields the
        // individual spec rather than the whole group — same resolution probe
        // gives, since it also lists both kinds and takes the smallest.
        assert!(
            body[s..e].starts_with("MaxRetries"),
            "expected the const spec, got: {:?}",
            &body[s..e]
        );
    }

    #[test]
    fn java_field_resolves_to_its_declaration_not_the_class() {
        let body = "class A {\n    private final int timeoutMs = 30;\n    void run() {}\n}\n";
        let needle = "timeoutMs";
        let start = body.find(needle).unwrap();
        let (s, e) = enclosing_block("java", body, start, start + needle.len())
            .expect("expected an enclosing block");
        let block = body[s..e].trim_start();
        assert!(
            block.starts_with("private final int timeoutMs"),
            "expected the field declaration, not the enclosing class, got: {block:?}"
        );
    }

    #[test]
    fn python_decorators_are_part_of_the_block() {
        // A decorated definition without its decorators reads as an ordinary
        // function, but `@property` (or a route/auth decorator) is frequently
        // the whole contract — dropping it hands an agent a misleading block.
        let body = "class Cfg:\n    @property\n    def timeout(self):\n        return self._t\n";
        let needle = "self._t";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) =
            enclosing_block("python", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.trim_start().starts_with("@property"),
            "expected the decorator to be part of the block, got: {block:?}"
        );
    }

    #[test]
    fn go_function_resolves_to_its_own_declaration() {
        let body = "package main\n\nfunc before() int {\n\treturn 1\n}\n\nfunc target(x int) int {\n\ty := x + 1\n\treturn y * 2\n}\n";
        let needle = "y * 2";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        let (s, e) = enclosing_block("go", body, start, end).expect("expected an enclosing block");
        let block = &body[s..e];
        assert!(
            block.starts_with("func target"),
            "expected block to start at the function declaration, got: {block:?}"
        );
    }

    #[test]
    fn unsupported_language_falls_back() {
        assert_eq!(
            enclosing_block("cobol", "IDENTIFICATION DIVISION.", 0, 5),
            None
        );
    }

    #[test]
    fn out_of_range_falls_back() {
        let body = "fn f() {}";
        assert_eq!(enclosing_block("rust", body, 0, body.len() + 1), None);
        assert_eq!(enclosing_block("rust", body, 5, 2), None);
    }

    #[test]
    fn top_level_statement_has_no_acceptable_enclosing_node() {
        // A `use` item at module scope is not inside any function/struct/etc,
        // so the walk reaches the tree root without a match.
        let body = "use std::fmt;\n\nfn f() {}\n";
        let start = body.find("use").unwrap();
        let end = start + "use std::fmt;".len();
        assert_eq!(enclosing_block("rust", body, start, end), None);
    }

    /// #2327: a nested test callback under the 16 KiB block cap and over a
    /// passage is not returned whole. A short callback still is, and a
    /// top-level arrow keeps the normal cap.
    #[test]
    fn nested_test_callback_larger_than_a_passage_is_not_a_block() {
        let mut inner = String::from("() => {\n  const CALLBACK_START_SENTINEL = 1;\n");
        for i in 0..70 {
            inner.push_str(&format!(
                "  const pad_{i:03} = \"session-token-handler-pad\";\n"
            ));
        }
        inner.push_str("  const refreshMarker = sessionTokenHandler();\n}\n");
        assert!(
            inner.len() > NESTED_CALLBACK_MAX_BYTES,
            "the callback has to be the oversized case, got {}",
            inner.len()
        );
        assert!(inner.len() < DEFAULT_MAX_BLOCK_BYTES);
        let body = format!(
            "describe(\"session token\", () => {{\n  it(\"refreshes the handler\", {inner});\n}});\n"
        );
        let needle = "refreshMarker";
        let start = body.find(needle).unwrap();
        assert_eq!(
            enclosing_block("typescript", &body, start, start + needle.len()),
            None,
            "a nested callback bigger than a passage must fall back to the window"
        );

        let small =
            "describe(\"s\", () => {\n  it(\"t\", () => {\n    return refreshMarker;\n  });\n});\n";
        let start = small.find(needle).unwrap();
        let (s, e) = enclosing_block("typescript", small, start, start + needle.len())
            .expect("a short callback is still the block");
        let block = &small[s..e];
        assert!(
            block.contains("refreshMarker") && block.len() <= NESTED_CALLBACK_MAX_BYTES,
            "{block:?}"
        );
        assert!(!block.contains("describe"), "{block:?}");

        let mut top = String::from("const refreshSession = () => {\n");
        for i in 0..70 {
            top.push_str(&format!(
                "  const pad_{i:03} = \"session-token-handler-pad\";\n"
            ));
        }
        top.push_str("  return refreshMarker;\n};\n");
        assert!(top.len() > NESTED_CALLBACK_MAX_BYTES);
        assert!(top.len() < DEFAULT_MAX_BLOCK_BYTES);
        let start = top.find(needle).unwrap();
        let (s, e) = enclosing_block("typescript", &top, start, start + needle.len())
            .expect("a top-level arrow is a function, not a callback");
        let block = &top[s..e];
        assert!(
            block.contains("refreshMarker") && block.len() > NESTED_CALLBACK_MAX_BYTES,
            "the whole arrow stays the block: {} bytes",
            block.len()
        );
    }

    #[test]
    fn oversized_block_falls_back_to_caller_window() {
        let mut body = String::from("fn huge() {\n");
        for i in 0..2000 {
            body.push_str(&format!("    let v{i} = {i};\n"));
        }
        body.push_str("}\n");
        let needle = "v5 = 5";
        let start = body.find(needle).unwrap();
        let end = start + needle.len();
        // Sanity: the function really is bigger than the default cap.
        assert!(body.len() > DEFAULT_MAX_BLOCK_BYTES);
        assert_eq!(enclosing_block("rust", &body, start, end), None);
    }

    #[test]
    fn repeated_lookup_on_same_body_hits_the_cache_and_agrees_with_cold_parse() {
        let body = "fn target(x: i32) -> i32 {\n    x + 1\n}\n";
        let start = body.find("x + 1").unwrap();
        let end = start + "x + 1".len();
        let first = enclosing_block("rust", body, start, end);
        let second = enclosing_block("rust", body, start, end);
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn cache_eviction_does_not_change_results() {
        // Push more distinct bodies through than the default cache capacity
        // so the LRU has to evict, then confirm every one still resolves
        // correctly on re-lookup (a bug in the eviction bookkeeping would
        // show up as a stale/aliased tree here).
        let mut bodies = Vec::new();
        for i in 0..(DEFAULT_CACHE_CAPACITY + 16) {
            bodies.push(format!("fn f{i}() {{\n    let v = {i};\n}}\n"));
        }
        for body in &bodies {
            let start = body.find("let v").unwrap();
            let end = body.len() - 2; // just before the closing brace
            let result = enclosing_block("rust", body, start, end);
            assert!(result.is_some(), "expected a resolved block for {body:?}");
            let (s, e) = result.unwrap();
            assert!(body[s..e].starts_with("fn f"));
        }
        // Re-touch the first (now evicted) body once more.
        let body = &bodies[0];
        let start = body.find("let v").unwrap();
        let end = start + "let v = 0".len();
        let result = enclosing_block("rust", body, start, end);
        assert!(result.is_some());
        assert!(result.unwrap().0 == 0);
    }
}
