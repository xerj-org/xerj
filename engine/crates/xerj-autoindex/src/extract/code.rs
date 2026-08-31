//! Source code — AST-aware extraction via tree-sitter.
//!
//! Source files used to fall through to the plain-text extractor, so a repo was
//! searchable only as prose. This parses each file with the matching tree-sitter
//! grammar and captures its DEFINITIONS (functions, classes, methods, structs,
//! traits, interfaces, modules, constants, …). Each file becomes a file-level
//! document carrying:
//! - `language`  the detected language
//! - `symbols`   a structured array of {name, kind, line}
//! - `defs`      "kind name" per symbol, newline-joined so BM25 matches a query
//!   like `class User` or `def save` to the file that owns it
//! - `body`      the full source text (still full-text searchable)
//!
//! PLUS one document PER DECLARATION (#500): `{name, kind, line, code}` under a
//! unique `code:<line>:<name>` locator, so a constant/field/signature lookup
//! retrieves the ~40–80 B declaration line as its own unit instead of only
//! surviving folded inside the enclosing class/method body.
//!
//! The capture-name in each query IS the symbol kind (`@function`, `@class`, …),
//! so adding a language is a grammar dep + one registry row. If a grammar fails
//! to parse a file, it is indexed as plain text rather than dropped.

use super::{ExtractStats, FieldOrigin, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

/// Skip machine-generated giants (minified bundles, generated parsers) — past a
/// couple MB a single "file" is not human code and only bloats the index.
const CODE_CAP: u64 = 2 << 20;

struct LangDef {
    name: &'static str,
    exts: &'static [&'static str],
    language: Language,
    query: Query,
    /// Content probe for extensions claimed by more than one language (#295).
    /// `.m` is both Objective-C and MATLAB: when several registry rows claim
    /// an extension, the first row whose probe returns true wins; a row with
    /// no probe is the fallback owner. Rows with a unique extension leave this
    /// `None` and are never probed.
    probe: Option<fn(&str) -> bool>,
}

/// Is this extension a language we AST-parse? Cheap check used by the sniffer.
pub fn is_code_ext(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    registry().iter().any(|d| d.exts.contains(&e.as_str()))
}

fn def(name: &'static str, exts: &'static [&'static str], lang: Language, q: &str) -> LangDef {
    let query = Query::new(&lang, q).unwrap_or_else(|e| panic!("bad {name} query: {e}"));
    // Text predicates (#eq?/#any-of?/#match?) are NOT applied by the core
    // library — parse_symbols() evaluates them itself. Reject operators it
    // does not implement here, at registry build time, so a typo'd or
    // unsupported predicate fails `all_queries_compile` instead of silently
    // matching everything at extraction time.
    for i in 0..query.pattern_count() {
        for p in query.general_predicates(i) {
            assert!(
                matches!(&*p.operator, "eq?" | "not-eq?" | "any-of?" | "not-any-of?"),
                "{name} query uses unsupported predicate #{}",
                p.operator
            );
        }
    }
    LangDef {
        name,
        exts,
        language: lang,
        query,
        probe: None,
    }
}

/// A `def(...)` whose extension is shared with another language — see
/// `LangDef::probe`.
fn def_probed(
    name: &'static str,
    exts: &'static [&'static str],
    lang: Language,
    q: &str,
    probe: fn(&str) -> bool,
) -> LangDef {
    let mut d = def(name, exts, lang, q);
    d.probe = Some(probe);
    d
}

/// `.m` probe: Objective-C vs MATLAB. Real Objective-C files near-universally
/// carry an `#import`/`@interface`/`@implementation`/`@protocol`/`@end`
/// marker; a `.m` with none of them is treated as MATLAB (the fallback row).
fn looks_like_objc(text: &str) -> bool {
    [
        "#import",
        "@interface",
        "@implementation",
        "@protocol",
        "@end",
    ]
    .iter()
    .any(|m| text.contains(m))
}

fn registry() -> &'static [LangDef] {
    static REG: OnceLock<Vec<LangDef>> = OnceLock::new();
    REG.get_or_init(|| {
        vec![
            def(
                "python",
                &["py", "pyi"],
                tree_sitter_python::LANGUAGE.into(),
                PYTHON_Q,
            ),
            def(
                "javascript",
                &["js", "jsx", "mjs", "cjs"],
                tree_sitter_javascript::LANGUAGE.into(),
                JS_Q,
            ),
            // `.mts`/`.cts` are TypeScript's ESM/CJS extensions — the exact
            // counterparts of the `.mjs`/`.cjs` that javascript already claims
            // below. Omitting them did not merely skip the AST pass: an
            // unclaimed extension is not code to the sniffer at all, so those
            // files fell through to the prose extractor and were chunked into
            // several body-only records with no `language`, `defs` or `symbols`.
            def(
                "typescript",
                &["ts", "mts", "cts"],
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                TS_Q,
            ),
            def(
                "tsx",
                &["tsx"],
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                TS_Q,
            ),
            def("rust", &["rs"], tree_sitter_rust::LANGUAGE.into(), RUST_Q),
            def("go", &["go"], tree_sitter_go::LANGUAGE.into(), GO_Q),
            def("java", &["java"], tree_sitter_java::LANGUAGE.into(), JAVA_Q),
            def("c", &["c", "h"], tree_sitter_c::LANGUAGE.into(), C_Q),
            def(
                "cpp",
                &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
                tree_sitter_cpp::LANGUAGE.into(),
                CPP_Q,
            ),
            def("ruby", &["rb"], tree_sitter_ruby::LANGUAGE.into(), RUBY_Q),
            def("php", &["php"], tree_sitter_php::LANGUAGE_PHP.into(), PHP_Q),
            def(
                "csharp",
                &["cs"],
                tree_sitter_c_sharp::LANGUAGE.into(),
                CSHARP_Q,
            ),
            def(
                "bash",
                &["sh", "bash"],
                tree_sitter_bash::LANGUAGE.into(),
                BASH_Q,
            ),
            // ── #295 Tier 1 ─────────────────────────────────────────────
            def(
                "kotlin",
                &["kt", "kts"],
                tree_sitter_kotlin_ng::LANGUAGE.into(),
                KOTLIN_Q,
            ),
            def(
                "swift",
                &["swift"],
                tree_sitter_swift::LANGUAGE.into(),
                SWIFT_Q,
            ),
            // `.sc` is also SuperCollider; Scala is the statistically dominant
            // owner in the repos agents index, and no SuperCollider grammar is
            // registered, so no probe is needed (same policy as `.h` → C).
            def(
                "scala",
                &["scala", "sc"],
                tree_sitter_scala::LANGUAGE.into(),
                SCALA_Q,
            ),
            def("dart", &["dart"], tree_sitter_dart::LANGUAGE.into(), DART_Q),
            def("lua", &["lua"], tree_sitter_lua::LANGUAGE.into(), LUA_Q),
            // `.pl` is also Prolog. Perl owns it: overwhelmingly dominant in
            // real repos, and no Prolog grammar is registered to probe for.
            def(
                "perl",
                &["pl", "pm"],
                tree_sitter_perl::LANGUAGE.into(),
                PERL_Q,
            ),
            // `.r` is also Rebol; R owns it, same policy as `.pl`.
            def("r", &["r"], tree_sitter_r::LANGUAGE.into(), R_Q),
            def(
                "julia",
                &["jl"],
                tree_sitter_julia::LANGUAGE.into(),
                JULIA_Q,
            ),
            def(
                "haskell",
                &["hs"],
                tree_sitter_haskell::LANGUAGE.into(),
                HASKELL_Q,
            ),
            def(
                "elixir",
                &["ex", "exs"],
                tree_sitter_elixir::LANGUAGE.into(),
                ELIXIR_Q,
            ),
            // ── #295 Tier 2 ─────────────────────────────────────────────
            def(
                "erlang",
                &["erl", "hrl"],
                tree_sitter_erlang::LANGUAGE.into(),
                ERLANG_Q,
            ),
            // OCaml ships separate grammars for implementations and
            // interfaces; `.mli` needs its own row (and query) or interface
            // files would fail to parse and fall back to plain text.
            def(
                "ocaml",
                &["ml"],
                tree_sitter_ocaml::LANGUAGE_OCAML.into(),
                OCAML_Q,
            ),
            def(
                "ocaml_interface",
                &["mli"],
                tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
                OCAML_MLI_Q,
            ),
            def("zig", &["zig"], tree_sitter_zig::LANGUAGE.into(), ZIG_Q),
            // `.m` collision (issue #295 decision): Objective-C vs MATLAB is
            // the one collision where both owners are registered, so it is
            // resolved by content probe — objc markers win, MATLAB is the
            // fallback row below. `.mm` (Objective-C++) is claimed too: the
            // objc grammar parses the Objective-C subset and a file it cannot
            // parse still indexes as plain text.
            def_probed(
                "objc",
                &["m", "mm"],
                tree_sitter_objc::LANGUAGE.into(),
                OBJC_Q,
                looks_like_objc,
            ),
            def(
                "groovy",
                &["groovy", "gradle"],
                tree_sitter_groovy::LANGUAGE.into(),
                GROOVY_Q,
            ),
            def(
                "powershell",
                &["ps1", "psm1", "psd1"],
                tree_sitter_powershell::LANGUAGE.into(),
                POWERSHELL_Q,
            ),
            def(
                "fsharp",
                &["fs", "fsx"],
                tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
                FSHARP_Q,
            ),
            def("nix", &["nix"], tree_sitter_nix::LANGUAGE.into(), NIX_Q),
            // ── #295 Tier 3 ─────────────────────────────────────────────
            // Free-form Fortran only: `.f` (fixed-form) is deliberately NOT
            // claimed — the grammar is free-form, a fixed-form file would
            // parse to garbage, and an unclaimed extension still indexes as
            // text, which is strictly better than a wrong AST.
            def(
                "fortran",
                &["f90", "f95", "f03"],
                tree_sitter_fortran::LANGUAGE.into(),
                FORTRAN_Q,
            ),
            // MATLAB is the probe-less fallback for `.m` — must sort after
            // the objc row (first passing probe wins, probe-less row catches
            // the rest).
            def(
                "matlab",
                &["m"],
                tree_sitter_matlab::LANGUAGE.into(),
                MATLAB_Q,
            ),
            def(
                "solidity",
                &["sol"],
                tree_sitter_solidity::LANGUAGE.into(),
                SOLIDITY_Q,
            ),
        ]
    })
}

pub fn extract(path: &Path, sn: &crate::sniff::Sniffed, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    // Grammar lookup and title are keyed on the LOGICAL name from `Sniffed`,
    // never on `path`: durable preparation reads content-addressed snapshot
    // blobs (`blobs/00000000`), so an extension recovered from the content
    // path is empty there and classified every code file as junk (#294).
    let named = sn.logical_name.as_deref().unwrap_or(path);
    let ext = named
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let claimants: Vec<&LangDef> = registry()
        .iter()
        .filter(|d| d.exts.contains(&ext.as_str()))
        .collect();
    if claimants.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }
    let Some(bytes) = super::read_whole(path, false, CODE_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    // Shared extensions (`.m`): first claimant whose content probe passes wins;
    // the probe-less row is the fallback. Single-claimant extensions skip the
    // probe entirely.
    let def = if claimants.len() == 1 {
        claimants[0]
    } else {
        claimants
            .iter()
            .find(|d| d.probe.map(|p| p(&text)).unwrap_or(true))
            .unwrap_or(&claimants[0])
    };

    // Parse + capture definitions. A parse failure (or an over-deep tree) is not
    // fatal — index the file as plain text so its content is still searchable.
    let symbols = parse_symbols(def, &text).unwrap_or_default();
    emit_code_doc(named, def.name, &text, &symbols, &mut stats, sink);
    Ok(stats)
}

/// One captured declaration — the unit #500 made independently retrievable.
///
/// `code` is the DECLARATION: from its first token (leading attributes
/// included) through the end of its signature, stopping where the body begins.
/// For a constant / field / `#define` / signature-only entry that is the whole
/// declaration (~40–80 B); for a method or class it is the complete signature,
/// which is the half #500 still had wrong — the field used to be *whichever
/// single physical line the NAME node happened to sit on*, so every declaration
/// that wraps returned a fragment (`public void configure(`) instead of an
/// answer.
///
/// "Stopping where the body begins" needs the grammar to say where that is, and
/// a few do not: tree-sitter-haskell names no body node, no `body:` field and no
/// terminator, so a Haskell equation's slice IS the equation. Measured on the
/// four corpora in `decl_bench`, a slice that covers its symbol's whole span
/// (i.e. carries the implementation) is 1.5 % of Lucene, 2.5 % of valkey +
/// memcached, 1.2 % of tantivy and 5.9 % of a 914 k-symbol Go + C tree — small,
/// but not zero, and `DECL_CAP` is what bounds it.
///
/// The implementation is not thrown away: the file document keeps the whole
/// source in `body`, and `line`..`end_line` says exactly which lines the body
/// occupies, so a caller that wants it can slice it instead of guessing the
/// span from where the next symbol starts.
#[derive(Debug)]
struct Symbol {
    name: String,
    kind: String,
    /// 1-based line the NAME sits on — the grep-equivalent citation line.
    /// `code` may begin one or two lines above it (a C++ `static void\nfoo()`,
    /// a Rust `#[inline]`), which is why the slice is not derived from it.
    line: usize,
    /// 1-based last line of the declaration INCLUDING its body.
    end_line: usize,
    /// The declaration, signature only — see the type docs.
    code: String,
}

/// Chars a stored `code` slice may occupy. Unchanged from the single-line
/// version on purpose: the declaration slice must not be able to cost more than
/// the fragment it replaces, so a pathological declaration is bounded, never
/// unbounded.
const DECL_CAP: usize = 400;
/// Attribute siblings a declaration may absorb before the walk gives up.
const ATTR_MAX_HOPS: usize = 8;
/// Levels the walk from the captured name up to its declaration may climb, and
/// nodes the body search may visit. Both are just runaway guards.
const DECL_MAX_DEPTH: usize = 12;
const BODY_SCAN_BUDGET: usize = 4096;
/// Nodes the "does a previous sibling hold an implementation?" look may visit,
/// summed over one symbol's whole climb.
const SIBLING_SCAN_BUDGET: usize = 256;
/// Bytes a raw slice may occupy before it is copied and dedented. A slice this
/// long cannot survive `DECL_CAP` anyway (400 chars is at most 1 600 bytes plus
/// the indentation `dedent` removes), so this only stops the COPY from being
/// O(symbols x file) on a grammar whose scope this heuristic still climbs past.
const SLICE_MAX_BYTES: usize = DECL_CAP * 32;

/// Does this node kind hold a declaration's IMPLEMENTATION rather than its
/// signature? 34 grammars spell that node ~20 ways (`block`, `statement_block`,
/// `compound_statement`, `class_body`, `body_statement`, `function_body`,
/// `declaration_list`, `field_declaration_list`, `do_block`, `suite`, …), so
/// match the shape rather than enumerate them. A spelling this misses only
/// makes the slice longer, and `DECL_CAP` then bounds it.
///
/// A spelling this WRONGLY matches is not symmetric, though: it ends the slice
/// early and stores a mid-signature fragment — the exact defect the slice
/// exists to remove — so the kinds that merely contain one of those words while
/// living inside the SIGNATURE have to be named and rejected:
///   * `block_parameter` / `block_argument`: tree-sitter-ruby declares these
///     inside `method_parameters`, so `def each(&blk)` otherwise stores
///     `def each(`;
///   * `block_pointer_declarator` / `abstract_block_pointer_declarator`: the
///     Objective-C block-typed parameter, `- (void)run:(void (^)(int))cb`;
///   * `block_comment`: a `/* … */` between the name and the body, in
///     tree-sitter rust, java, kotlin, scala, dart, groovy, julia and fsharp.
fn is_body_kind(kind: &str) -> bool {
    if kind.contains("parameter")
        || kind.contains("argument")
        || kind.contains("declarator")
        || kind.contains("comment")
    {
        return false;
    }
    kind.contains("body")
        || kind.contains("block")
        || kind.ends_with("declaration_list")
        || kind == "compound_statement"
        || kind == "suite"
}

/// Kinds that hold MANY declarations — the scope a declaration lives in, and so
/// where the climb from a captured name must stop. Every body is a container;
/// these are the extra spellings for scopes that are not bodies (Haskell's
/// `declarations`, a nested module).
///
/// Missing one is NOT free, which is why the list below is long and every entry
/// was read off a real ancestor chain rather than guessed: the climb runs past
/// the declaration into the enclosing scope, so `code` starts at
/// `@implementation` / `#ifdef` / `const api = {` and `end_line` becomes the
/// last line of the whole class, the `#endif` or the object literal instead of
/// the symbol's own.
fn is_container_kind(lang: &str, kind: &str) -> bool {
    if is_body_kind(kind)
        || kind.ends_with("declarations")
        || kind.ends_with("statements")
        || kind.ends_with("statement_list")
    {
        return true;
    }
    if matches!(
        kind,
        "source"
            | "source_file"
            | "translation_unit"
            | "program"
            | "module"
            | "compilation_unit"
            | "source_code"
    ) {
        return true;
    }
    matches!(
        kind,
        // Objective-C: `@interface` holds its method DECLARATIONS as direct
        // children, and `@implementation` holds an `implementation_definition`
        // that holds the method definitions. (`class_implementation` itself is
        // deliberately NOT here: it is the class's own declaration node.)
        "class_interface"
            | "category_interface"
            | "protocol_declaration"
            | "implementation_definition"
            // C / C++ / Objective-C conditional compilation. A `#ifdef`-guarded
            // function's parent is the whole guarded region, the functions
            // before it and their bodies included.
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_ifndef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "preproc_elifndef"
            // JavaScript / TypeScript: the methods of an object literal.
            | "object"
            // MATLAB `classdef` sections.
            | "methods"
            | "properties"
            | "events"
            // Fortran's `contains` section.
            | "internal_procedures"
            | "module_procedures"
            // Nix attribute set.
            | "binding_set"
            // Go's parenthesised `var ( … )` group. (Its `const ( … )` twin has
            // no list node — see `is_container`.)
            | "var_spec_list"
            | "const_spec_list"
    ) || (lang == "zig"
        // Zig spells its scopes `*_declaration`, which in C# is a DECLARATION of
        // a type rather than a scope (C#'s `struct_declaration` has a
        // `declaration_list` body, already a container above). So this group has
        // to be language-scoped, or a C# struct loses its modifiers.
        && matches!(
            kind,
            "struct_declaration"
                | "enum_declaration"
                | "union_declaration"
                | "opaque_declaration"
                | "error_set_declaration"
                | "container_declaration"
        ))
}

/// `is_container_kind` for the cases where the KIND alone cannot say. Go spells
/// `const X = 1` and `const ( X = 1\n Y = 2 )` with the same node — and the same
/// again for `var` and `type` — so what separates a declaration from a scope
/// there is the parenthesis, not the node's name.
fn is_container(lang: &str, n: Node<'_>) -> bool {
    if is_container_kind(lang, n.kind()) {
        return true;
    }
    if lang != "go"
        || !matches!(
            n.kind(),
            "const_declaration" | "var_declaration" | "type_declaration"
        )
    {
        return false;
    }
    let mut cur = n.walk();
    for child in n.children(&mut cur) {
        if child.kind() == "(" {
            return true;
        }
    }
    false
}

/// Is this node, or anything under it, an implementation? Bounded, and used
/// only to tell a SIBLING DECLARATION (which has a body) apart from the
/// `modifiers` / return-type / attribute siblings that are part of the
/// declaration being sliced (which do not).
fn holds_body(node: Node<'_>, budget: &mut usize) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if *budget == 0 {
            return false; // fail open: `DECL_CAP` bounds the slice anyway
        }
        *budget -= 1;
        if is_body_kind(n.kind()) {
            return true;
        }
        let mut cur = n.walk();
        for child in n.children(&mut cur) {
            stack.push(child);
        }
    }
    false
}

/// Attribute/annotation/decorator nodes that sit BESIDE a declaration rather
/// than inside it (Rust `#[inline]`, C# `[Obsolete]`) — part of the
/// declaration for a reader, so part of the slice. Java annotations and Python
/// decorators are already children of the declaration node and need no help.
fn is_attribute_kind(kind: &str) -> bool {
    // tree-sitter-erlang spells `-module(m).` / `-export([f/0]).` as
    // `module_attribute`: a complete declaration of its own that happens to
    // carry the word, not a modifier of whatever follows it.
    kind != "module_attribute"
        && (kind.contains("attribute") || kind.contains("annotation") || kind.contains("decorator"))
}

/// The declaration node a captured NAME belongs to: climb until the parent is a
/// scope container (a body/block/list) or the tree root. Shape-based because
/// the capture depth of the name differs per grammar — Rust captures the
/// `identifier` directly under `function_item`, Java goes
/// `field_declaration > variable_declarator > identifier`, TypeScript
/// `export_statement > lexical_declaration > variable_declarator > identifier`.
fn declaration_node<'t>(lang: &str, name: Node<'t>) -> Node<'t> {
    let mut decl = name;
    let mut budget = SIBLING_SCAN_BUDGET;
    for _ in 0..DECL_MAX_DEPTH {
        let Some(parent) = decl.parent() else { break };
        // Stop *below* the container: the container's other children are the
        // sibling declarations, which are emphatically not part of this one.
        if parent.parent().is_none() || is_container(lang, parent) {
            // A scope that is ALSO this declaration: Objective-C's
            // `@interface Foo : NSObject` holds its method declarations as
            // direct children, so the class name's own parent is the scope.
            // Stopping below it would slice the class down to the bare name.
            if decl.id() == name.id() && parent.parent().is_some() {
                decl = parent;
            }
            break;
        }
        // The same stop, for a scope shape `is_container_kind` was never told
        // about: a parent in which something BEFORE us already carries an
        // implementation is a scope whatever it is called, and climbing into it
        // would start this symbol's slice inside the previous symbol's body.
        if std::iter::successors(decl.prev_sibling(), |n| n.prev_sibling())
            .any(|p| holds_body(p, &mut budget))
        {
            break;
        }
        decl = parent;
    }
    decl
}

/// The first token of the declaration, attributes included: walk back over
/// contiguous attribute siblings so `#[inline]\npub fn f()` is one declaration.
fn declaration_head<'t>(decl: Node<'t>, text: &str) -> Node<'t> {
    let mut head = decl;
    for _ in 0..ATTR_MAX_HOPS {
        let Some(prev) = head.prev_sibling() else {
            break;
        };
        if !is_attribute_kind(prev.kind())
            || !gap_is_blank(text, prev.end_byte(), head.start_byte())
        {
            break;
        }
        head = prev;
    }
    head
}

/// Is everything between two byte offsets whitespace, and at most one newline?
/// One newline = "the line directly above"; two = a blank line between them,
/// which in every language convention means the two are not one declaration.
fn gap_is_blank(text: &str, from: usize, to: usize) -> bool {
    let Some(gap) = text.get(from..to) else {
        return false;
    };
    gap.chars().all(char::is_whitespace) && gap.matches('\n').count() <= 1
}

/// Where the declaration's implementation starts, as a byte offset: the first
/// body-shaped node inside `decl` that begins at or after the captured name —
/// or, for the grammars that name no body node at all, the `body:` FIELD, and
/// failing that the terminator of an `end`-delimited definition. Document
/// order, so a lambda nested deeper in the body can never win over the body
/// itself.
fn body_start(lang: &str, decl: Node<'_>, after: usize) -> Option<usize> {
    body_by_kind(lang, decl, after)
        .or_else(|| body_by_field(decl, after).map(|b| b.start_byte()))
        .or_else(|| end_delimited_head(decl))
}

/// Where a scope's HEAD ends: the first child that begins on a row BELOW the
/// scope's own first row. `@interface Foo : NSObject`, `const S = struct {`,
/// `classdef A` + its `methods` opener are all this shape — the members start
/// on the next line, so everything above them is the head.
fn container_head_end(n: Node<'_>) -> Option<usize> {
    let row = n.start_position().row;
    let mut cur = n.walk();
    for child in n.children(&mut cur) {
        if child.start_position().row > row {
            return Some(child.start_byte());
        }
    }
    None
}

/// A parameter or argument list is part of the SIGNATURE, so nothing inside one
/// is this declaration's implementation — the `block` in `sort(xs, (a, b) -> {
/// … })` and the `body:` of `Comparator.comparingInt(d -> d.doc)` both belong to
/// a lambda that is an argument, and stopping at either cuts the declaration in
/// half. Neither body search descends into one.
fn is_signature_scope(kind: &str) -> bool {
    kind.contains("argument") || kind.contains("parameter")
}

/// Does this node kind DEFINE something, so that its `body:` field is an
/// implementation rather than a value? `function_definition` (r), `let_binding`
/// (ocaml), `function_or_value_defn` (fsharp), `function_expression` (nix) and
/// `arrow_function` (js/ts) do; `composite_literal` (go) and
/// `lambda_expression` (java) do not.
fn is_definition_kind(kind: &str) -> bool {
    kind.contains("definition")
        || kind.contains("declaration")
        || kind.contains("binding")
        || kind.contains("defn")
        || kind.contains("function")
        || kind.contains("method")
}

/// Fortran and Julia name no body node and no `body:` field: a definition is a
/// signature, then statements, then a terminator (`end`, `end subroutine f`).
/// Recognising that terminator is what separates them from a WRAPPED
/// declaration with no body at all (a two-line C prototype), where the first
/// child on the next row is still part of the signature.
fn end_delimited_head(decl: Node<'_>) -> Option<usize> {
    let last = decl.child(u32::try_from(decl.child_count().checked_sub(1)?).ok()?)?;
    if last.kind() == "end" || last.kind().starts_with("end_") {
        container_head_end(decl)
    } else {
        None
    }
}

/// The `body:` field, for the grammars that name no body NODE: tree-sitter r
/// (`function_definition body: braced_expression`), nix (`function_expression
/// body:`), ocaml (`let_binding body:`) and fsharp (`function_or_value_defn
/// body:`) all mark it this way. Without it those languages have no stopping
/// point at all and `code` runs to the end of the whole definition.
///
/// A `body:` child of the same kind as its parent is a CURRIED lambda
/// (`f = a: b: a + b` in Nix) — stopping there would cut the signature in half,
/// so keep descending through those.
///
/// And the parent has to BE a definition. Go's `composite_literal` marks its
/// `{ … }` with `body:` too, so without this a `var Config = map[string]int{…}`
/// would store `Config = map[string]int` and drop the value it declares.
fn body_by_field<'t>(decl: Node<'t>, after: usize) -> Option<Node<'t>> {
    let mut best: Option<Node<'t>> = None;
    let mut stack: Vec<Node<'t>> = vec![decl];
    let mut budget = BODY_SCAN_BUDGET;
    while let Some(n) = stack.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if n.end_byte() <= after || best.is_some_and(|b| n.start_byte() >= b.start_byte()) {
            continue;
        }
        let mut cur = n.walk();
        if !cur.goto_first_child() {
            continue;
        }
        loop {
            let child = cur.node();
            if cur.field_name() == Some("body")
                && child.start_byte() >= after
                && child.kind() != n.kind()
                && is_definition_kind(n.kind())
                && best.is_none_or(|b| child.start_byte() < b.start_byte())
            {
                best = Some(child); // never descend INTO a body
            } else if !is_signature_scope(child.kind()) {
                stack.push(child);
            }
            if !cur.goto_next_sibling() {
                break;
            }
        }
    }
    best
}

/// The body-shaped node, or the multi-line SCOPE, that ends the declaration.
///
/// A scope counts because several grammars put the members straight inside one
/// with no body node around them — Zig's `const S = struct { … }`, an
/// Objective-C `@interface`, a MATLAB `methods` section, a JavaScript object
/// literal. The slice then stops at that scope's first member rather than at
/// the scope's own first byte, so `const S = struct {` keeps its `struct {`.
/// Only a MULTI-ROW scope qualifies: an inline `{ a: 1 }` default argument is a
/// one-row `object`, and stopping there would truncate the signature.
fn body_by_kind(lang: &str, decl: Node<'_>, after: usize) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None; // (node start, slice end)
    let mut stack: Vec<Node<'_>> = vec![decl];
    let mut budget = BODY_SCAN_BUDGET;
    while let Some(n) = stack.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        // Prune: entirely before the name, or already beaten.
        if n.end_byte() <= after || best.is_some_and(|(b, _)| n.start_byte() >= b) {
            continue;
        }
        if n.id() != decl.id() && n.start_byte() >= after {
            if is_body_kind(n.kind()) {
                best = Some((n.start_byte(), n.start_byte()));
                continue; // never descend INTO a body
            }
            if is_container(lang, n) && n.end_position().row > n.start_position().row {
                let end = container_head_end(n).unwrap_or_else(|| n.start_byte());
                best = Some((n.start_byte(), end));
                continue; // nor into someone else's scope
            }
        }
        let mut cur = n.walk();
        for child in n.children(&mut cur) {
            if !is_signature_scope(child.kind()) {
                stack.push(child);
            }
        }
    }
    // `decl` is itself the scope holding the members (an Objective-C
    // `@interface`, whose class name is a direct child of it).
    best.map(|(_, end)| end).or_else(|| {
        is_container(lang, decl)
            .then(|| container_head_end(decl))
            .flatten()
    })
}

/// Trim a multi-line slice for storage: drop blank edge lines, strip trailing
/// whitespace, and remove the indentation the whole block shares, so a method
/// nested four levels deep does not pay for 16 leading spaces per line.
fn dedent(slice: &str) -> String {
    let lines: Vec<&str> = slice.lines().map(str::trim_end).collect();
    let (Some(first), Some(last)) = (
        lines.iter().position(|l| !l.is_empty()),
        lines.iter().rposition(|l| !l.is_empty()),
    ) else {
        return String::new();
    };
    let block = &lines[first..=last];
    // CHAR count of the shared leading whitespace, not a byte count. A byte
    // count is what `l.len() - l.trim_start().len()` gives, and `trim_start`
    // trims Unicode whitespace, so a block mixing ASCII indentation with U+00A0
    // or U+3000 could put the minimum mid-character and blank that line out.
    // Skipping N chars is on a boundary by construction.
    let indent = block
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);
    block
        .iter()
        .map(|l| {
            let mut cs = l.chars();
            for _ in 0..indent {
                cs.next();
            }
            cs.as_str()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Cap at a char (never byte) boundary, preferring the last whole line that
/// fits so a truncated slice is still readable code.
fn cap_chars(text: String, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text;
    }
    let cut: String = text.chars().take(cap).collect();
    match cut.rfind('\n') {
        Some(nl) if nl > cap / 2 => cut[..nl].to_string(),
        _ => cut,
    }
}

/// Extend a byte offset back over the whitespace indenting its line, so a
/// multi-line slice arrives at `dedent` as a whole block (without this the
/// first line carries no indentation and the shared-indent minimum is 0).
fn snap_to_indent(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = start;
    while at > 0 && (bytes[at - 1] == b' ' || bytes[at - 1] == b'\t') {
        at -= 1;
    }
    at
}

/// Evaluate a pattern's text predicates against one match. The core library
/// exposes `#eq?`/`#any-of?` (and their `not-` forms) as *general* predicates
/// and applies NONE of them itself — without this, the Elixir query would tag
/// every function call in the file as a definition. Operators are validated
/// in `def()`; an unknown one landing here rejects the match (fail closed).
fn predicates_hold(query: &Query, m: &tree_sitter::QueryMatch, text: &str) -> bool {
    use tree_sitter::QueryPredicateArg as Arg;
    let cap_text = |ix: u32| -> &str {
        m.captures
            .iter()
            .find(|c| c.index == ix)
            .and_then(|c| text.get(c.node.byte_range()))
            .unwrap_or("")
    };
    query.general_predicates(m.pattern_index).iter().all(|p| {
        let mut args = p.args.iter();
        let Some(Arg::Capture(first)) = args.next() else {
            return false;
        };
        let got = cap_text(*first);
        let strings: Vec<&str> = p
            .args
            .iter()
            .skip(1)
            .filter_map(|a| match a {
                Arg::String(s) => Some(&**s),
                Arg::Capture(c) => text.get(
                    m.captures
                        .iter()
                        .find(|x| x.index == *c)
                        .map(|x| x.node.byte_range())?,
                ),
            })
            .collect();
        match &*p.operator {
            "eq?" | "any-of?" => strings.contains(&got),
            "not-eq?" | "not-any-of?" => !strings.contains(&got),
            _ => false,
        }
    })
}

/// #852: does `node` have an ancestor of the given tree-sitter kind? Used to
/// reject captures nested inside a `compound_statement` (a function body).
fn has_ancestor_kind(node: tree_sitter::Node, kind: &str) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == kind {
            return true;
        }
        cur = n.parent();
    }
    false
}

/// #820 item 3: is the `declaration` enclosing this C++ `@const` capture
/// actually immutable? `CPP_Q`'s `init_declarator` patterns capture every
/// initialized top-level/namespace declaration alike — walk up to the
/// nearest `declaration` ancestor and look for a `type_qualifier` child
/// spelled `const`/`constexpr`/`consteval`/`constinit` (the grammar's
/// `type_qualifier` choice, verified in node-types.json). `volatile`,
/// `restrict`, `_Atomic`, `mutable`, etc. — and no qualifier at all — are
/// equally non-constant. No `declaration` ancestor (an enumerator, a `static
/// const` field member) is never reached by the caller — see the
/// `has_ancestor_kind(.., "init_declarator")` guard at the call site.
fn cpp_declarator_is_const_qualified(node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "declaration" {
            let mut walker = n.walk();
            return n.children(&mut walker).any(|child| {
                child.kind() == "type_qualifier"
                    && matches!(
                        child.utf8_text(source),
                        Ok("const") | Ok("constexpr") | Ok("consteval") | Ok("constinit")
                    )
            });
        }
        cur = n.parent();
    }
    false
}

fn parse_symbols(def: &LangDef, text: &str) -> Option<Vec<Symbol>> {
    let mut parser = Parser::new();
    parser.set_language(&def.language).ok()?;
    let tree = parser.parse(text.as_bytes(), None)?;
    let names = def.query.capture_names();
    // Row → source line, built once (not `lines().nth(row)` per symbol, which
    // would be O(symbols · file)).
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Symbol> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&def.query, tree.root_node(), text.as_bytes());
    while let Some(m) = it.next() {
        if !predicates_hold(&def.query, m, text) {
            continue;
        }
        for cap in m.captures {
            let mut kind = names[cap.index as usize]; // capture name == symbol kind
            if kind.starts_with('_') {
                continue; // predicate-only capture (`@_kw`), not a symbol
            }
            let node = cap.node;
            // #852: every C++ pattern targets file/namespace/class-scope API
            // symbols — none intends to capture a function-local definition. A
            // type/class/struct/enum or its enumerators DEFINED INSIDE a
            // function body (e.g. `void f(){ struct S{ enum E{A}; }; }`) still
            // matched the unanchored `@class`/`@struct`/`@enum` patterns and the
            // `field_declaration_list` enum-const anchor, leaking locals as
            // symbols. tree-sitter has no negative-ancestor test, so drop any
            // C++ capture nested under a `compound_statement` (a function body).
            if def.name == "cpp" && has_ancestor_kind(node, "compound_statement") {
                continue;
            }
            // #820 item 3: `CPP_Q`'s `init_declarator` patterns tag every
            // initialized top-level/namespace declaration `@const`, mutable or
            // not. The `init_declarator` ancestor is unique to those patterns
            // (an enumerator or a `static const` field member never has one),
            // so it also selects exactly the captures this relabel applies to.
            if def.name == "cpp"
                && kind == "const"
                && has_ancestor_kind(node, "init_declarator")
                && !cpp_declarator_is_const_qualified(node, text.as_bytes())
            {
                kind = "static";
            }
            let name = text.get(node.byte_range()).unwrap_or("").trim();
            if name.is_empty() || name.len() > 200 {
                continue;
            }
            let row = node.start_position().row;
            // #500, second half: slice the DECLARATION, not the physical line
            // the name sits on. `public HnswGraphBuilder(\n    int M, int
            // beamWidth, …)` used to be stored as `public HnswGraphBuilder(` —
            // a fragment that answers nothing — and a declaration whose
            // modifiers precede the name (`static void\nfoo()`) stored the
            // wrong line entirely. Slice [declaration head .. body start), so
            // the unit is the complete signature rather than a fragment of it.
            let decl = declaration_node(def.name, node);
            let head = declaration_head(decl, text);
            let from = snap_to_indent(text, head.start_byte());
            let to = body_start(def.name, decl, node.end_byte()).unwrap_or_else(|| decl.end_byte());
            // Size the slice BEFORE copying it: `dedent` allocates, and a slice
            // this long cannot survive `DECL_CAP`, so there is nothing to gain
            // by copying it first.
            let mut code = if to.saturating_sub(from) > SLICE_MAX_BYTES {
                String::new()
            } else {
                text.get(from..to.max(from)).map(dedent).unwrap_or_default()
            };
            if code.is_empty() || code.chars().count() > DECL_CAP {
                // Over the cap (a lookup-table constant, a minified line, or a
                // grammar whose body node `is_body_kind` does not recognise):
                // fall back to the single start line. Bounded either way — the
                // slice can never cost more than the fragment it replaced.
                code = lines.get(row).copied().unwrap_or("").trim().to_string();
            }
            let code = cap_chars(code, DECL_CAP);
            out.push(Symbol {
                name: name.to_string(),
                kind: kind.to_string(),
                line: row + 1,
                end_line: decl.end_position().row + 1,
                code,
            });
            if out.len() >= 5000 {
                return Some(out); // pathological generated file — enough
            }
        }
    }
    Some(out)
}

fn emit_code_doc(
    path: &Path,
    language: &str,
    text: &str,
    symbols: &[Symbol],
    stats: &mut ExtractStats,
    sink: Sink,
) -> bool {
    let title = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("source")
        .to_string();

    let mut fields = Map::new();
    fields.insert("title".into(), Value::String(title));
    fields.insert("language".into(), Value::String(language.to_string()));

    if !symbols.is_empty() {
        // Searchable "kind name" list — this is what makes `class User` /
        // `def save` retrieve the file, and it dedups repeated names.
        let mut seen = std::collections::HashSet::new();
        let defs: Vec<String> = symbols
            .iter()
            .filter(|s| seen.insert((s.kind.clone(), s.name.clone())))
            .map(|s| format!("{} {}", s.kind, s.name))
            .collect();
        fields.insert("defs".into(), Value::String(defs.join("\n")));
        let arr: Vec<Value> = symbols
            .iter()
            .map(|s| {
                let mut m = Map::new();
                m.insert("name".into(), Value::String(s.name.clone()));
                m.insert("kind".into(), Value::String(s.kind.clone()));
                m.insert("line".into(), Value::Number((s.line as u64).into()));
                // #500: a client that wants the IMPLEMENTATION can now slice
                // exactly `line`..`end_line` out of `body`. Without it the only
                // way to bound a symbol was "up to where the next one starts",
                // which returns a whole class for a class and the rest of the
                // file for the last symbol — the coarse unit this issue is about.
                m.insert("end_line".into(), Value::Number((s.end_line as u64).into()));
                Value::Object(m)
            })
            .collect();
        fields.insert("symbols".into(), Value::Array(arr));
        fields.insert(
            "symbol_count".into(),
            Value::Number((symbols.len() as u64).into()),
        );
    }
    // The full source stays full-text searchable.
    fields.insert("body".into(), Value::String(text.to_string()));

    stats.records += 1;
    if !sink(RawRecord {
        fields,
        locator: "code".into(),
        group: None,
        // `defs`/`symbols`/`symbol_count` appear only when this extractor finds
        // something, so a better grammar would otherwise move the file to a
        // different dataset and orphan its old document (#178).
        origin: FieldOrigin::Extractor,
    }) {
        return false;
    }

    // #500: promote each declaration to its OWN retrievable document. A
    // constant/field/`#define`/signature lookup then returns the ~40–80 B
    // declaration line with an exact `name` (mapped keyword), instead of only
    // surviving folded inside the ~2–3 KB enclosing class/method body (the
    // measured 32–48× byte blow-up + the rank-35 recall miss). The file
    // document above keeps `body`/`defs` for full-text and cross-file search.
    let file_path = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("source")
        .to_string();
    // A single declaration can be captured under two kinds at the same line+name
    // (e.g. `export const f = () => {}` → @function + @const). Those share the
    // `code:<line>:<name>` locator → the same `doc_id` → ES stores ONE document.
    // The emitted RECORD count MUST match, or the count-reconciliation barrier
    // (sync_executor) sees "sealed N vs read-back N-1" and aborts the whole run.
    // So dedup by locator here, BEFORE counting/emitting — do not rely on the
    // downstream `_id` overwrite (which fixes the doc but not the count).
    let mut emitted_locators = std::collections::HashSet::new();
    for s in symbols {
        if s.code.is_empty() {
            continue;
        }
        let (name, line) = (&s.name, s.line);
        let locator = format!("code:{line}:{name}");
        if !emitted_locators.insert(locator.clone()) {
            continue;
        }
        let mut sf = Map::new();
        // `title` = the symbol so the hit reads as the declaration, `path` for
        // citation, `name`/`code` searchable (the mapping exposes `name.keyword`
        // for exact-identifier ranking), `kind`/`line` for filtering + citation.
        sf.insert("title".into(), Value::String(name.clone()));
        sf.insert("path".into(), Value::String(file_path.clone()));
        sf.insert("language".into(), Value::String(language.to_string()));
        sf.insert("name".into(), Value::String(name.clone()));
        sf.insert("kind".into(), Value::String(s.kind.clone()));
        sf.insert("line".into(), Value::Number((line as u64).into()));
        sf.insert("code".into(), Value::String(s.code.clone()));
        // No field beyond the five #579 froze: a NEW scalar field on this
        // document would make `reconcile_plan::ensure_compatible` (see
        // reconcile_plan.rs, "field {name} is absent from frozen dataset")
        // abort the next incremental run over an index an older binary froze.
        // `end_line` therefore rides in the file document's `symbols` array,
        // which that check exempts because an array carries no scalar
        // observations (the #580/#581 rule).
        stats.records += 1;
        if !sink(RawRecord {
            fields: sf,
            // Unique, stable per-declaration locator so `doc_id(dataset,
            // file_key, locator)` gives each symbol its OWN document instead of
            // colliding with the file document (all of which used "code"). Keyed
            // by line+name: unique per declaration, stable across re-index unless
            // the line moves (deduped above so the record count matches the doc
            // count).
            locator,
            group: None,
            origin: FieldOrigin::Extractor,
        }) {
            return false;
        }
    }
    true
}

// ── Per-language capture queries. Capture-name == emitted symbol kind. ─────────

const PYTHON_Q: &str = r#"
(function_definition name: (identifier) @function)
(class_definition name: (identifier) @class)
(module (expression_statement (assignment left: (identifier) @const right: (_))))
"#;

// The last pattern is the JavaScript half of #170, and it is the same hole
// #285 closed for TypeScript: an ESM module whose public surface is
// `export const x = someBuilder(...)` — a schema, a route table, a config
// object — is not a function value, so the two function-valued patterns above
// miss it and the module extracts ZERO symbols.
//
// Deliberately identical in shape to the TS_Q pattern, and for the same three
// reasons: exported-only because `defs` is the cross-file retrieval surface
// (the broader scope was measured in #285 as weight without retrieval gain);
// anchored to `program` because `lexical_declaration` is the same node inside a
// function body, so an unanchored form would fill `defs` with function-local
// `const` (the 90%-locals trap measured for C in #170); and the `"const"` token
// matched so `export let` — mutable module state — stays out.
//
// The kind overlap is WIDER here than in TypeScript and is asserted, not
// assumed: JS_Q's function-valued pattern admits `function_expression` as well
// as `arrow_function`, so `export const f = function () {}` carries both
// `@function` and `@const`, where the TS equivalent only does so for arrows.
// `defs` dedups on (kind, name), so each spelling still answers its own search.
const JS_Q: &str = r#"
(function_declaration name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(class_declaration name: (identifier) @class)
(method_definition name: (property_identifier) @method)
(variable_declarator name: (identifier) @function value: [(arrow_function) (function_expression)])
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @function value: (arrow_function))))
(program (export_statement declaration: (lexical_declaration "const" (variable_declarator name: (identifier) @const))))
"#;

// The last pattern is #170's failure mode in the language where it is most
// common. Modern TypeScript declares most of a module's public surface as
// `export const x = someBuilder(...)` — schema/table definitions, routers,
// config objects. That is not an `arrow_function`, so the arrow-valued pattern
// above missed it, and a module made entirely of such declarations extracted
// ZERO symbols.
//
// Scoped to EXPORTED module constants, deliberately, and narrower than the
// obvious fix:
//
//   * `program > export_statement` only. A bare `program > lexical_declaration`
//     would also pick up module-private constants. That was measured and
//     dropped — see the note below — because `defs` is a cross-file retrieval
//     surface: what another module can name is what another module can search
//     for. A private constant is found by `body` search, by a caller who is
//     already in the right file.
//   * Anchored, not free-floating: `lexical_declaration` is the same node
//     inside a function body, so an unanchored pattern would fill `defs` with
//     every function-local `const` — the 90%-locals trap measured for C in #170.
//   * `"const"` token matched, so `let` stays out: `lexical_declaration` covers
//     both, and a mutable module binding is state, not a constant.
//
// Measured, and the reason for the narrower scope: on a private TypeScript
// monorepo (~1,600 TS-family files) the broad form was benchmarked against the
// unpatched engine across 8 retrieval tasks and produced no token improvement
// (+1.8%, inside a per-task spread of -11%..+12%), at unchanged answer accuracy.
// It did cut search round-trips by 19%, but every extra symbol is paid for on
// every response that carries the array. With no demonstrated retrieval gain,
// the private half of the capture was not worth its weight; the exported half
// is what closes the zero-symbol hole.
//
// An arrow-valued exported const is captured twice on purpose — `@function` by
// the pattern above and `@const` here. `defs` dedups on (kind, name), so the
// file becomes reachable by both `function Button` and `const Button`, which is
// what a caller searching for either would expect; suppressing one would mean
// choosing which of the two true statements to hide.
const TS_Q: &str = r#"
(function_declaration name: (identifier) @function)
(class_declaration name: (type_identifier) @class)
(method_definition name: (property_identifier) @method)
(interface_declaration name: (type_identifier) @interface)
(type_alias_declaration name: (type_identifier) @type)
(enum_declaration name: (identifier) @enum)
(variable_declarator name: (identifier) @function value: (arrow_function))
(program (export_statement declaration: (lexical_declaration "const" (variable_declarator name: (identifier) @const))))
"#;

// `const_item` / `static_item` are captured because a file whose whole content
// is a lookup table (`pub const BYTE_FREQUENCIES: [u8; 256] = [...]`) otherwise
// extracts ZERO symbols and becomes unreachable by symbol search — see #170.
// Those files matter disproportionately: their contents are empirical, so they
// are exactly what cannot be recalled and must be retrieved.
//
// Deliberately NOT anchored to `source_file`. Measured over the 331-file
// memchr/regex/aho-corasick corpus, the 959 const/static declarations sit at:
// file-scope 720, impl-block associated consts 75, trait 4, function-local 160.
// Anchoring would throw away the 79 associated consts — `impl Foo { const N }`
// is idiomatic Rust — to suppress function-local ones, and a function-local
// `const` in Rust is still a deliberately named constant (Rust `let` bindings
// are a different node and are never captured), so it is signal, not noise.
const RUST_Q: &str = r#"
(function_item name: (identifier) @function)
(struct_item name: (type_identifier) @struct)
(enum_item name: (type_identifier) @enum)
(trait_item name: (type_identifier) @trait)
(mod_item name: (identifier) @module)
(macro_definition name: (identifier) @macro)
(type_item name: (type_identifier) @type)
(const_item name: (identifier) @const)
(static_item name: (identifier) @static)
"#;

// Go's package-level `const`/`var` are anchored to `source_file` on purpose:
// unanchored, `var_declaration` also matches every function-local `var`, which
// fills `defs` with locals (verified — unanchoring makes `go_const_and_var`
// fail on a captured `local`). Note the grammar is asymmetric: a grouped
// `const ( A = 1 )` keeps `const_spec` as a direct child of `const_declaration`
// (there is no `const_spec_list` node — a query naming one fails to compile),
// but a grouped `var ( T = 1 )` wraps its specs in `var_spec_list`, so that
// form needs its own pattern or package-level tables in `var (…)` are missed.
const GO_Q: &str = r#"
(function_declaration name: (identifier) @function)
(method_declaration name: (field_identifier) @method)
(type_declaration (type_spec name: (type_identifier) @type))
(source_file (const_declaration (const_spec name: (identifier) @const)))
(source_file (var_declaration (var_spec name: (identifier) @static)))
(source_file (var_declaration (var_spec_list (var_spec name: (identifier) @static))))
"#;

// #170 is about files that extract zero symbols and so become unreachable; Java
// barely has that failure mode, because every file must declare a type. Measured:
// 7 of 1043 Java files in one corpus and 1 of 72 in another extract zero symbols
// under the type/method patterns (0.7%), against 20 of 331 for Rust and 186 of
// 400 for C headers. So the `static`-field pattern below was originally held back
// as "a recall improvement, not #170, and no Java corpus is wired into the
// end-to-end check that proves it."
//
// #500 supplied exactly that missing end-to-end evidence: a fair, precise-query
// retrieval test over Apache Lucene measured the gap directly — a static constant
// like `DEFAULT_MAX_CONN` was not a symbol at all (`term name=DEFAULT_MAX_CONN` ->
// count 0), so a field-level fact only survived folded inside its ~2 KB parent
// class (32-48x more bytes per answered question than grep) or was
// length-normalised out of reach (the `DEFAULT_BEAM_WIDTH` rank-35 recall miss).
// So the pattern is now shipped: it promotes each static constant to its own
// `const` symbol whose `code` is the ~40-80 B declaration span, not the parent.
// The `static` modifier is the precision filter — it captured 372 constants with
// 0 method-local false positives, where capturing ALL fields would have pulled in
// 1311 including private instance state. Java interface fields are implicitly
// `public static final` constants but parse as `constant_declaration`, not
// `field_declaration` (verified via s-expr: `interface I { int IC = 7; }` ->
// `(interface_body (constant_declaration …))`), so they get their own pattern
// below and are captured with no `static` guard needed. Non-static instance
// fields remain a separate recall question, not this bug.
const JAVA_Q: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(enum_declaration name: (identifier) @enum)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @method)
(field_declaration (modifiers "static") declarator: (variable_declarator name: (identifier) @const))
(constant_declaration declarator: (variable_declarator name: (identifier) @const))
"#;

// C has #170's failure mode worse than anywhere else: 186 of 400 sampled headers
// and 39 of 400 sampled .c files extracted zero symbols before this.
//
// `preproc_def` is the lever that matters — it rescued 148 of those 186 headers
// and 13 of those 39 .c files, because a header of nothing but `#define`s is the
// canonical C constant table. It needs no anchoring: only 319 of 7356 captures
// (4%) sat inside a function body, and include-guard-shaped names were 298 of
// 4192 in headers (7%). Function-like `#define F(x)` is a different node
// (`preproc_function_def`) and is not captured.
//
// The object declarations ARE anchored to `translation_unit`. A bare
// `(declaration ...)` is exactly the noisy query to avoid: measured over the
// same 400 .c files it captured 11806 names of which 11320 (96%) were function
// locals. Anchored, it captured 274 with zero function locals.
//
// Known limit of that anchoring, measured not assumed: an include guard parses
// as `translation_unit > preproc_ifdef > declaration`, so guarded headers put
// their declarations one level too deep and only `preproc_def` sees them. That
// is why the anchored patterns rescued just 6 headers. Widening the anchor per
// preprocessor form (`preproc_ifdef`, `preproc_if`, nesting) is unbounded and
// each extra level re-admits in-function declarations, so it is left alone:
// missing a symbol is recoverable, filling `defs` with locals is not.
//
// ── #172: the header API surface. Measured over valkey + memcached at
// /home/claude/.xerj-code/corpora/kv-oss (547 .c, 360 .h). "in-body" below
// counts captures with a `compound_statement` ancestor, i.e. the locals trap.
//
// Prototypes (`int do_thing(int);`) are `declaration` + `function_declarator`,
// a different node from `function_definition`, so none were captured — a C
// library's whole callable API was missing from `defs`. These two patterns are
// deliberately NOT anchored to `translation_unit`, against the shape of the
// rest of this query, because real headers are guarded: anchored, the prototype
// pattern captures 689 names (176 in headers); unanchored it captures 3871
// (3285 in headers). Unanchoring is safe HERE and nowhere else in C, because
// the pattern demands a `function_declarator` — declaring a function inside a
// function body is legal but vanishingly rare: 14 of 3871 (0.4%) were in-body,
// and of those 9 are a macro misparse (`JEMALLOC_CC_SILENCE_INIT`) and the rest
// are genuine local prototypes. Contrast the bare `(declaration declarator:
// (identifier))` that #170 measured: 6666 captures, 6033 (90%) in-body.
// The `pointer_declarator` variant is separate because `char *f(void);` wraps
// the `function_declarator` one level deeper: 652 more (546 in headers), 0
// in-body. Two levels (`char **f(void);`) is 10 captures corpus-wide and is not
// worth a third pattern. The same wrapper gap existed on `function_definition`,
// where it was costing 1477 pointer-returning function definitions (4 in-body,
// all macro misparses of real top-level functions).
//
// `extern int global_counter;` has no initialiser, so the `init_declarator`
// patterns above never saw it. The `extern` keyword itself is the precision
// filter — it is an anonymous token inside `storage_class_specifier`, and
// matching it lets these three stay unanchored (so guarded headers work) at
// 153 + 28 + 20 = 201 captures with 2 in-body, both genuine `extern` statements
// declaring globals inside a function. Without the keyword the same shape is
// the 90%-locals trap above. `static int x;` at file scope (no initialiser) is
// still missed; it needs an anchored pattern and is not this bug.
//
// `preproc_def` now requires a `value:` — an include guard's `#define FOO_H`
// has an EMPTY replacement list and so no `value` child, while `#define
// MAX_ITEMS 128` has `value: (preproc_arg)` (verified in the grammar, not
// assumed). That drops 564 of 5255 `#define` captures: 284 are `_H`-suffixed
// guard names, the rest are valueless build knobs (`LUA_CORE`, `_GNU_SOURCE`,
// `LTTNG_UST_TRACEPOINT_DEFINE`). The cost is real and was measured: 21 files
// lose their last symbol and go back to zero-symbol (#170's failure mode), but
// in 14 of them the lost symbol was the file's own include guard and in the
// other 7 a build knob, so no file lost a constant anyone would search for.
// Whole-corpus zero-symbol files still improve on the pre-#172 query for
// headers via the prototype patterns: .c 9→17, .h 10→17 zero, against 6201
// newly captured API names. A name-shape regex (`#not-match?` on `FOO_H`) was
// the alternative; it would keep the build knobs but it is a heuristic on
// spelling, and it misses guards like `MATH_C_`, so the grammar-level rule won.
const C_Q: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @function))
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @function)))
(declaration declarator: (function_declarator declarator: (identifier) @function))
(declaration declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @function)))
(struct_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(type_definition declarator: (type_identifier) @type)
(preproc_def name: (identifier) @const value: (preproc_arg))
(translation_unit (declaration declarator: (init_declarator declarator: (identifier) @static)))
(translation_unit (declaration declarator: (init_declarator declarator: (array_declarator declarator: (identifier) @static))))
(declaration (storage_class_specifier "extern") declarator: (identifier) @static)
(declaration (storage_class_specifier "extern") declarator: (pointer_declarator declarator: (identifier) @static))
(declaration (storage_class_specifier "extern") declarator: (array_declarator declarator: (identifier) @static))
"#;

// #820 item 3: the four `init_declarator`-anchored patterns below (top-level
// and namespace-scoped) match ANY initialized declaration, immutable or not —
// so a plain mutable global (`int counter = 0;`) reaches the same `@const`
// capture as `constexpr int MAX = 10;`. They stay tagged `@const` here for
// recall (dropping the mutable ones would be the #170 regression in
// reverse); `parse_symbols`' `cpp_declarator_is_const_qualified` check below
// relabels the ones with no `const`/`constexpr`/`consteval`/`constinit`
// qualifier to `static` — the kind `C_Q`, `GO_Q` and `RUST_Q` already use for
// a named, mutable module-level binding.
const CPP_Q: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @function))
(function_definition declarator: (function_declarator declarator: (field_identifier) @method))
(class_specifier name: (type_identifier) @class)
(struct_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(namespace_definition name: (namespace_identifier) @module)
(translation_unit (declaration declarator: (init_declarator declarator: (identifier) @const)))
(translation_unit (declaration declarator: (init_declarator declarator: (pointer_declarator declarator: (identifier) @const))))
(namespace_definition body: (declaration_list (declaration declarator: (init_declarator declarator: (identifier) @const))))
(namespace_definition body: (declaration_list (declaration declarator: (init_declarator declarator: (pointer_declarator declarator: (identifier) @const)))))
(translation_unit (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const))))
(translation_unit (_ (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const)))))
(namespace_definition body: (declaration_list (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const)))))
(namespace_definition body: (declaration_list (_ (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const))))))
(field_declaration_list (_ (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const)))))
(linkage_specification body: (declaration_list (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const)))))
(linkage_specification body: (declaration_list (_ (enum_specifier body: (enumerator_list (enumerator name: (identifier) @const))))))
(field_declaration (storage_class_specifier) @_s declarator: (field_identifier) @const (#eq? @_s "static"))
"#;

const RUBY_Q: &str = r#"
(method name: (identifier) @method)
(singleton_method name: (identifier) @method)
(class name: (constant) @class)
(module name: (constant) @module)
(assignment left: (constant) @const)
"#;

// `enum_declaration` was missing entirely, which is worse than a missing kind:
// a PHP 8.1 enum file declares no class, interface or trait, so it extracted
// ZERO symbols and could not be reached by symbol search at all (#170).
//
// `const_declaration` covers both a class/interface/enum constant and a
// file-scope `const`, in one pattern — the grammar uses the same node for
// both, with `const_element` holding the name. This is the only way to write a
// named constant inside a PHP type, so without it a class of nothing but
// constants was invisible.
//
// No anchoring here, unlike C/Go/Rust: PHP has no function-local `const`
// statement, so there is no locals trap to guard against. (`define()` is a
// function call, a different node, and is deliberately not captured.)
const PHP_Q: &str = r#"
(function_definition name: (name) @function)
(method_declaration name: (name) @method)
(class_declaration name: (name) @class)
(interface_declaration name: (name) @interface)
(trait_declaration name: (name) @trait)
(enum_declaration name: (name) @enum)
(enum_case name: (name) @const)
(const_declaration (const_element (name) @const))
"#;

// Also no constant capture, for a blunter reason than Java: there were 4 C#
// files on the machine this was measured on, so there is no corpus to measure a
// candidate query's precision against. Shipping one would be a guess, and the
// C numbers above show how wrong that guess can be (96% locals from a node type
// that looks perfectly reasonable on paper).
const CSHARP_Q: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(struct_declaration name: (identifier) @struct)
(enum_declaration name: (identifier) @enum)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @method)
(field_declaration (modifier) @_m (variable_declaration (variable_declarator (identifier) @const)) (#any-of? @_m "const" "static"))
(enum_member_declaration name: (identifier) @const)
"#;

const BASH_Q: &str = r#"
(function_definition name: (word) @function)
"#;

// ── #295 queries. Prior art: where the grammar crate ships an author-written
// `queries/tags.scm`, the definition patterns below are adapted from it and
// the source is cited; reference-capture patterns and doc-comment plumbing
// are dropped (this extractor only wants definitions), and `@definition.*`
// capture names are replaced by this file's capture-name==kind convention.
// Where no tags.scm ships, patterns are derived from the grammar's
// `src/node-types.json` and pinned by the fixture tests below. ────────────

// Kotlin has no tags.scm in tree-sitter-kotlin-ng 1.1.0; node-types.json:
// class/object/function declarations carry `name: (identifier)`. The grammar
// has no separate interface node — `interface Foo` is a class_declaration
// whose keyword token distinguishes it, so the "interface" pattern matches
// the anonymous token to keep `interface Foo` searchable as such. Kotlin has
// no `.h`-style split, and function-local `fun` is rare enough that the
// unanchored function pattern is signal (local helpers are still named
// definitions, as in Rust).
//
// #820 item 3: `property_declaration`'s grammar rule is `choice('val',
// 'var')` — an anonymous token in a fixed position, the same technique the
// "interface" pattern above already uses to tell `interface Foo` from `class
// Foo`. Matching it here tells a `var` (mutable) binding from a `val`
// (read-only) one, so file-/object-/companion-scoped `var` is labeled
// `static` — the kind `C_Q`/`GO_Q`/`RUST_Q` use for a named, mutable
// module-level binding — instead of being folded into `const` alongside a
// real `val`.
const KOTLIN_Q: &str = r#"
(class_declaration "interface" name: (identifier) @interface)
(class_declaration name: (identifier) @class)
(object_declaration name: (identifier) @object)
(function_declaration name: (identifier) @function)
(type_alias type: (identifier) @type)
(source_file (property_declaration "val" (variable_declaration (identifier) @const)))
(source_file (property_declaration "var" (variable_declaration (identifier) @static)))
(object_declaration (class_body (property_declaration "val" (variable_declaration (identifier) @const))))
(object_declaration (class_body (property_declaration "var" (variable_declaration (identifier) @static))))
(companion_object (class_body (property_declaration "val" (variable_declaration (identifier) @const))))
(companion_object (class_body (property_declaration "var" (variable_declaration (identifier) @static))))
"#;

// Adapted from tree-sitter-swift 0.7.3 queries/tags.scm (class_declaration /
// protocol_declaration / function_declaration name captures). The grammar
// folds class/struct/enum/actor/extension into one class_declaration node
// with a `declaration_kind` token field — matched here so a Swift `struct`
// is searchable as `struct Point`, not mislabelled a class. tags.scm's
// method/property captures are dropped: methods are the same
// function_declaration node captured below, and properties are the
// locals-trap shape (#170).
//
// #820 item 3: a `property_declaration`'s `let`/`var` keyword lives on the
// `mutability` field of its `value_binding_pattern` child (verified in
// node-types.json), not on `property_declaration` itself — matched here to
// tell a `var` (mutable) binding from a `let` (read-only) one, so top-level
// and `static` `var` is labeled `static` — the kind `C_Q`/`GO_Q`/`RUST_Q` use
// for a named, mutable module-level binding — instead of being folded into
// `const` alongside a real `let`.
const SWIFT_Q: &str = r#"
(class_declaration declaration_kind: "class" name: (type_identifier) @class)
(class_declaration declaration_kind: "struct" name: (type_identifier) @struct)
(class_declaration declaration_kind: "enum" name: (type_identifier) @enum)
(class_declaration declaration_kind: "actor" name: (type_identifier) @class)
(class_declaration declaration_kind: "extension" name: (user_type) @class)
(protocol_declaration name: (type_identifier) @protocol)
(function_declaration name: (simple_identifier) @function)
(source_file (property_declaration (value_binding_pattern mutability: "let") (pattern (simple_identifier) @const)))
(source_file (property_declaration (value_binding_pattern mutability: "var") (pattern (simple_identifier) @static)))
(enum_entry name: (simple_identifier) @const)
(property_declaration (modifiers (property_modifier) @_m) (value_binding_pattern mutability: "let") (pattern (simple_identifier) @const)) (#eq? @_m "static")
(property_declaration (modifiers (property_modifier) @_m) (value_binding_pattern mutability: "var") (pattern (simple_identifier) @static)) (#eq? @_m "static")
"#;

// Adapted from tree-sitter-scala 0.26.2 queries/tags.scm. `val`/`var`
// captures are dropped: the same node appears inside method bodies, which is
// the 90%-locals trap measured for C in #170; tags.scm tags them because
// editors want local navigation — `defs` does not.
const SCALA_Q: &str = r#"
(class_definition name: (identifier) @class)
(object_definition name: (identifier) @object)
(trait_definition name: (identifier) @trait)
(enum_definition name: (identifier) @enum)
(full_enum_case name: (identifier) @const)
(simple_enum_case name: (identifier) @const)
(function_definition name: (identifier) @function)
(type_definition name: (type_identifier) @type)
(compilation_unit (val_definition pattern: (identifier) @const))
(object_definition body: (template_body (val_definition pattern: (identifier) @const)))
"#;

// Adapted from tree-sitter-dart 0.2.0 queries/tags.scm (class / mixin / enum
// / function / method / typedef / enum-constant captures; getter/setter and
// constructor variants dropped as weight without retrieval surface).
const DART_Q: &str = r#"
(class_declaration name: (identifier) @class)
(mixin_declaration (identifier) @mixin)
(enum_declaration name: (identifier) @enum)
(enum_constant name: (identifier) @const)
(function_signature name: (identifier) @function)
(type_alias (type_identifier) @type)
"#;

// Adapted from tree-sitter-lua 0.5.0 queries/tags.scm: the three spellings a
// Lua module actually uses — `function M.add()`, `function M:method()`, and
// `local add = function()` / `M.add = function()` assignments, plus
// function-valued table fields.
const LUA_Q: &str = r#"
(function_declaration name: (identifier) @function)
(function_declaration name: (dot_index_expression field: (identifier) @function))
(function_declaration name: (method_index_expression method: (identifier) @method))
(assignment_statement (variable_list name: (identifier) @function) (expression_list value: (function_definition)))
(assignment_statement (variable_list name: (dot_index_expression field: (identifier) @function)) (expression_list value: (function_definition)))
(table_constructor (field name: (identifier) @function value: (function_definition)))
"#;

// No tags.scm in tree-sitter-perl 1.1.2; node-types.json: function_definition
// carries `name: (identifier)`, packages are a bare `package_name` child.
const PERL_Q: &str = r#"
(package_statement (package_name) @module)
(function_definition name: (identifier) @function)
"#;

// Adapted from tree-sitter-r 1.3.0 queries/tags.scm: R has no declaration
// keyword — a "definition" is `name <- function(...)` (or `=`), which is
// exactly what tags.scm matches. The string-lhs variants are dropped
// (assigning a function to a string name is vanishingly rare outside
// metaprogramming).
const R_Q: &str = r#"
(binary_operator lhs: (identifier) @function operator: "<-" rhs: (function_definition))
(binary_operator lhs: (identifier) @function operator: "=" rhs: (function_definition))
"#;

// No tags.scm in tree-sitter-julia 0.23.1; node-types.json: definitions wrap
// a `signature` (whose call_expression holds the name) or a `type_head`.
// The bare-identifier signature is the zero-arg `function f end` form; the
// binary_expression type_head is `struct Foo <: Bar`.
const JULIA_Q: &str = r#"
(function_definition (signature (call_expression (identifier) @function)))
(function_definition (signature (identifier) @function))
(macro_definition (signature (call_expression (identifier) @macro)))
(struct_definition (type_head (identifier) @struct))
(struct_definition (type_head (binary_expression (identifier) @struct)))
(abstract_definition (type_head (identifier) @type))
(abstract_definition (type_head (binary_expression (identifier) @type)))
(module_definition name: (identifier) @module)
"#;

// No tags.scm in tree-sitter-haskell 0.23.1; node-types.json: top level is
// `haskell > declarations > (function|signature|data_type|…)`. The function
// and signature patterns are anchored to `declarations` because the same
// `function` node appears in `where` blocks (via `local_binds`, which holds
// `decl` directly — so the anchor excludes exactly the locals, #170's trap).
// `signature` is captured too: every exported Haskell function has a type
// signature, and a file of signatures-only (class heads, foreign imports)
// must not extract zero symbols. Kinds follow the language's own keywords
// (`data`, `class`) so `data Maybe` matches what a Haskeller searches.
const HASKELL_Q: &str = r#"
(declarations (function name: (variable) @function))
(declarations (signature name: (variable) @function))
(data_type name: (name) @data)
(newtype name: (name) @data)
(class name: (name) @class)
(type_synomym name: (name) @type)
"#;

// Adapted from tree-sitter-elixir 0.3.5 queries/tags.scm — the module and
// function/macro definition patterns, including the guard-clause form. In
// Elixir `def` is itself a macro, so a definition is a `call` whose target
// spells a def-keyword: the `#any-of?` predicates (evaluated by
// predicates_hold — the core library applies none of them) are what keeps
// every other function call in the file out of `defs`. tags.scm's reference
// patterns and @ignore plumbing are dropped.
const ELIXIR_Q: &str = r#"
(call target: (identifier) @_kw (arguments (alias) @module) (#any-of? @_kw "defmodule" "defprotocol"))
(call target: (identifier) @_kw
  (arguments [
    (identifier) @function
    (call target: (identifier) @function)
    (binary_operator left: (call target: (identifier) @function) operator: "when")
  ])
  (#any-of? @_kw "def" "defp" "defdelegate" "defguard" "defguardp" "defmacro" "defmacrop" "defn" "defnp"))
"#;

// No tags.scm in tree-sitter-erlang 0.20.0 (the WhatsApp ELP grammar);
// node-types.json: fun_decl wraps function_clause (name: atom),
// module/record/define/type attributes carry their own name fields. `.hrl`
// headers are records + macros — without those two patterns a header
// extracts zero symbols, the same failure C had before #170. `-define`
// names are `var` (uppercase) or `atom` (lowercase); both are captured.
const ERLANG_Q: &str = r#"
(fun_decl (function_clause name: (atom) @function))
(module_attribute name: (atom) @module)
(record_decl name: (atom) @record)
(pp_define lhs: (macro_lhs name: (var) @const))
(pp_define lhs: (macro_lhs name: (atom) @const))
(type_alias name: (type_name name: (atom) @type))
"#;

// Adapted from tree-sitter-ocaml 0.25.0 queries/tags.scm: let-bindings with
// parameters (or a fun/function body) are functions — a bare `let x = 5` is
// deliberately not one; modules, module types, classes, methods, types and
// externals as tagged upstream. Doc-comment (#strip!) plumbing dropped.
const OCAML_Q: &str = r#"
(module_definition (module_binding (module_name) @module))
(module_type_definition (module_type_name) @interface)
(type_definition (type_binding name: (type_constructor) @type))
(value_definition (let_binding pattern: (value_name) @function (parameter)))
(value_definition (let_binding pattern: (value_name) @function body: (fun_expression)))
(value_definition (let_binding pattern: (value_name) @function body: (function_expression)))
(external (value_name) @function)
(method_definition (method_name) @method)
(class_definition (class_binding (class_name) @class))
"#;

// The interface grammar is separate (see the registry row). A `.mli` is a
// module's public surface: every `val` line IS the API, so
// value_specification is the load-bearing capture — the tags.scm above
// only covers `.ml` nodes and would extract zero symbols here.
const OCAML_MLI_Q: &str = r#"
(value_specification (value_name) @function)
(type_definition (type_binding name: (type_constructor) @type))
(module_definition (module_binding (module_name) @module))
(module_type_definition (module_type_name) @interface)
(external (value_name) @function)
"#;

// No tags.scm in tree-sitter-zig 1.1.2; node-types.json: functions carry
// `name: (identifier)`; types have NO name of their own — `const Point =
// struct {...}` is a variable_declaration whose value is the container
// declaration, so the type patterns match that shape. The bare top-level
// const pattern is anchored to source_file (function-local `const` in Zig is
// ordinary control flow, the locals trap); the container patterns need no
// anchor because the shape itself (a container child) cannot be a local.
const ZIG_Q: &str = r#"
(function_declaration name: (identifier) @function)
(variable_declaration (identifier) @struct (struct_declaration))
(variable_declaration (identifier) @enum (enum_declaration))
(variable_declaration (identifier) @union (union_declaration))
(source_file (variable_declaration (identifier) @const))
"#;

// No tags.scm in tree-sitter-objc 3.0.2. The C-family half (functions,
// structs, enums, typedefs, #defines) reuses C_Q's measured shapes — same
// nodes, same locals-trap reasoning (see C_Q). The Objective-C half from
// node-types.json: @interface/@implementation/@protocol carry their name as
// the FIRST identifier child (`superclass`/`category` are later fields, and
// a bare `(identifier)` pattern would capture those too — the `.` anchor is
// what keeps them out). Method selectors: unary selectors are a bare
// identifier child; each keyword segment is a keyword_declarator whose
// first child is the segment name (multi-segment selectors yield one symbol
// per segment; `defs` dedups).
const OBJC_Q: &str = r#"
(class_interface . (identifier) @class)
(class_implementation . (identifier) @class)
(protocol_declaration . (identifier) @protocol)
(method_definition (identifier) @method)
(method_definition (keyword_declarator . (identifier) @method))
(method_declaration (identifier) @method)
(method_declaration (keyword_declarator . (identifier) @method))
(function_definition declarator: (function_declarator declarator: (identifier) @function))
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @function)))
(struct_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(type_definition declarator: (type_identifier) @type)
(preproc_def name: (identifier) @const value: (preproc_arg))
"#;

// No tags.scm in tree-sitter-groovy 0.1.2; node-types.json mirrors the Java
// grammar's shapes (name: (identifier) throughout) plus a Groovy-specific
// function_definition for script-level `def foo() {}` — the form Gradle
// build scripts and Jenkinsfiles are made of.
const GROOVY_Q: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(enum_declaration name: (identifier) @enum)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @method)
(function_definition name: (identifier) @function)
"#;

// No tags.scm in tree-sitter-powershell 0.26.4; node-types.json: none of the
// definition nodes use fields — the name is the first named child
// (function_name for functions, simple_name for class/enum/method), so the
// class/enum patterns anchor to keep member names from matching.
const POWERSHELL_Q: &str = r#"
(function_statement (function_name) @function)
(class_statement . (simple_name) @class)
(class_method_definition (simple_name) @method)
(enum_statement . (simple_name) @enum)
"#;

// Adapted from tree-sitter-fsharp 0.3.11 queries/tags.scm. The function
// patterns keep upstream's anchoring to the four top-level contexts
// (file/named_module/module_defn/namespace via declaration_expression) —
// F# nests `let` inside functions constantly, and the same
// function_or_value_defn node models both, so the unanchored form is the
// locals trap (#170). The type pattern collapses upstream's nine-variant
// enumeration into a wildcard: every variant wraps the same type_name node.
const FSHARP_Q: &str = r#"
(named_module name: (long_identifier) @module)
(module_defn . (_) @module)
(type_definition (_ (type_name type_name: (_) @type)))
(file (declaration_expression (function_or_value_defn (function_declaration_left . (_) @function))))
(named_module (declaration_expression (function_or_value_defn (function_declaration_left . (_) @function))))
(module_defn (declaration_expression (function_or_value_defn (function_declaration_left . (_) @function))))
(namespace (declaration_expression (function_or_value_defn (function_declaration_left . (_) @function))))
(member_defn (method_or_prop_defn name: (property_or_ident) @method))
"#;

// Adapted from tree-sitter-nix 0.3.0 queries/tags.scm — its one definition
// pattern: a binding whose value is a function expression. The attrpath
// capture is narrowed to the attr identifier so `defs` carries `foo`, not
// `foo.bar.baz` punctuation.
const NIX_Q: &str = r#"
(binding attrpath: (attrpath attr: (identifier) @function) expression: (function_expression))
"#;

// Adapted from tree-sitter-fortran 0.6.0 queries/tags.scm: functions,
// subroutines, modules/programs/submodules, derived types. tags.scm maps
// program/submodule to module and derived types to class; derived types are
// @type here (`type :: point_t` is what a Fortran user searches).
const FORTRAN_Q: &str = r#"
(function_statement (name) @function)
(subroutine_statement (name) @function)
(module_statement (name) @module)
(submodule_statement (name) @module)
(program_statement (name) @module)
(derived_type_statement (type_name) @type)
"#;

// Adapted from tree-sitter-matlab 1.3.0 queries/neovim/tags.scm — the two
// definition patterns (function/class), name-fielded.
const MATLAB_Q: &str = r#"
(function_definition name: (identifier) @function)
(class_definition name: (identifier) @class)
"#;

// Adapted from tree-sitter-solidity 1.2.13 queries/tags.scm. Kinds keep the
// language's own keywords (contract/library/event) rather than tags.scm's
// class/interface mapping — `contract Token` is the search a Solidity user
// types. Functions inside contracts are the same function_definition node,
// captured once, unanchored (Solidity has no nested functions to trap on).
const SOLIDITY_Q: &str = r#"
(contract_declaration name: (identifier) @contract)
(interface_declaration name: (identifier) @interface)
(library_declaration name: (identifier) @library)
(function_definition name: (identifier) @function)
(struct_declaration name: (identifier) @struct)
(enum_declaration name: (identifier) @enum)
(event_definition name: (identifier) @event)
(modifier_definition name: (identifier) @function)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn syms(lang: &str, src: &str) -> Vec<Symbol> {
        let def = registry().iter().find(|d| d.name == lang).unwrap();
        parse_symbols(def, src).unwrap()
    }
    fn has(s: &[Symbol], name: &str, kind: &str) -> bool {
        s.iter().any(|s| s.name == name && s.kind == kind)
    }

    /// Opt-in extraction-throughput harness (perf analysis, NOT part of CI).
    /// Runs the real in-process code extractor over every source file the
    /// registry claims under a corpus dir and reports files/s + MB/s. No-op
    /// (returns immediately) unless `XERJ_EXTRACT_BENCH` is set, so it costs the
    /// normal test run nothing:
    ///   `XERJ_EXTRACT_BENCH=/path/to/repo cargo test -p xerj-autoindex \
    ///       extract_bench -- --nocapture --ignored`
    #[test]
    #[ignore = "opt-in perf harness; set XERJ_EXTRACT_BENCH"]
    fn extract_bench() {
        let Ok(root) = std::env::var("XERJ_EXTRACT_BENCH") else {
            return;
        };
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(&root)];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(is_code_ext)
                {
                    files.push(p);
                }
            }
        }
        let mut total_bytes = 0u64;
        let mut parsed = 0usize;
        let start = std::time::Instant::now();
        for path in &files {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let Some(def) = registry().iter().find(|d| d.exts.contains(&ext.as_str())) else {
                continue;
            };
            let _ = parse_symbols(def, &text);
            total_bytes += bytes.len() as u64;
            parsed += 1;
        }
        let secs = start.elapsed().as_secs_f64().max(1e-9);
        eprintln!(
            "extract_bench: {parsed} files, {:.1} MB in {secs:.2}s -> {:.0} files/s, {:.1} MB/s",
            total_bytes as f64 / 1e6,
            parsed as f64 / secs,
            total_bytes as f64 / 1e6 / secs,
        );
    }

    /// Opt-in DECLARATION-SLICE measurement harness for #500 (NOT part of CI).
    /// Runs the real extractor over a corpus and reports, for the pre-fix
    /// name-line slice and the declaration slice side by side, what the
    /// retrievable unit costs in bytes and how often it was wrong — TRUNCATED
    /// mid-signature (unbalanced brackets), WRAPPED past one line, OVERRAN by a
    /// line that does not stop where the declaration does, or COVERS THE WHOLE
    /// SPAN, meaning the grammar named nothing to stop at and the
    /// implementation is in the slice. The byte and quality claims for this
    /// change are measured here, not asserted.
    ///   `XERJ_DECL_BENCH=/path/to/repo cargo test --release -p xerj-autoindex \
    ///       --lib decl_bench -- --nocapture --ignored`
    /// `XERJ_DECL_EXT=java` restricts it to one extension, and `XERJ_DECL_DUMP=1`
    /// prints every truncated slice with its file and line — which is how the
    /// residual is diagnosed rather than guessed at. Two caveats the numbers
    /// carry: `unbalanced()` counts a `)` inside a string literal or a comment
    /// (`// long enough ;)`) as a truncation, and the WHOLE SPAN count is a
    /// lower bound because `dedent` drops blank edge lines.
    #[test]
    #[ignore = "opt-in measurement harness; set XERJ_DECL_BENCH"]
    fn decl_bench() {
        let Ok(root) = std::env::var("XERJ_DECL_BENCH") else {
            return;
        };
        let only = std::env::var("XERJ_DECL_EXT").ok();
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(&root)];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !is_code_ext(ext) {
                    continue;
                }
                if only.as_deref().is_some_and(|x| x != ext) {
                    continue;
                }
                files.push(p);
            }
        }
        files.sort();
        // Three units per symbol, measured side by side in ONE pass so the
        // before/after is the same corpus, the same parse and the same symbol
        // set: `was` is the pre-fix slice (verbatim: the single physical line
        // the name sits on, trimmed and capped), `now` is the declaration
        // slice, and `span` is the symbol's whole `line..end_line` extent — the
        // class/method body a client gets when it cannot ask for less.
        let (mut was, mut now, mut span) = (Vec::new(), Vec::new(), Vec::new());
        let (mut was_cut, mut now_cut) = (0usize, 0usize);
        let (mut wrapped, mut overran, mut whole) = (0usize, 0usize, 0usize);
        let mut per_kind: std::collections::BTreeMap<String, (usize, usize, usize, usize)> =
            std::collections::BTreeMap::new();
        let mut file_bytes_total = 0u64;
        let mut nfiles = 0usize;
        for path in &files {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let Some(def) = registry().iter().find(|d| d.exts.contains(&ext.as_str())) else {
                continue;
            };
            let Some(syms) = parse_symbols(def, &text) else {
                continue;
            };
            if syms.is_empty() {
                continue;
            }
            nfiles += 1;
            file_bytes_total += bytes.len() as u64;
            let lines: Vec<&str> = text.lines().collect();
            for sym in &syms {
                // Exactly what this field held before the declaration slice.
                let old: String = lines
                    .get(sym.line - 1)
                    .copied()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(DECL_CAP)
                    .collect();
                let body: usize = lines
                    .get(sym.line - 1..sym.end_line.min(lines.len()))
                    .map(|ls| ls.iter().map(|l| l.len() + 1).sum())
                    .unwrap_or(0);
                was.push(old.len());
                now.push(sym.code.len());
                span.push(body);
                was_cut += usize::from(unbalanced(&old));
                now_cut += usize::from(unbalanced(&sym.code));
                if unbalanced(&sym.code) && std::env::var("XERJ_DECL_DUMP").is_ok() {
                    eprintln!(
                        "CUT {}:{} [{}] {:?}",
                        path.display(),
                        sym.line,
                        sym.kind,
                        sym.code
                    );
                }
                // The two ways the name-line slice was wrong. WRAPPED: the
                // declaration spans more than one line, so one line could only
                // ever be a fragment of it. OVERRAN: the declaration fits on one
                // line but the line does not stop with it — a trailing `{` in the
                // ordinary case, the whole body when the body shares the line
                // (`void m(){ int local = 1; }`).
                wrapped += usize::from(sym.code.contains('\n'));
                overran += usize::from(!sym.code.contains('\n') && old.len() > sym.code.len());
                // The slice INCLUDES THE IMPLEMENTATION when it covers the
                // symbol's whole span — the grammar named nothing to stop at.
                // A lower bound: `dedent` drops blank edge lines, so a slice
                // ending on one is not counted here.
                let span_lines = sym.end_line.saturating_sub(sym.line) + 1;
                whole += usize::from(span_lines > 1 && sym.code.lines().count() >= span_lines);
                let e = per_kind.entry(sym.kind.clone()).or_insert((0, 0, 0, 0));
                e.0 += 1;
                e.1 += sym.code.len();
                e.2 += usize::from(unbalanced(&old));
                e.3 += usize::from(unbalanced(&sym.code));
            }
        }
        let n = was.len().max(1);
        let stat = |v: &mut Vec<usize>| {
            v.sort_unstable();
            (
                v.get(v.len() / 2).copied().unwrap_or(0),
                v.iter().sum::<usize>() as f64 / n as f64,
            )
        };
        let (was_med, was_mean) = stat(&mut was);
        let (now_med, now_mean) = stat(&mut now);
        let (span_med, span_mean) = stat(&mut span);
        eprintln!(
            "decl_bench: {nfiles} files ({:.1} MB), {n} symbols\n  \
             retrievable unit    median  mean     truncated mid-signature\n  \
             was (name line)     {was_med:<7} {was_mean:<8.1} {was_cut} ({:.1}%)\n  \
             now (declaration)   {now_med:<7} {now_mean:<8.1} {now_cut} ({:.1}%)\n  \
             span (line..end)    {span_med:<7} {span_mean:<8.1} -\n  \
             declaration WRAPPED past one line: {wrapped} ({:.1}%)\n  \
             one-line slice OVERRAN the declaration: {overran} ({:.1}%)\n  \
             slice COVERS THE WHOLE SPAN (implementation included): {whole} ({:.1}%)",
            file_bytes_total as f64 / 1e6,
            100.0 * was_cut as f64 / n as f64,
            100.0 * now_cut as f64 / n as f64,
            100.0 * wrapped as f64 / n as f64,
            100.0 * overran as f64 / n as f64,
            100.0 * whole as f64 / n as f64,
        );
        for (k, (cnt, b, tw, tn)) in per_kind {
            eprintln!(
                "    {k:<10} n={cnt:<6} mean={:<7.1} truncated was={tw} now={tn}",
                b as f64 / cnt as f64,
            );
        }
    }

    /// A declaration slice ending on an unclosed bracket did not survive to the
    /// end of its signature — the agent is handed `public void configure(`.
    fn unbalanced(code: &str) -> bool {
        let (mut round, mut square) = (0i32, 0i32);
        for c in code.chars() {
            match c {
                '(' => round += 1,
                ')' => round -= 1,
                '[' => square += 1,
                ']' => square -= 1,
                _ => {}
            }
        }
        round != 0 || square != 0
    }

    /// A node kind that merely CONTAINS a body word, inside the SIGNATURE.
    /// Matching any kind containing "block" ended the slice on it, which is the
    /// mid-signature fragment the declaration slice exists to remove — and, on
    /// the Ruby case, a strict regression on what the name-line slice stored.
    #[test]
    fn a_body_shaped_name_inside_the_signature_does_not_end_the_declaration() {
        // tree-sitter-ruby declares `block_parameter` under `method_parameters`,
        // so `&blk` was the earliest "body" after the name: `def each(`.
        let s = syms(
            "ruby",
            "class C\n  def each(&blk)\n    yield 1\n  end\nend\n",
        );
        let f = s.iter().find(|x| x.name == "each").unwrap();
        assert_eq!(f.code, "def each(&blk)", "{f:?}");
        assert_eq!(f.end_line, 4, "{f:?}");

        // `block_comment` between the name and the body — tree-sitter rust,
        // java, kotlin, scala, dart, groovy, julia and fsharp all spell it so.
        let s = syms(
            "rust",
            "pub fn scan(input: &str) /* n */ -> usize {\n    0\n}\n",
        );
        let f = s.iter().find(|x| x.name == "scan").unwrap();
        assert_eq!(f.code, "pub fn scan(input: &str) /* n */ -> usize", "{f:?}");

        // Objective-C's block-typed parameter is `block_pointer_declarator`,
        // and it is the dominant Objective-C callback idiom.
        let s = syms(
            "objc",
            "@implementation Foo\n- (void)run:(void (^)(int))cb {\n  int x = 1;\n}\n@end\n",
        );
        let f = s.iter().find(|x| x.name == "run").unwrap();
        assert_eq!(f.code, "- (void)run:(void (^)(int))cb", "{f:?}");
    }

    /// `end_line` is the symbol's own last line and `code` starts at the
    /// symbol's own first token — in the grammars whose scope node is spelled
    /// nothing like a body, where the climb used to run past the declaration
    /// into the enclosing `@implementation` / `#ifdef` / object literal.
    #[test]
    fn end_line_and_slice_are_the_symbol_not_its_enclosing_scope() {
        // C: the parent of a `#ifdef`-guarded function is the guarded REGION,
        // so `b`'s slice began at `#ifdef` and swallowed `a`'s whole body.
        let s = syms(
            "c",
            "#ifdef HAVE_X\nstatic void a(void)\n{\n  int i = 0;\n}\nstatic void b(void)\n{\n  int j = 0;\n}\n#endif\n",
        );
        let a = s.iter().find(|x| x.name == "a").unwrap();
        assert_eq!(a.code, "static void a(void)", "{a:?}");
        assert_eq!(a.end_line, 5, "{a:?}");
        let b = s.iter().find(|x| x.name == "b").unwrap();
        assert_eq!(b.code, "static void b(void)", "{b:?}");
        assert_eq!(b.end_line, 9, "{b:?}"); // 10 is the `#endif`

        // JavaScript / TypeScript: a method of an object literal.
        let s = syms(
            "javascript",
            "const api = {\n  greet(name) {\n    return name;\n  },\n  farewell(name) {\n    return name;\n  },\n};\n",
        );
        let f = s.iter().find(|x| x.name == "farewell").unwrap();
        assert_eq!(f.code, "farewell(name)", "{f:?}");
        assert_eq!(f.end_line, 7, "{f:?}"); // 8 is the end of the literal

        // Objective-C: `@interface` holds method declarations and
        // `@implementation` an `implementation_definition`, both as direct
        // children — and the class name is a direct child of the scope itself.
        let s = syms(
            "objc",
            "@interface Foo : NSObject\n- (void)doIt:(int)n;\n@end\n@implementation Foo\n- (void)doIt:(int)n {\n  int x = 1;\n}\n- (void)other {\n}\n@end\n",
        );
        let f = s.iter().find(|x| x.name == "other").unwrap();
        assert_eq!(f.code, "- (void)other", "{f:?}");
        assert_eq!(f.end_line, 9, "{f:?}"); // 10 is the `@end`
        let c = s.iter().find(|x| x.kind == "class" && x.line == 1).unwrap();
        assert_eq!(c.code, "@interface Foo : NSObject", "{c:?}");
        let c = s.iter().find(|x| x.kind == "class" && x.line == 4).unwrap();
        assert_eq!(c.code, "@implementation Foo", "{c:?}");

        // Zig: the scope is a `struct_declaration`, which in C# is a type's own
        // declaration instead — so that one name is language-scoped.
        let s = syms(
            "zig",
            "const S = struct {\n    pub fn one() u8 {\n        return 1;\n    }\n    pub fn two() u8 {\n        return 2;\n    }\n};\n",
        );
        let f = s.iter().find(|x| x.name == "two").unwrap();
        assert_eq!(f.code, "pub fn two() u8", "{f:?}");
        assert_eq!(f.end_line, 7, "{f:?}");
        let s = syms(
            "csharp",
            "namespace N {\n  public struct Point {\n    public int X;\n  }\n}\n",
        );
        let f = s.iter().find(|x| x.name == "Point").unwrap();
        assert_eq!(f.code, "public struct Point", "{f:?}");

        // MATLAB `classdef` sections and Fortran's `contains`.
        let s = syms(
            "matlab",
            "classdef A\n  methods\n    function r = one(obj)\n      r = 1;\n    end\n    function r = two(obj)\n      r = 2;\n    end\n  end\nend\n",
        );
        let f = s.iter().find(|x| x.name == "two").unwrap();
        assert_eq!(f.code, "function r = two(obj)", "{f:?}");
        assert_eq!(f.end_line, 8, "{f:?}");
        let s = syms(
            "fortran",
            "module m\ncontains\n  subroutine one()\n  end subroutine one\n  subroutine two()\n  end subroutine two\nend module m\n",
        );
        let f = s.iter().find(|x| x.name == "two").unwrap();
        assert_eq!(f.code, "subroutine two()", "{f:?}");

        // PowerShell: two functions in one `statement_list`.
        let s = syms(
            "powershell",
            "function One {\n  1\n}\nfunction Two {\n  2\n}\n",
        );
        let f = s.iter().find(|x| x.name == "One").unwrap();
        assert_eq!(f.end_line, 3, "{f:?}");

        // Go's parenthesised groups. The node kind is the same one it uses for
        // a lone `const X = 1`, so the parenthesis is what tells them apart —
        // get it wrong and every generated `.pb.go` enum table reports the whole
        // `var ( … )` block as one symbol's span.
        let s = syms("go", "const (\n\tOne = 1\n\tTwo = 2\n)\n");
        let f = s.iter().find(|x| x.name == "Two").unwrap();
        assert_eq!(f.code, "Two = 2", "{f:?}");
        assert_eq!(f.end_line, 3, "{f:?}");
        let s = syms("go", "const One = 1\n");
        let f = s.iter().find(|x| x.name == "One").unwrap();
        assert_eq!(f.code, "const One = 1", "{f:?}");

        // …and a Go composite literal marks its `{ … }` with the `body:` field,
        // which must NOT end the slice: the value is what the var declares.
        let s = syms(
            "go",
            "var (\n\tA_name = map[int32]string{\n\t\t0: \"x\",\n\t}\n\tA_value = map[string]int32{\n\t\t\"x\": 0,\n\t}\n)\n",
        );
        let f = s.iter().find(|x| x.name == "A_value").unwrap();
        assert!(f.code.starts_with("A_value = map[string]int32{"), "{f:?}");
        assert_eq!(f.end_line, 7, "{f:?}");
    }

    /// Six registered grammars name no body-shaped NODE for a function, so the
    /// slice would otherwise run to the end of the whole definition. Four of
    /// them mark the body with a `body:` FIELD and two delimit it with `end`;
    /// Haskell does neither, and that limit is asserted here rather than
    /// claimed away.
    #[test]
    fn grammars_that_name_no_body_node_still_stop_before_the_implementation() {
        let s = syms("r", "myfun <- function(a, b) {\n  a + b\n}\n");
        let f = s.iter().find(|x| x.name == "myfun").unwrap();
        assert_eq!(f.code, "myfun <- function(a, b)", "{f:?}");

        let s = syms("ocaml", "let add a b =\n  a + b\n");
        let f = s.iter().find(|x| x.name == "add").unwrap();
        assert_eq!(f.code, "let add a b =", "{f:?}");

        let s = syms("fsharp", "let add a b =\n    a + b\n");
        let f = s.iter().find(|x| x.name == "add").unwrap();
        assert_eq!(f.code, "let add a b =", "{f:?}");

        // A curried lambda's `body:` is another lambda; stopping at the first
        // one would cut the signature in half (`f = a:`).
        let s = syms("nix", "{ f = a: b: a + b; }\n");
        let f = s.iter().find(|x| x.name == "f").unwrap();
        assert_eq!(f.code, "f = a: b:", "{f:?}");

        let s = syms("julia", "function add(a, b)\n    a + b\nend\n");
        let f = s.iter().find(|x| x.name == "add").unwrap();
        assert_eq!(f.code, "function add(a, b)", "{f:?}");

        // The residual: tree-sitter-haskell has no body node, no `body:` field
        // and no `end` terminator, so an equation's slice is the equation.
        let s = syms("haskell", "module M where\nadd a b = a + b\n");
        let f = s.iter().find(|x| x.line == 2).unwrap();
        assert_eq!(f.code, "add a b = a + b", "{f:?}");
    }

    #[test]
    fn all_queries_compile() {
        // registry() builds every Query; a malformed query panics here.
        assert!(registry().len() >= 13);
    }

    /// #294 at this extractor's own boundary: content in an extensionless
    /// snapshot blob, language resolved from the logical name on `Sniffed`.
    #[test]
    fn extracts_from_extensionless_blob_via_sniffed_logical_name() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("00000000");
        std::fs::write(&blob, "def alpha_helper():\n    return 1\n").unwrap();
        let sn = crate::sniff::sniff_with_name(&blob, Path::new("src/app.py")).unwrap();
        assert_eq!(sn.family, crate::sniff::Family::Code);
        let mut records: Vec<RawRecord> = Vec::new();
        let stats = extract(&blob, &sn, &mut |record| {
            records.push(record);
            true
        })
        .unwrap();
        // #500: the file-level document + one per-declaration document
        // (`alpha_helper`), no junk. records[0] is the file document.
        assert_eq!((stats.records, stats.junk), (2, 0));
        assert_eq!(records[0].fields["language"], "python");
        assert!(
            records
                .iter()
                .any(|r| r.fields.get("name").is_some_and(|n| n == "alpha_helper")),
            "the function must be its own symbol document (#500)"
        );
        // Title comes from the logical name, not the blob ordinal.
        assert_eq!(records[0].fields["title"], "app.py");
    }

    /// #500 regression: a declaration captured under two kinds at the same
    /// line+name (`export const X = () => …` → @function + @const, the dominant
    /// modern-TS public-surface shape) must emit EXACTLY ONE record, because both
    /// share the `code:<line>:<name>` locator → one document. Emitting two would
    /// make the sealed record count exceed the read-back doc count, and the
    /// count-reconciliation barrier aborts the entire index run.
    #[test]
    fn double_captured_declaration_emits_one_record_per_locator() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("m.ts");
        std::fs::write(&f, "export const Button = () => 1;\n").unwrap();
        let sn = crate::sniff::sniff_with_name(&f, Path::new("m.ts")).unwrap();
        assert_eq!(sn.family, crate::sniff::Family::Code);
        // The parser really does double-capture this shape (guards the premise).
        assert!(
            syms("typescript", "export const Button = () => 1;\n").len() >= 2,
            "premise: Button is captured under >1 kind"
        );
        let mut records: Vec<RawRecord> = Vec::new();
        let stats = extract(&f, &sn, &mut |record| {
            records.push(record);
            true
        })
        .unwrap();
        let distinct: std::collections::HashSet<&str> =
            records.iter().map(|r| r.locator.as_str()).collect();
        assert_eq!(
            records.len(),
            distinct.len(),
            "each emitted record needs a UNIQUE locator, else sealed count > read-back \
             doc count aborts the run: {:?}",
            records.iter().map(|r| &r.locator).collect::<Vec<_>>()
        );
        // The sealed count the reconcile barrier compares against must equal the
        // number of records actually emitted.
        assert_eq!(stats.records as usize, records.len());
        assert!(
            records
                .iter()
                .any(|r| r.fields.get("name").is_some_and(|n| n == "Button")),
            "Button must still be its own symbol document"
        );
    }

    /// #500, remaining half: the retrievable declaration must be the WHOLE
    /// signature, must stop dead where the implementation begins, and the file
    /// document must say where that implementation ends.
    ///
    /// Proven to fail on the pre-fix extractor, which stored "whichever single
    /// physical line the NAME node sits on": `code` for `configure` came back as
    /// `public void configure(` — a fragment that answers nothing.
    #[test]
    fn symbol_document_is_the_whole_declaration_and_the_body_is_addressable() {
        let src = r#"class Conn {
  /** Default number of maximum connections per node */
  public static final int DEFAULT_MAX_CONN = 16;

  public void configure(
      Map<String, String> options,
      boolean strict) {
    int unrelatedBodyToken = 1;
  }
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("Conn.java");
        std::fs::write(&f, src).unwrap();
        let sn = crate::sniff::sniff_with_name(&f, Path::new("Conn.java")).unwrap();
        let mut records: Vec<RawRecord> = Vec::new();
        extract(&f, &sn, &mut |record| {
            records.push(record);
            true
        })
        .unwrap();
        let sym = |n: &str| {
            records
                .iter()
                .find(|r| r.fields.get("name").is_some_and(|v| v == n))
                .unwrap_or_else(|| panic!("no symbol document for {n}"))
        };

        let m = sym("configure");
        let code = m.fields["code"].as_str().unwrap();
        assert!(
            code.contains("boolean strict)"),
            "the declaration must survive to the END of its signature, not stop on \
             the line the name happens to sit on (#500): {code:?}"
        );
        assert!(
            !code.contains("unrelatedBodyToken"),
            "…and must still stop where the body begins (#500): {code:?}"
        );
        assert_eq!(m.fields["line"], 5);

        let c = sym("DEFAULT_MAX_CONN");
        assert_eq!(
            c.fields["code"],
            "public static final int DEFAULT_MAX_CONN = 16;"
        );

        // The body is addressable rather than returned: the file document's
        // `symbols` sidecar carries the exact `line`..`end_line` span, so a
        // caller that wants the implementation never has to guess it from where
        // the NEXT symbol starts — the guess that hands back a whole class for a
        // class and the rest of the file for the last symbol.
        let file_doc = records
            .iter()
            .find(|r| r.locator == "code")
            .expect("file document");
        let entry = file_doc.fields["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "configure")
            .unwrap_or_else(|| panic!("{:?}", file_doc.fields["symbols"]));
        assert_eq!(entry["line"], 5);
        assert_eq!(entry["end_line"], 9);
    }

    /// The declaration slice is shape-based, not per-language, so the shapes
    /// that differ most are what is worth pinning: the name captured directly
    /// under its declaration (Rust), the modifiers on the line ABOVE the name
    /// (C — invisible to the old name-line slice), the body nested two levels
    /// below the declaration node (an arrow-valued TS export), and a wrapped
    /// signature closed by a `:` rather than a brace (Python).
    #[test]
    fn declaration_slice_holds_across_grammar_shapes() {
        let s = syms(
            "rust",
            "#[inline]\npub fn scan(\n    input: &str,\n) -> usize {\n    input.len()\n}\n",
        );
        let f = s.iter().find(|x| x.name == "scan").unwrap();
        assert!(f.code.starts_with("#[inline]"), "{:?}", f.code);
        assert!(f.code.contains(") -> usize"), "{:?}", f.code);
        assert!(!f.code.contains("input.len()"), "{:?}", f.code);
        assert_eq!(f.end_line, 6, "{f:?}");

        let s = syms(
            "c",
            "static unsigned long\nhash_bytes(const char *p, size_t n)\n{\n    return 0;\n}\n",
        );
        let f = s.iter().find(|x| x.name == "hash_bytes").unwrap();
        assert!(f.code.starts_with("static unsigned long"), "{:?}", f.code);
        assert!(!f.code.contains("return 0"), "{:?}", f.code);

        let s = syms(
            "typescript",
            "export const build = (n: number): string => {\n  return String(n);\n};\n",
        );
        let f = s.iter().find(|x| x.name == "build").unwrap();
        assert!(f.code.contains("(n: number): string =>"), "{:?}", f.code);
        assert!(!f.code.contains("String(n)"), "{:?}", f.code);

        let s = syms(
            "python",
            "def scan(\n        text,\n        strict=False):\n    \"\"\"Count the bytes.\"\"\"\n    return len(text)\n",
        );
        let f = s.iter().find(|x| x.name == "scan").unwrap();
        assert!(f.code.contains("strict=False):"), "{:?}", f.code);
        assert!(!f.code.contains("return len"), "{:?}", f.code);
    }

    /// The declaration slice must never cost more than the fragment it replaced:
    /// past the cap it degrades to the start line. A lookup-table constant is
    /// the case that would otherwise pull a whole array into the symbol doc.
    #[test]
    fn oversized_declaration_degrades_to_the_start_line() {
        let body = "    0,".repeat(200);
        let src = format!("pub const TABLE: [u8; 200] = [\n{body}\n];\n");
        let s = syms("rust", &src);
        let c = s.iter().find(|x| x.name == "TABLE").unwrap();
        assert!(
            c.code.chars().count() <= DECL_CAP,
            "{} chars",
            c.code.chars().count()
        );
        assert_eq!(c.code, "pub const TABLE: [u8; 200] = [");
    }

    /// #295 acceptance criterion: every registered grammar must instantiate
    /// on the linked core. Core 0.26 accepts ABI 13–15; a grammar crate
    /// generated against a newer ABI would refuse to load — this makes that
    /// an immediate CI failure on every OS in the matrix rather than a
    /// runtime parse failure at a user's machine (the c-sharp crate already
    /// forced one core bump this way).
    #[test]
    fn all_languages_load() {
        for d in registry() {
            let mut p = Parser::new();
            assert!(
                p.set_language(&d.language).is_ok(),
                "{}: grammar ABI {} incompatible with linked tree-sitter core",
                d.name,
                d.language.abi_version()
            );
        }
    }

    /// The public docs (`ROADMAP.md`, `landing/index.html`) state a language
    /// count. Honest-claims rule: pin it to the registry so the number cannot
    /// drift. Registry ROWS exceed languages by one — OCaml needs two rows
    /// because `.ml` and `.mli` are separate grammars — while `tsx` has always
    /// been counted as its own language by these docs.
    #[test]
    fn documented_language_count() {
        assert_eq!(registry().len(), 35, "registry row count changed");
        let langs: std::collections::HashSet<&str> = registry()
            .iter()
            .map(|d| d.name.trim_end_matches("_interface"))
            .collect();
        assert_eq!(
            langs.len(),
            34,
            "docs say 34 languages; update ROADMAP.md and landing/index.html"
        );
    }

    /// The #295 regression test: these languages fell through to the prose
    /// extractor at HEAD (verified live — `language`/`symbols`/`defs` all
    /// null after an `xerj autoindex` run over Kotlin/Swift/Lua/Elixir
    /// fixtures). An unclaimed extension is not code to the sniffer AT ALL,
    /// so those files were chunked into body-only records. This fails on the
    /// pre-#295 registry and pins every newly claimed extension to its
    /// language.
    #[test]
    fn issue_295_extensions_route() {
        for (ext, lang) in [
            ("kt", "kotlin"),
            ("kts", "kotlin"),
            ("swift", "swift"),
            ("scala", "scala"),
            ("sc", "scala"),
            ("dart", "dart"),
            ("lua", "lua"),
            ("pl", "perl"),
            ("pm", "perl"),
            ("r", "r"),
            ("jl", "julia"),
            ("hs", "haskell"),
            ("ex", "elixir"),
            ("exs", "elixir"),
            ("erl", "erlang"),
            ("hrl", "erlang"),
            ("ml", "ocaml"),
            ("mli", "ocaml_interface"),
            ("zig", "zig"),
            ("m", "objc"),
            ("mm", "objc"),
            ("groovy", "groovy"),
            ("gradle", "groovy"),
            ("ps1", "powershell"),
            ("psm1", "powershell"),
            ("psd1", "powershell"),
            ("fs", "fsharp"),
            ("fsx", "fsharp"),
            ("nix", "nix"),
            ("f90", "fortran"),
            ("f95", "fortran"),
            ("f03", "fortran"),
            ("sol", "solidity"),
        ] {
            assert!(is_code_ext(ext), "{ext} must be recognised as code");
            let d = registry().iter().find(|d| d.exts.contains(&ext));
            assert_eq!(
                d.map(|d| d.name),
                Some(lang),
                "{ext} must route to the {lang} grammar"
            );
        }
        // Deliberately NOT claimed, each for a stated reason: fixed-form
        // Fortran would mis-parse under the free-form grammar; nim/crystal
        // have no usable published grammar; sql routing is deferred; clojure
        // waits on a crate release against core 0.26.
        for ext in ["f", "nim", "cr", "sql", "clj"] {
            assert!(!is_code_ext(ext), "{ext} must stay unclaimed (see #295)");
        }
    }

    /// The `.m` collision decision from #295: content probe. Objective-C
    /// markers route to objc; a markerless `.m` is MATLAB. The probe only
    /// runs for extensions with more than one claimant.
    #[test]
    fn m_extension_content_probe() {
        assert!(looks_like_objc("#import <Foundation/Foundation.h>\n"));
        assert!(looks_like_objc("@interface Foo : NSObject\n@end\n"));
        assert!(!looks_like_objc(
            "function y = square(x)\n  y = x^2;\nend\n"
        ));
        // Registry order is load-bearing: objc (probed) must sort before
        // matlab (fallback) among the `.m` claimants.
        let claimants: Vec<&str> = registry()
            .iter()
            .filter(|d| d.exts.contains(&"m"))
            .map(|d| d.name)
            .collect();
        assert_eq!(claimants, ["objc", "matlab"], "probe order broken");
        assert!(
            registry()
                .iter()
                .find(|d| d.name == "objc")
                .unwrap()
                .probe
                .is_some(),
            "objc row must carry the content probe"
        );
    }

    #[test]
    fn python() {
        let s = syms(
            "python",
            "def foo():\n  pass\nclass Bar:\n  def baz(self):\n    pass\n",
        );
        assert!(has(&s, "foo", "function"));
        assert!(has(&s, "Bar", "class"));
        assert!(has(&s, "baz", "function"));
    }

    /// #500: Python was the one major language whose extractor captured only
    /// `function` + `class`, so a module-level constant / config binding (the
    /// exact `DEFAULT_MAX_CONN` failure #500 measured on Java) was indexed
    /// nowhere and only survived folded inside… nothing — Python has no
    /// enclosing type, so a settings module extracted ZERO retrievable facts
    /// for its constants. Mirror the Java-constant (#605) / TS-`export const`
    /// (#285) fix: promote each module-level assignment to its own `const`
    /// symbol. Anchored to `module` (like TS_Q's `program`) so a
    /// function-LOCAL assignment stays out of the cross-file retrieval surface.
    #[test]
    fn python_module_level_constants() {
        let s = syms(
            "python",
            "MAX_CONN = 100\n\
             DATABASE_URL = \"postgres://x\"\n\
             ROUTES = [\"a\", \"b\"]\n\
             TYPED: int = 5\n\
             BARE_ANNOT: str\n\
             def f():\n    local_v = 1\n    return local_v\n\
             class C:\n    pass\n",
        );
        assert!(has(&s, "MAX_CONN", "const"), "got {s:?}");
        assert!(has(&s, "DATABASE_URL", "const"), "got {s:?}");
        assert!(has(&s, "ROUTES", "const"), "got {s:?}");
        // A type-annotated assignment WITH a value is a real constant.
        assert!(has(&s, "TYPED", "const"), "got {s:?}");
        // A bare annotation (`BARE_ANNOT: str`, no value) binds nothing — the
        // `right: (_)` gate excludes it (precision, no recall loss).
        assert!(!has(&s, "BARE_ANNOT", "const"), "got {s:?}");
        // Function-local assignment stays out (anchored to `module`).
        assert!(!has(&s, "local_v", "const"), "got {s:?}");
        // Existing function/class capture is unchanged.
        assert!(has(&s, "f", "function"));
        assert!(has(&s, "C", "class"));
    }

    #[test]
    fn javascript() {
        let s = syms(
            "javascript",
            "function f(){}\nclass C{ m(){} }\nconst g = () => 1;\n",
        );
        assert!(has(&s, "f", "function"));
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
        assert!(has(&s, "g", "function"));
    }

    /// The JavaScript half of #293. `TS_Q` gained exported-module-`const`
    /// capture in #285; `JS_Q` did not, so an ESM JavaScript module whose whole
    /// public surface is `export const x = someBuilder(...)` still extracted
    /// ZERO symbols — #170's failure mode, unchanged, in `.js`/`.jsx`/`.mjs`/
    /// `.cjs`. The scope boundaries are the ones #285 established and are
    /// re-asserted here rather than assumed to carry over: the grammar differs.
    #[test]
    fn javascript_exported_module_const() {
        let s = syms(
            "javascript",
            "export const users = pgTable(\"users\", {});\n\
             export const ROUTES = [\"a\", \"b\"];\n\
             const MAX_BODY_CHARS = 30000;\n\
             export let mutableState = 2;\n\
             function f(){ const local = 1; return local; }\n",
        );
        assert!(has(&s, "users", "const"), "got {s:?}");
        assert!(has(&s, "ROUTES", "const"), "got {s:?}");
        // Module-private const stays out: `defs` is the cross-file retrieval
        // surface, and #285 measured the broader scope as weight without gain.
        assert!(
            !has(&s, "MAX_BODY_CHARS", "const"),
            "captured a module-private const: {s:?}"
        );
        // Function-local `const` must NOT be captured — the 90%-locals trap.
        assert!(!has(&s, "local", "const"), "captured a local: {s:?}");
        // `let` is mutable module state, not a constant, even when exported.
        assert!(!has(&s, "mutableState", "const"), "captured a let: {s:?}");
    }

    /// The overlap is wider in JavaScript than in TypeScript, and that is worth
    /// pinning rather than discovering later: `JS_Q`'s function-valued pattern
    /// admits `function_expression` as well as `arrow_function`, so BOTH forms
    /// of an exported function-valued const carry two kinds. `defs` dedups on
    /// (kind, name), so each answers searches for either spelling.
    #[test]
    fn javascript_exported_function_valued_const_is_both_kinds() {
        let s = syms(
            "javascript",
            "export const arrowFn = () => 1;\n\
             export const exprFn = function () { return 1; };\n",
        );
        assert!(has(&s, "arrowFn", "function"), "got {s:?}");
        assert!(has(&s, "arrowFn", "const"), "got {s:?}");
        assert!(has(&s, "exprFn", "function"), "got {s:?}");
        assert!(has(&s, "exprFn", "const"), "got {s:?}");
    }

    #[test]
    fn typescript() {
        let s = syms(
            "typescript",
            "interface I {}\ntype T = number;\nclass C { m(): void {} }\nfunction f(): void {}\n",
        );
        assert!(has(&s, "I", "interface"));
        assert!(has(&s, "T", "type"));
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
        assert!(has(&s, "f", "function"));
    }

    /// Exported module constants. `export const x = someBuilder(...)` is the
    /// dominant way modern TypeScript declares a module's public surface
    /// (schema/table/router builders, config objects). It is not an
    /// `arrow_function`, so before this the only const pattern (arrow-valued)
    /// missed it and a module consisting of such declarations extracted ZERO
    /// symbols — #170's failure mode, in the language where it is most common.
    /// The negative assertions below are the scope boundary, not omissions.
    #[test]
    fn typescript_module_const() {
        let s = syms(
            "typescript",
            "export const users = pgTable(\"users\", {});\n\
             export const ROUTES = [\"a\", \"b\"];\n\
             const MAX_BODY_CHARS = 30_000;\n\
             export let mutableState = 2;\n\
             function f(){ const local = 1; return local; }\n",
        );
        assert!(has(&s, "users", "const"), "got {s:?}");
        assert!(has(&s, "ROUTES", "const"), "got {s:?}");
        // Module-PRIVATE const stays out. Not an oversight: `defs` is the
        // cross-file retrieval surface, and the broad form that also captured
        // these showed no retrieval gain while adding weight to every response
        // that carries the array — see the note on TS_Q.
        assert!(
            !has(&s, "MAX_BODY_CHARS", "const"),
            "captured a module-private const: {s:?}"
        );
        // Function-local `const` must NOT be captured, or `defs` fills with
        // locals — the same trap measured at 90% noise for C in #170.
        assert!(!has(&s, "local", "const"), "captured a local: {s:?}");
        // `let` is mutable module state, not a constant, even when exported.
        assert!(!has(&s, "mutableState", "const"), "captured a let: {s:?}");
    }

    /// Pins the deliberate overlap documented on TS_Q: an arrow-valued module
    /// const is both a function and a const, and is captured as both so either
    /// search term reaches the file. If a future edit suppresses one, this
    /// fails and the reader is sent to the comment explaining the choice.
    #[test]
    fn typescript_arrow_const_is_both_kinds() {
        let s = syms("typescript", "export const Button = () => 1;\n");
        assert!(has(&s, "Button", "function"), "got {s:?}");
        assert!(has(&s, "Button", "const"), "got {s:?}");
    }

    /// `.mts`/`.cts` are TypeScript's ESM/CJS file extensions, the exact
    /// counterparts of `.mjs`/`.cjs` which javascript already claims. Without
    /// them a Node-ESM TypeScript file is not code to the sniffer at all: it
    /// falls through to the prose extractor, so it yields no `language`, no
    /// `defs`, no `symbols`, and gets chunked into several records instead of
    /// one.
    #[test]
    fn typescript_module_extensions() {
        assert!(is_code_ext("mts"), "mts must be recognised as code");
        assert!(is_code_ext("cts"), "cts must be recognised as code");
        for ext in ["mts", "cts"] {
            let d = registry().iter().find(|d| d.exts.contains(&ext));
            assert_eq!(
                d.map(|d| d.name),
                Some("typescript"),
                "{ext} must route to the typescript grammar"
            );
        }
    }
    #[test]
    fn rust() {
        let s = syms(
            "rust",
            "fn f(){}\nstruct S;\nenum E{A}\ntrait T{}\nmod m{}\n",
        );
        assert!(has(&s, "f", "function"));
        assert!(has(&s, "S", "struct"));
        assert!(has(&s, "E", "enum"));
        assert!(has(&s, "T", "trait"));
        assert!(has(&s, "m", "module"));
    }
    /// A file that is nothing but a lookup table must still yield symbols.
    /// Without `const_item`/`static_item` in RUST_Q this extracts zero, and the
    /// file becomes unreachable by symbol search (#170) — which is exactly the
    /// content that cannot be recalled and therefore most needs retrieving.
    #[test]
    fn rust_const_and_static() {
        let s = syms(
            "rust",
            "pub const BYTE_FREQUENCIES: [u8; 256] = [1, 2];\nstatic TABLE: [u8; 2] = [3, 4];\n",
        );
        assert!(has(&s, "BYTE_FREQUENCIES", "const"), "got {s:?}");
        assert!(has(&s, "TABLE", "static"), "got {s:?}");
    }
    /// Associated consts are why RUST_Q is not anchored to `source_file`; if a
    /// future edit anchors it to suppress function-local consts, this fails.
    #[test]
    fn rust_associated_const() {
        let s = syms(
            "rust",
            "struct S;\nimpl S { pub const LANES: usize = 8; }\ntrait T { const NAME: &'static str; }\n",
        );
        assert!(has(&s, "LANES", "const"), "got {s:?}");
        assert!(has(&s, "NAME", "const"), "got {s:?}");
    }
    #[test]
    fn go_const_and_var() {
        let s = syms(
            "go",
            "package p\nconst Limit = 10\nvar Table = []byte{1}\nfunc F(){ var local = 1; _ = local }\n",
        );
        assert!(has(&s, "Limit", "const"), "got {s:?}");
        assert!(has(&s, "Table", "static"), "got {s:?}");
        // Function-local `var` must NOT be captured, or defs fills with locals.
        assert!(!has(&s, "local", "static"), "captured a local: {s:?}");
    }
    /// Grouped `const (…)` / `var (…)` are how Go actually writes lookup tables.
    /// They parse differently from each other — grouped `var` interposes a
    /// `var_spec_list` node, grouped `const` does not — so the anchored form
    /// silently misses `var (…)` unless that case is matched explicitly.
    #[test]
    fn go_grouped_const_and_var() {
        let s = syms(
            "go",
            "package p\nconst (\n Alpha = 1\n Beta = 2\n)\nvar (\n Grouped = []byte{1}\n)\nfunc F(){ var loc = 1; const lc = 2; _ = loc; _ = lc }\n",
        );
        assert!(has(&s, "Alpha", "const"), "got {s:?}");
        assert!(has(&s, "Beta", "const"), "got {s:?}");
        assert!(has(&s, "Grouped", "static"), "got {s:?}");
        assert!(!has(&s, "loc", "static"), "captured a local: {s:?}");
        assert!(!has(&s, "lc", "const"), "captured a local const: {s:?}");
    }
    #[test]
    fn go() {
        let s = syms(
            "go",
            "package p\nfunc F(){}\nfunc (r R) M(){}\ntype T struct{}\n",
        );
        assert!(has(&s, "F", "function"));
        assert!(has(&s, "M", "method"));
        assert!(has(&s, "T", "type"));
    }
    #[test]
    fn java() {
        let s = syms("java", "class C { void m(){} interface I {} }\n");
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
        assert!(has(&s, "I", "interface"));
    }
    /// #500: a Java constant (class `static` field OR interface field) must become
    /// its own `const` symbol whose `code` is the ~40-80 B declaration span, so a
    /// field-level fact is retrievable directly instead of only folded inside its
    /// ~2 KB parent class. Instance fields and method locals stay out.
    #[test]
    fn java_static_constants_are_symbols() {
        let s = syms(
            "java",
            "class HnswGraphBuilder {\n    public static final int DEFAULT_MAX_CONN = 16;\n    private int instanceField = 3;\n    void m(){ int local = 1; (void)local; }\n}\ninterface Params {\n    int DEFAULT_BEAM_WIDTH = 100;\n}\n",
        );
        // The class static constant is its own `const` symbol, and its `code` is
        // the declaration span (~46 B) — NOT the multi-line parent class body.
        let c = s
            .iter()
            .find(|s| s.name == "DEFAULT_MAX_CONN" && s.kind == "const")
            .unwrap_or_else(|| panic!("a static class constant must be a symbol (#500): {s:?}"));
        assert!(
            c.code.contains("DEFAULT_MAX_CONN = 16") && c.code.len() < 120,
            "code must be the declaration span, not the ~KB parent class (#500): {:?}",
            c.code
        );
        // A Java interface field is an implicitly-static constant (constant_declaration).
        assert!(
            has(&s, "DEFAULT_BEAM_WIDTH", "const"),
            "an interface constant must be a symbol (#500): {s:?}"
        );
        // The static/constant filter keeps instance fields and method locals out.
        assert!(
            !has(&s, "instanceField", "const"),
            "a non-static instance field must NOT be captured (noise): {s:?}"
        );
        assert!(
            !has(&s, "local", "const"),
            "a method local must NOT be captured: {s:?}"
        );
    }
    #[test]
    fn c() {
        let s = syms(
            "c",
            "int f(int x){return x;}\nstruct S{int a;};\nenum E{A};\n",
        );
        assert!(has(&s, "f", "function"));
        assert!(has(&s, "S", "struct"));
        assert!(has(&s, "E", "enum"));
    }
    /// C's table file: `#define`s plus file-scope arrays, and NOT the locals
    /// inside a function — an unanchored `(declaration …)` captured 96% locals
    /// on a real 400-file sample, so the anchoring is the whole point.
    #[test]
    fn c_const_and_file_scope() {
        let s = syms(
            "c",
            "#define MAX_LEN 256\nstatic const unsigned char BYTE_FREQUENCIES[256] = {1,2};\nint g_count = 7;\nvoid f(void){ int local = 1; const int lc = 2; (void)local; (void)lc; }\n",
        );
        assert!(has(&s, "MAX_LEN", "const"), "got {s:?}");
        assert!(has(&s, "BYTE_FREQUENCIES", "static"), "got {s:?}");
        assert!(has(&s, "g_count", "static"), "got {s:?}");
        assert!(!has(&s, "local", "static"), "captured a local: {s:?}");
        assert!(!has(&s, "lc", "static"), "captured a local: {s:?}");
    }
    /// Function-like macros are a different node and must stay out of `defs`.
    #[test]
    fn c_function_macro_not_captured() {
        let s = syms("c", "#define MIN(a,b) ((a)<(b)?(a):(b))\n#define LIMIT 4\n");
        assert!(has(&s, "LIMIT", "const"), "got {s:?}");
        assert!(!has(&s, "MIN", "const"), "captured a function macro: {s:?}");
    }

    /// The header probe from #172, unguarded. A header is prototypes and
    /// `extern`s; before this both were invisible, so a C library's callable
    /// API never reached `defs` and `do_thing` could not be found.
    #[test]
    fn c_header_api_surface() {
        let s = syms(
            "c",
            "#define MAX_ITEMS 128\n\
             extern int global_counter;\n\
             extern char *global_name;\n\
             extern struct thing global_table[];\n\
             struct thing { int x; };\n\
             int do_thing(int a);\n\
             char *dup_thing(const char *s);\n",
        );
        assert!(has(&s, "do_thing", "function"), "got {s:?}");
        assert!(has(&s, "dup_thing", "function"), "got {s:?}");
        assert!(has(&s, "global_counter", "static"), "got {s:?}");
        assert!(has(&s, "global_name", "static"), "got {s:?}");
        assert!(has(&s, "global_table", "static"), "got {s:?}");
        assert!(has(&s, "MAX_ITEMS", "const"), "got {s:?}");
        assert!(has(&s, "thing", "struct"), "got {s:?}");
    }

    /// The same header behind an include guard. Everything sits one level
    /// deeper (`translation_unit > preproc_ifdef > …`), which is why the
    /// prototype and `extern` patterns are unanchored — and the guard's own
    /// `#define` must NOT become a constant, which is what it used to do.
    #[test]
    fn c_include_guard_is_not_a_constant() {
        let s = syms(
            "c",
            "#ifndef GUARDED_H\n\
             #define GUARDED_H\n\
             #define MAX_ITEMS 128\n\
             extern int global_counter;\n\
             struct thing { int x; };\n\
             int do_thing(int a);\n\
             #endif\n",
        );
        assert!(!has(&s, "GUARDED_H", "const"), "captured a guard: {s:?}");
        assert!(has(&s, "MAX_ITEMS", "const"), "got {s:?}");
        assert!(has(&s, "do_thing", "function"), "got {s:?}");
        assert!(has(&s, "global_counter", "static"), "got {s:?}");
        assert!(has(&s, "thing", "struct"), "got {s:?}");
    }

    /// The precision half of the prototype/`extern` patterns. Unanchoring them
    /// is only safe because each demands a shape a local cannot have; a bare
    /// declaration inside a function body must stay out of `defs` (measured at
    /// 90% locals if it does not — #170).
    #[test]
    fn c_locals_still_excluded() {
        let s = syms(
            "c",
            "int do_thing(int a);\n\
             void f(void) {\n\
                 int local;\n\
                 char *ptr;\n\
                 int table[4];\n\
                 struct thing *node;\n\
                 (void)local; (void)ptr; (void)table; (void)node;\n\
             }\n",
        );
        assert!(has(&s, "do_thing", "function"), "got {s:?}");
        assert!(has(&s, "f", "function"), "got {s:?}");
        assert!(!has(&s, "local", "static"), "captured a local: {s:?}");
        assert!(!has(&s, "ptr", "static"), "captured a local: {s:?}");
        assert!(!has(&s, "table", "static"), "captured a local: {s:?}");
        assert!(!has(&s, "node", "static"), "captured a local: {s:?}");
    }

    /// A pointer-returning function wraps its `function_declarator` in a
    /// `pointer_declarator`, so it needs its own pattern — 1477 definitions and
    /// 652 prototypes in the measured corpus went missing without it.
    #[test]
    fn c_pointer_returning_functions() {
        let s = syms(
            "c",
            "char *sdsnew(const char *init);\nchar *sdsdup(char *s) { return s; }\n",
        );
        assert!(has(&s, "sdsnew", "function"), "got {s:?}");
        assert!(has(&s, "sdsdup", "function"), "got {s:?}");
    }

    /// The deliberate cost of the empty-replacement-list rule: a valueless
    /// build knob is dropped along with the guards. Documented as a test so a
    /// future reader sees it was a choice, not an oversight.
    #[test]
    fn c_valueless_define_is_dropped() {
        let s = syms("c", "#define LUA_CORE\n#define LIMIT 4\n");
        assert!(has(&s, "LIMIT", "const"), "got {s:?}");
        assert!(!has(&s, "LUA_CORE", "const"), "got {s:?}");
    }
    #[test]
    fn cpp() {
        let s = syms(
            "cpp",
            "class C { void m(){} };\nint f(){return 0;}\nnamespace n {}\n",
        );
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "f", "function"));
        assert!(has(&s, "n", "module"));
    }

    /// #500: `CPP_Q` captured function/class/struct/enum/namespace but not
    /// top-level `constexpr`/`const` variables or enum values — so a config
    /// header of `constexpr int DEFAULT_MAX_CONN = 100;` (the fact #500 measured)
    /// indexed the constant nowhere. Capture translation-unit and namespace
    /// declarations (plain + pointer declarators, e.g. `const char* HOST`) plus
    /// enumerators as `const`, following the `C_Q` file-scope precedent (#170);
    /// anchored to unit/namespace so a function-LOCAL stays out.
    #[test]
    fn cpp_constants() {
        let s = syms(
            "cpp",
            "constexpr int MaxConn = 100;\n\
             const char* Host = \"x\";\n\
             namespace cfg { constexpr int Timeout = 30; }\n\
             enum Color { Red, Green };\n\
             class K { static const int MEMBER = 1; const int inst_c = 2; int inst = 3; };\n\
             int fn() { int local = 1; return local; }\n",
        );
        assert!(has(&s, "MaxConn", "const"), "got {s:?}");
        assert!(has(&s, "Host", "const"), "got {s:?}");
        assert!(has(&s, "Timeout", "const"), "got {s:?}");
        assert!(has(&s, "Red", "const"), "got {s:?}");
        assert!(has(&s, "Green", "const"), "got {s:?}");
        // class-scope `static const` member captured (the C++ analog of the
        // Kotlin companion / Java static constant #500 measured).
        assert!(has(&s, "MEMBER", "const"), "got {s:?}");
        // Instance state stays out: a non-static `const` member and a plain
        // instance field are both excluded (the `static` precision gate).
        assert!(!has(&s, "inst_c", "const"), "got {s:?}");
        assert!(!has(&s, "inst", "const"), "got {s:?}");
        // Function-local stays out (anchored to unit / namespace scope).
        assert!(!has(&s, "local", "const"), "got {s:?}");
        assert!(has(&s, "fn", "function"));
    }

    /// #820 item 3: a plain initialized top-level/namespace global — no
    /// `const`/`constexpr`/`consteval`/`constinit` qualifier — reaches the
    /// same `init_declarator` capture as a real constant and used to be
    /// labeled `const` too. It is mutable, so it is relabeled `static`: the
    /// kind `C_Q`/`GO_Q`/`RUST_Q` already use for a named, mutable
    /// module-level binding. `volatile` is equally non-constant.
    #[test]
    fn cpp_mutable_global_is_static_not_const() {
        let s = syms(
            "cpp",
            "int counter = 0;\n\
             volatile int flag = 1;\n\
             const char* name = \"x\";\n\
             namespace cfg { int level = 0; }\n\
             constexpr int MAX = 10;\n",
        );
        assert!(has(&s, "counter", "static"), "got {s:?}");
        assert!(has(&s, "flag", "static"), "got {s:?}");
        assert!(has(&s, "level", "static"), "got {s:?}");
        assert!(!has(&s, "counter", "const"), "got {s:?}");
        assert!(!has(&s, "flag", "const"), "got {s:?}");
        assert!(!has(&s, "level", "const"), "got {s:?}");
        // Genuinely immutable globals keep their `const` kind.
        assert!(has(&s, "name", "const"), "got {s:?}");
        assert!(has(&s, "MAX", "const"), "got {s:?}");
    }

    /// #820 item 2: the `enumerator` pattern must be anchored to file-scope
    /// (top-level / namespace / class / `extern "C"` linkage block) — a
    /// FUNCTION-LOCAL enum's values are not a cross-file symbol and must not
    /// leak into `defs`.
    #[test]
    fn cpp_function_local_enum_does_not_leak() {
        let s = syms(
            "cpp",
            "enum Top { TA, TB };\n\
             namespace n { enum Ns { NA, NB }; }\n\
             class K { enum Member { MA, MB }; };\n\
             extern \"C\" { enum Linkage { GA, GB }; }\n\
             typedef enum { TDA, TDB } TdColor;\n\
             enum WithVar { WVA, WVB } gvar;\n\
             enum { ANONA = 1, ANONB = 2 } dims;\n\
             namespace m { enum NsVar { NVA } nvinst; }\n\
             void f() { enum Local { LA, LB }; }\n\
             void g() { typedef enum { LTA, LTB } LocTd; }\n\
             void h() { enum LVar { LVA } lvi; }\n",
        );
        // File-scope enumerators (top-level / namespace / class / extern "C")
        // stay captured.
        assert!(has(&s, "TA", "const"), "got {s:?}");
        assert!(has(&s, "NA", "const"), "got {s:?}");
        assert!(has(&s, "MA", "const"), "got {s:?}");
        // `extern "C"` is a linkage_specification block, a common interop-header
        // shape — its file-scope enum must be captured, not dropped (#841 gate).
        assert!(has(&s, "GA", "const"), "got {s:?}");
        // `typedef enum { .. } Name;` is the pervasive C/C++ header idiom — the
        // enum_specifier sits under a type_definition, so it needs its own
        // scope anchors, else its values are dropped (#841 gate 2).
        assert!(has(&s, "TDA", "const"), "got {s:?}");
        // An enum with an INLINE variable declarator (`enum E {..} v;`) — and
        // the idiomatic anonymous-constant form (`enum { A, B } v;`) — wraps the
        // enum_specifier in a `declaration` node, so file-scope anchors must
        // allow that wrapper (#841 gate 3, the #500 "constants indexed nowhere").
        assert!(has(&s, "WVA", "const"), "got {s:?}");
        assert!(has(&s, "ANONA", "const"), "got {s:?}");
        assert!(has(&s, "NVA", "const"), "got {s:?}");
        // Function-local enumerators must NOT leak — plain, typedef'd, OR with
        // an inline declarator.
        assert!(!has(&s, "LA", "const"), "got {s:?}");
        assert!(!has(&s, "LB", "const"), "got {s:?}");
        assert!(!has(&s, "LTA", "const"), "got {s:?}");
        assert!(!has(&s, "LVA", "const"), "got {s:?}");
    }

    /// #852: an enum that is a member of a struct/class DEFINED INSIDE a function
    /// still matched the `field_declaration_list` enum-const anchor (and the
    /// unanchored `@struct`/`@class` name patterns), leaking the local type's
    /// enumerators AND its name. A `compound_statement`-ancestor post-filter
    /// drops every C++ capture inside a function body.
    #[test]
    fn cpp_enum_in_function_local_type_does_not_leak() {
        let s = syms(
            "cpp",
            "enum Top { TA };\n\
             struct FileStruct { enum FE { FSA }; };\n\
             void s() { struct LocalStruct { enum E { LSA }; }; }\n\
             void c() { class LocalClass { enum E2 { LCA }; }; }\n",
        );
        // A file-scope enum and a file-scope struct's member enum stay captured,
        // as does the file-scope struct name.
        assert!(has(&s, "TA", "const"), "got {s:?}");
        assert!(has(&s, "FSA", "const"), "got {s:?}");
        assert!(has(&s, "FileStruct", "struct"), "got {s:?}");
        // The function-local struct/class, their member enums, and those enums'
        // values must all stay out.
        assert!(!has(&s, "LSA", "const"), "got {s:?}");
        assert!(!has(&s, "LCA", "const"), "got {s:?}");
        assert!(!has(&s, "LocalStruct", "struct"), "got {s:?}");
        assert!(!has(&s, "LocalClass", "class"), "got {s:?}");
    }

    #[test]
    fn ruby() {
        let s = syms("ruby", "class C\n def m\n end\nend\nmodule M\nend\n");
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
        assert!(has(&s, "M", "module"));
    }

    /// #500: `RUBY_Q` captured method/class/module but not CONSTANTS, so a
    /// `MAX_CONN = 100` (the `DEFAULT_MAX_CONN`-class fact #500 measured) was
    /// indexed nowhere. Ruby's grammar makes this precise for free: a constant
    /// assignment binds `left: (constant)` — a distinct node from a local
    /// variable's `(identifier)` — so `(assignment left: (constant))` captures
    /// UPPER_CASE constants at any scope and NEVER a lowercase local, no anchor
    /// needed (unlike Python/module).
    #[test]
    fn ruby_constants() {
        let s = syms(
            "ruby",
            "MAX = 100\n\
             HOST = \"x\"\n\
             class C\n  TABLE = [1, 2]\n  def m\n    local = 1\n  end\nend\n",
        );
        assert!(has(&s, "MAX", "const"), "got {s:?}");
        assert!(has(&s, "HOST", "const"), "got {s:?}");
        assert!(has(&s, "TABLE", "const"), "got {s:?}");
        // A lowercase local variable is `(identifier)`, never `(constant)`.
        assert!(!has(&s, "local", "const"), "got {s:?}");
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
    }

    #[test]
    fn php() {
        let s = syms(
            "php",
            "<?php\nfunction f(){}\nclass C { function m(){} }\ninterface I {}\n",
        );
        assert!(has(&s, "f", "function"));
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
        assert!(has(&s, "I", "interface"));
    }

    /// PHP 8.1 enums and class constants. `enum_declaration` was absent from
    /// PHP_Q entirely, so an enum file extracted zero symbols and was
    /// unreachable by symbol search; class constants — PHP's only way to write
    /// a named constant inside a type — were invisible for the same reason.
    /// PHP has no function-local `const` statement, so unlike C/Go/Rust these
    /// patterns need no anchoring.
    #[test]
    fn php_enum_and_const() {
        let s = syms(
            "php",
            "<?php\n\
             enum Suit: string { case Hearts = 'H'; case Spades = 'S'; }\n\
             class C { const LIMIT = 5; public const int TYPED = 1; }\n\
             interface I { const IFACE_CONST = 2; }\n\
             const TOP_LEVEL = 3;\n",
        );
        assert!(has(&s, "Suit", "enum"), "got {s:?}");
        assert!(has(&s, "Hearts", "const"), "got {s:?}");
        assert!(has(&s, "Spades", "const"), "got {s:?}");
        assert!(has(&s, "LIMIT", "const"), "got {s:?}");
        assert!(has(&s, "TYPED", "const"), "got {s:?}");
        assert!(has(&s, "IFACE_CONST", "const"), "got {s:?}");
        assert!(has(&s, "TOP_LEVEL", "const"), "got {s:?}");
    }
    #[test]
    fn csharp() {
        let s = syms(
            "csharp",
            "class C { void M(){} }\ninterface I {}\nenum E {A}\n",
        );
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "M", "method"));
        assert!(has(&s, "I", "interface"));
        assert!(has(&s, "E", "enum"));
    }

    /// #500: `CSHARP_Q` captured only class/interface/struct/enum/method/ctor,
    /// so a `const`/`static readonly` constant or an enum member — the
    /// `DEFAULT_MAX_CONN`-class fact #500 measured — was indexed nowhere. Mirror
    /// the Java-constant fix (#605): promote each `const`/`static` field and
    /// each enum member to its own `const` symbol. The `const`/`static` gate is
    /// the precision filter (like Java's `static`): an INSTANCE `readonly` field
    /// (dependency state, not a constant) stays out.
    #[test]
    fn csharp_static_constants_and_enum_members() {
        let s = syms(
            "csharp",
            "class C {\n\
             \x20 public const int MaxConn = 100;\n\
             \x20 static readonly string Url = \"x\";\n\
             \x20 readonly int Instance = 3;\n\
             \x20 void M() { int local = 1; }\n\
             }\n\
             enum E { A, B }\n",
        );
        assert!(has(&s, "MaxConn", "const"), "got {s:?}");
        assert!(has(&s, "Url", "const"), "got {s:?}");
        assert!(has(&s, "A", "const"), "got {s:?}");
        assert!(has(&s, "B", "const"), "got {s:?}");
        // Instance readonly field (not static/const) and method-local stay out.
        assert!(!has(&s, "Instance", "const"), "got {s:?}");
        assert!(!has(&s, "local", "const"), "got {s:?}");
        // Existing captures unchanged.
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "M", "method"));
        assert!(has(&s, "E", "enum"));
    }

    #[test]
    fn bash() {
        let s = syms("bash", "foo() { echo hi; }\nfunction bar { echo yo; }\n");
        assert!(has(&s, "foo", "function"));
    }

    // ── #295 fixtures: one real-world-shaped snippet per added language,
    // asserting at least one symbol of each kind its query captures. ─────

    #[test]
    fn kotlin() {
        let s = syms(
            "kotlin",
            "package demo\n\
             interface Greets { fun greet(): String }\n\
             class Greeter(val name: String) : Greets {\n\
                 override fun greet(): String = \"hi $name\"\n\
             }\n\
             object Registry { fun lookup(id: Int) = id }\n\
             typealias Handler = (Int) -> Unit\n\
             fun topLevel(): Int = 42\n",
        );
        assert!(has(&s, "Greeter", "class"), "got {s:?}");
        assert!(has(&s, "Greets", "interface"), "got {s:?}");
        assert!(has(&s, "Registry", "object"), "got {s:?}");
        assert!(has(&s, "greet", "function"), "got {s:?}");
        assert!(has(&s, "topLevel", "function"), "got {s:?}");
        assert!(has(&s, "Handler", "type"), "got {s:?}");
    }

    /// #500: `KOTLIN_Q` captured class/object/function/type but not `val`/`const
    /// val` properties, so a top-level or object-level constant — the
    /// `DEFAULT_MAX_CONN`-class fact #500 measured — was indexed nowhere. Capture
    /// file-level and object-level (singleton) properties as `const` symbols;
    /// anchored to `source_file` / the object body so a function-LOCAL `val` and
    /// class INSTANCE state stay out (the Java-`static` precision boundary).
    #[test]
    fn kotlin_constants() {
        let s = syms(
            "kotlin",
            "const val MAX = 100\n\
             val Host = \"x\"\n\
             object Cfg {\n  const val TIMEOUT = 30\n}\n\
             class Db {\n  companion object {\n    const val DEFAULT_MAX_CONN = 100\n  }\n  val instance = 1\n}\n\
             fun f() {\n  val local = 1\n}\n",
        );
        assert!(has(&s, "MAX", "const"), "got {s:?}");
        assert!(has(&s, "Host", "const"), "got {s:?}");
        assert!(has(&s, "TIMEOUT", "const"), "got {s:?}");
        // companion object const — the idiomatic Kotlin class-scoped constant
        // (direct analog of the Java `static final` #500 measured).
        assert!(has(&s, "DEFAULT_MAX_CONN", "const"), "got {s:?}");
        // Function-local `val` and class INSTANCE state stay out.
        assert!(!has(&s, "local", "const"), "got {s:?}");
        assert!(!has(&s, "instance", "const"), "got {s:?}");
        assert!(has(&s, "Cfg", "object"));
        assert!(has(&s, "Db", "class"));
        assert!(has(&s, "f", "function"));
    }

    /// #820 item 3: a top-level/object/companion `var` reached the same
    /// capture as `val` and was labeled `const` too, even though it is
    /// mutable. `val`/`var` are literal tokens in the grammar (like the
    /// `class_declaration "interface"` pattern above), so `var` is matched
    /// separately and labeled `static` — the kind `C_Q`/`GO_Q`/`RUST_Q` use
    /// for a named, mutable module-level binding.
    #[test]
    fn kotlin_top_level_var_is_static_not_const() {
        let s = syms(
            "kotlin",
            "var counter = 0\n\
             object Cfg {\n  var level = 0\n}\n\
             class Db {\n  companion object {\n    var attempts = 0\n  }\n}\n",
        );
        assert!(has(&s, "counter", "static"), "got {s:?}");
        assert!(has(&s, "level", "static"), "got {s:?}");
        assert!(has(&s, "attempts", "static"), "got {s:?}");
        assert!(!has(&s, "counter", "const"), "got {s:?}");
        assert!(!has(&s, "level", "const"), "got {s:?}");
        assert!(!has(&s, "attempts", "const"), "got {s:?}");
    }

    #[test]
    fn swift() {
        let s = syms(
            "swift",
            "protocol Shape { func area() -> Double }\n\
             struct Point { var x: Double }\n\
             enum Direction { case north, south }\n\
             class Canvas {\n\
                 func draw(_ p: Point) {}\n\
             }\n\
             func launch() {}\n",
        );
        assert!(has(&s, "Shape", "protocol"), "got {s:?}");
        assert!(has(&s, "Point", "struct"), "got {s:?}");
        assert!(has(&s, "Direction", "enum"), "got {s:?}");
        assert!(has(&s, "Canvas", "class"), "got {s:?}");
        assert!(has(&s, "draw", "function"), "got {s:?}");
        assert!(has(&s, "launch", "function"), "got {s:?}");
    }

    /// #500: `SWIFT_Q` captured types + functions but not constants, so Swift's
    /// dominant constant idiom — a `static let` namespaced in an `enum`/`struct`
    /// (`enum K { static let maxConn = 100 }`), the `DEFAULT_MAX_CONN`-class fact
    /// #500 measured — plus top-level `let` and enum cases were indexed nowhere.
    /// Capture: top-level `let` (source_file), `static let` type members (the
    /// `static` modifier is the precision gate — an INSTANCE `let` stays out,
    /// the Java-`static` boundary), and enum cases.
    #[test]
    fn swift_constants() {
        let s = syms(
            "swift",
            "enum K { static let MaxConn = 100 }\n\
             struct S { static let Url = \"x\"; let instanceName = \"n\" }\n\
             let TopLevel = 1\n\
             enum Dir { case north, south }\n\
             func f() { let local = 1 }\n",
        );
        assert!(has(&s, "MaxConn", "const"), "got {s:?}");
        assert!(has(&s, "Url", "const"), "got {s:?}");
        assert!(has(&s, "TopLevel", "const"), "got {s:?}");
        assert!(has(&s, "north", "const"), "got {s:?}");
        assert!(has(&s, "south", "const"), "got {s:?}");
        // Instance `let` (not static) and function-local `let` stay out.
        assert!(!has(&s, "instanceName", "const"), "got {s:?}");
        assert!(!has(&s, "local", "const"), "got {s:?}");
    }

    /// #820 item 3: a top-level or `static` Swift `var` reached the same
    /// capture as `let` and was labeled `const` too, even though it is
    /// mutable. The `let`/`var` keyword lives on the `mutability` field of
    /// `value_binding_pattern` (verified in node-types.json), matched here so
    /// `var` is labeled `static` — the kind `C_Q`/`GO_Q`/`RUST_Q` use for a
    /// named, mutable module-level binding — instead of folding into `const`.
    #[test]
    fn swift_var_is_static_not_const() {
        let s = syms(
            "swift",
            "var counter = 0\n\
             enum K { static var attempts = 0 }\n\
             struct S { static var level = 0 }\n",
        );
        assert!(has(&s, "counter", "static"), "got {s:?}");
        assert!(has(&s, "attempts", "static"), "got {s:?}");
        assert!(has(&s, "level", "static"), "got {s:?}");
        assert!(!has(&s, "counter", "const"), "got {s:?}");
        assert!(!has(&s, "attempts", "const"), "got {s:?}");
        assert!(!has(&s, "level", "const"), "got {s:?}");
    }

    #[test]
    fn scala() {
        let s = syms(
            "scala",
            "trait Greeter { def greet(name: String): String }\n\
             class Impl extends Greeter { def greet(name: String) = s\"hi $name\" }\n\
             object Main { def run(): Unit = () }\n\
             enum Color { case Red, Green }\n\
             type Handler = String => Unit\n",
        );
        assert!(has(&s, "Greeter", "trait"), "got {s:?}");
        assert!(has(&s, "Impl", "class"), "got {s:?}");
        assert!(has(&s, "Main", "object"), "got {s:?}");
        assert!(has(&s, "greet", "function"), "got {s:?}");
        assert!(has(&s, "Color", "enum"), "got {s:?}");
        assert!(has(&s, "Red", "const"), "got {s:?}");
        assert!(has(&s, "Handler", "type"), "got {s:?}");
    }

    /// #500: `SCALA_Q` captured class/object/trait/enum/function/type and enum
    /// cases, but not `val` definitions, so a top-level or object (singleton)
    /// `val MaxConn = 100` — the `DEFAULT_MAX_CONN`-class fact #500 measured —
    /// was indexed nowhere. Capture compilation-unit and object-body `val`
    /// definitions as `const`; anchored to those scopes so a function-LOCAL
    /// `val` and class INSTANCE state stay out.
    #[test]
    fn scala_val_constants() {
        let s = syms(
            "scala",
            "val MaxConn = 100\n\
             object Cfg {\n  val Url = \"x\"\n}\n\
             def f(): Int = { val local = 1; local }\n",
        );
        assert!(has(&s, "MaxConn", "const"), "got {s:?}");
        assert!(has(&s, "Url", "const"), "got {s:?}");
        // Function-local `val` stays out (anchored to unit / object scope).
        assert!(!has(&s, "local", "const"), "got {s:?}");
        assert!(has(&s, "Cfg", "object"));
        assert!(has(&s, "f", "function"));
    }

    #[test]
    fn dart() {
        let s = syms(
            "dart",
            "class Greeter { String greet(String name) => 'hi'; }\n\
             mixin Musical { void play() {} }\n\
             enum Suit { hearts, spades }\n\
             typedef Handler = void Function(int);\n\
             int topLevel(int x) { return x; }\n",
        );
        assert!(has(&s, "Greeter", "class"), "got {s:?}");
        assert!(has(&s, "Musical", "mixin"), "got {s:?}");
        assert!(has(&s, "Suit", "enum"), "got {s:?}");
        assert!(has(&s, "hearts", "const"), "got {s:?}");
        assert!(has(&s, "Handler", "type"), "got {s:?}");
        assert!(has(&s, "topLevel", "function"), "got {s:?}");
        assert!(has(&s, "greet", "function"), "got {s:?}");
    }

    #[test]
    fn lua() {
        let s = syms(
            "lua",
            "local M = {}\n\
             function M.add(a, b) return a + b end\n\
             function M:reset() end\n\
             local helper = function(x) return x end\n\
             function standalone() end\n\
             local T = { handler = function() end }\n\
             return M\n",
        );
        assert!(has(&s, "add", "function"), "got {s:?}");
        assert!(has(&s, "reset", "method"), "got {s:?}");
        assert!(has(&s, "helper", "function"), "got {s:?}");
        assert!(has(&s, "standalone", "function"), "got {s:?}");
        assert!(has(&s, "handler", "function"), "got {s:?}");
    }

    #[test]
    fn perl() {
        let s = syms(
            "perl",
            "package My::Module;\n\
             sub new {\n    my ($class) = @_;\n    return bless {}, $class;\n}\n\
             sub greet { return 'hi'; }\n\
             1;\n",
        );
        assert!(has(&s, "My::Module", "module"), "got {s:?}");
        assert!(has(&s, "new", "function"), "got {s:?}");
        assert!(has(&s, "greet", "function"), "got {s:?}");
    }

    #[test]
    fn r() {
        let s = syms(
            "r",
            "square <- function(x) {\n  x^2\n}\ncube = function(x) x^3\nvalue <- 42\n",
        );
        assert!(has(&s, "square", "function"), "got {s:?}");
        assert!(has(&s, "cube", "function"), "got {s:?}");
        // Plain value assignment is not a definition.
        assert!(!has(&s, "value", "function"), "captured a value: {s:?}");
    }

    #[test]
    fn julia() {
        let s = syms(
            "julia",
            "module Geometry\n\
             struct Point\n    x::Float64\nend\n\
             abstract type Shape end\n\
             struct Circle <: Shape\n    r::Float64\nend\n\
             function area(c::Circle)\n    3.14 * c.r^2\nend\n\
             macro trace(ex)\n    ex\nend\n\
             end\n",
        );
        assert!(has(&s, "Geometry", "module"), "got {s:?}");
        assert!(has(&s, "Point", "struct"), "got {s:?}");
        assert!(has(&s, "Circle", "struct"), "got {s:?}");
        assert!(has(&s, "Shape", "type"), "got {s:?}");
        assert!(has(&s, "area", "function"), "got {s:?}");
        assert!(has(&s, "trace", "macro"), "got {s:?}");
    }

    #[test]
    fn haskell() {
        // NOTE: single literal with explicit indentation — Haskell layout is
        // significant, and the `\`-continuation form strips leading spaces,
        // which silently promotes where-locals to top level.
        let s = syms(
            "haskell",
            "module Demo where\n\ndata Tree = Leaf | Node Tree Tree\nnewtype Wrapper = Wrapper Int\ntype Alias = Int\nclass Pretty a where\n  pretty :: a -> String\ndepth :: Tree -> Int\ndepth t = go t\n  where\n    go Leaf = 0\n    go (Node l r) = 1 + max (go l) (go r)\n",
        );
        assert!(has(&s, "Tree", "data"), "got {s:?}");
        assert!(has(&s, "Wrapper", "data"), "got {s:?}");
        assert!(has(&s, "Alias", "type"), "got {s:?}");
        assert!(has(&s, "Pretty", "class"), "got {s:?}");
        assert!(has(&s, "depth", "function"), "got {s:?}");
        // The where-bound helper is a local — the `declarations` anchor is
        // what keeps it out of `defs` (#170's trap, Haskell edition).
        assert!(!has(&s, "go", "function"), "captured a where-local: {s:?}");
    }

    #[test]
    fn elixir() {
        let s = syms(
            "elixir",
            "defmodule Server do\n\
               def start(port) do\n    {:ok, port}\n  end\n\
               def stop, do: :ok\n\
               defp validate(port) when is_integer(port), do: port\n\
               defmacro trace(ast), do: ast\n\
               def handle(:ping), do: :pong\n\
               IO.puts(\"not a definition\")\n\
             end\n",
        );
        assert!(has(&s, "Server", "module"), "got {s:?}");
        assert!(has(&s, "start", "function"), "got {s:?}");
        assert!(has(&s, "stop", "function"), "got {s:?}");
        assert!(has(&s, "validate", "function"), "got {s:?}");
        assert!(has(&s, "trace", "function"), "got {s:?}");
        assert!(has(&s, "handle", "function"), "got {s:?}");
        // The #any-of? predicate is what rejects ordinary calls — without
        // predicate evaluation every call in the file would be a "def".
        assert!(!has(&s, "puts", "function"), "captured a call: {s:?}");
    }

    #[test]
    fn erlang() {
        let s = syms(
            "erlang",
            "-module(server).\n\
             -record(state, {port, owner}).\n\
             -define(MAX_CONN, 100).\n\
             -define(default_port, 8080).\n\
             -type conn() :: pid().\n\
             start(Port) -> {ok, Port}.\n\
             stop() -> ok.\n",
        );
        assert!(has(&s, "server", "module"), "got {s:?}");
        assert!(has(&s, "state", "record"), "got {s:?}");
        assert!(has(&s, "MAX_CONN", "const"), "got {s:?}");
        assert!(has(&s, "default_port", "const"), "got {s:?}");
        assert!(has(&s, "conn", "type"), "got {s:?}");
        assert!(has(&s, "start", "function"), "got {s:?}");
        assert!(has(&s, "stop", "function"), "got {s:?}");
    }

    #[test]
    fn ocaml() {
        let s = syms(
            "ocaml",
            "module Geometry = struct\n  let pi = 3.14\nend\n\
             type shape = Circle of float | Square of float\n\
             let area s = match s with Circle r -> r | Square w -> w\n\
             let double = fun x -> x * 2\n\
             let tau = 6.28\n\
             external now : unit -> float = \"caml_now\"\n",
        );
        assert!(has(&s, "Geometry", "module"), "got {s:?}");
        assert!(has(&s, "shape", "type"), "got {s:?}");
        assert!(has(&s, "area", "function"), "got {s:?}");
        assert!(has(&s, "double", "function"), "got {s:?}");
        assert!(has(&s, "now", "function"), "got {s:?}");
        // A bare value binding is not a function definition.
        assert!(!has(&s, "tau", "function"), "captured a value: {s:?}");
    }

    /// `.mli` parses under a DIFFERENT grammar; without its own registry row
    /// an interface file — the file that IS a module's public API — would
    /// fall back to plain text.
    #[test]
    fn ocaml_interface() {
        let s = syms(
            "ocaml_interface",
            "type shape = Circle of float\n\
             val area : shape -> float\n\
             val name : string\n\
             module type S = sig\n  val id : int\nend\n",
        );
        assert!(has(&s, "shape", "type"), "got {s:?}");
        assert!(has(&s, "area", "function"), "got {s:?}");
        assert!(has(&s, "S", "interface"), "got {s:?}");
    }

    #[test]
    fn zig() {
        let s = syms(
            "zig",
            "const std = @import(\"std\");\n\
             const Point = struct {\n    x: f32,\n    pub fn norm(self: Point) f32 { return self.x; }\n};\n\
             const Direction = enum { north, south };\n\
             const Value = union { int: i32, float: f32 };\n\
             pub fn main() void {\n    const local = 1;\n    _ = local;\n}\n",
        );
        assert!(has(&s, "Point", "struct"), "got {s:?}");
        assert!(has(&s, "Direction", "enum"), "got {s:?}");
        assert!(has(&s, "Value", "union"), "got {s:?}");
        assert!(has(&s, "main", "function"), "got {s:?}");
        assert!(has(&s, "norm", "function"), "got {s:?}");
        assert!(has(&s, "std", "const"), "got {s:?}");
        // Function-local const must stay out (source_file anchor).
        assert!(!has(&s, "local", "const"), "captured a local: {s:?}");
    }

    #[test]
    fn objc() {
        let s = syms(
            "objc",
            "#import <Foundation/Foundation.h>\n\
             #define MAX_RETRIES 3\n\
             @protocol Drawable\n- (void)draw;\n@end\n\
             @interface Shape : NSObject\n- (double)area;\n@end\n\
             @implementation Shape\n\
             - (double)area { return 0; }\n\
             - (void)moveToX:(double)x y:(double)y { }\n\
             @end\n\
             static int helper(int v) { return v; }\n",
        );
        assert!(has(&s, "Shape", "class"), "got {s:?}");
        assert!(has(&s, "Drawable", "protocol"), "got {s:?}");
        assert!(has(&s, "area", "method"), "got {s:?}");
        assert!(has(&s, "moveToX", "method"), "got {s:?}");
        assert!(has(&s, "helper", "function"), "got {s:?}");
        assert!(has(&s, "MAX_RETRIES", "const"), "got {s:?}");
    }

    /// The other half of the `.m` probe: a markerless `.m` routes to MATLAB
    /// and extracts MATLAB symbols.
    #[test]
    fn matlab() {
        let s = syms("matlab", "function y = square(x)\n  y = x^2;\nend\n");
        assert!(has(&s, "square", "function"), "got {s:?}");
        let c = syms(
            "matlab",
            "classdef Point\n  methods\n    function obj = Point()\n    end\n  end\nend\n",
        );
        assert!(has(&c, "Point", "class"), "got {c:?}");
    }

    #[test]
    fn groovy() {
        let s = syms(
            "groovy",
            "class Pipeline {\n\
                 Pipeline() {}\n\
                 def run(stage) { stage() }\n\
             }\n\
             interface Task { void execute() }\n\
             enum Phase { BUILD, TEST }\n\
             def deploy(env) { println env }\n",
        );
        assert!(has(&s, "Pipeline", "class"), "got {s:?}");
        assert!(has(&s, "Task", "interface"), "got {s:?}");
        assert!(has(&s, "Phase", "enum"), "got {s:?}");
        assert!(has(&s, "deploy", "function"), "got {s:?}");
        assert!(has(&s, "run", "method"), "got {s:?}");
    }

    #[test]
    fn powershell() {
        let s = syms(
            "powershell",
            "function Get-Widget {\n    param($Name)\n    $Name\n}\n\
             class Inventory {\n    [int] $Count\n    [void] Add([int] $n) { }\n}\n\
             enum Color { Red; Green }\n",
        );
        assert!(has(&s, "Get-Widget", "function"), "got {s:?}");
        assert!(has(&s, "Inventory", "class"), "got {s:?}");
        assert!(has(&s, "Add", "method"), "got {s:?}");
        assert!(has(&s, "Color", "enum"), "got {s:?}");
    }

    #[test]
    fn fsharp() {
        // NOTE: single literal with explicit indentation — F# layout is
        // significant (see the haskell fixture note).
        let s = syms(
            "fsharp",
            "module Demo\ntype Shape =\n    | Circle of float\n    | Square of float\ntype Point = { X: float; Y: float }\nlet area shape =\n    let helper r = r * r\n    helper 2.0\n",
        );
        assert!(has(&s, "Demo", "module"), "got {s:?}");
        assert!(has(&s, "Shape", "type"), "got {s:?}");
        assert!(has(&s, "Point", "type"), "got {s:?}");
        assert!(has(&s, "area", "function"), "got {s:?}");
        // Nested let is a local — the context anchoring keeps it out.
        assert!(!has(&s, "helper", "function"), "captured a local: {s:?}");
    }

    #[test]
    fn nix() {
        let s = syms(
            "nix",
            "{\n  mkShell = pkgs: pkgs.mkShell {};\n  version = \"1.0\";\n}\n",
        );
        assert!(has(&s, "mkShell", "function"), "got {s:?}");
        // A non-function binding is data, not a definition.
        assert!(!has(&s, "version", "function"), "captured data: {s:?}");
    }

    #[test]
    fn fortran() {
        let s = syms(
            "fortran",
            "module geometry\n\
               type :: point_t\n    real :: x\n  end type\n\
             contains\n\
               function area(r) result(a)\n    real :: r, a\n    a = r * r\n  end function\n\
               subroutine reset(p)\n    type(point_t) :: p\n  end subroutine\n\
             end module\n\
             program main\n  use geometry\nend program\n",
        );
        assert!(has(&s, "geometry", "module"), "got {s:?}");
        assert!(has(&s, "point_t", "type"), "got {s:?}");
        assert!(has(&s, "area", "function"), "got {s:?}");
        assert!(has(&s, "reset", "function"), "got {s:?}");
        assert!(has(&s, "main", "module"), "got {s:?}");
    }

    #[test]
    fn solidity() {
        let s = syms(
            "solidity",
            "contract Token {\n\
                 struct Account { uint balance; }\n\
                 enum Phase { Open, Closed }\n\
                 event Transfer(address from, address to);\n\
                 modifier onlyOwner() { _; }\n\
                 function mint(uint amount) public { }\n\
             }\n\
             interface IToken { function total() external; }\n\
             library MathLib { function min(uint a, uint b) internal pure returns (uint) { return a; } }\n",
        );
        assert!(has(&s, "Token", "contract"), "got {s:?}");
        assert!(has(&s, "IToken", "interface"), "got {s:?}");
        assert!(has(&s, "MathLib", "library"), "got {s:?}");
        assert!(has(&s, "Account", "struct"), "got {s:?}");
        assert!(has(&s, "Phase", "enum"), "got {s:?}");
        assert!(has(&s, "Transfer", "event"), "got {s:?}");
        assert!(has(&s, "mint", "function"), "got {s:?}");
        assert!(has(&s, "onlyOwner", "function"), "got {s:?}");
        assert!(has(&s, "min", "function"), "got {s:?}");
    }
}
