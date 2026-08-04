//! Source code — AST-aware extraction via tree-sitter.
//!
//! Source files used to fall through to the plain-text extractor, so a repo was
//! searchable only as prose. This parses each file with the matching tree-sitter
//! grammar and captures its DEFINITIONS (functions, classes, methods, structs,
//! traits, interfaces, modules, constants, …). Each file becomes one document
//! carrying:
//! - `language`  the detected language
//! - `symbols`   a structured array of {name, kind, line}
//! - `defs`      "kind name" per symbol, newline-joined so BM25 matches a query
//!   like `class User` or `def save` to the file that owns it
//! - `body`      the full source text (still full-text searchable)
//!
//! The capture-name in each query IS the symbol kind (`@function`, `@class`, …),
//! so adding a language is a grammar dep + one registry row. If a grammar fails
//! to parse a file, it is indexed as plain text rather than dropped.

use super::{ExtractStats, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

/// Skip machine-generated giants (minified bundles, generated parsers) — past a
/// couple MB a single "file" is not human code and only bloats the index.
const CODE_CAP: u64 = 2 << 20;

struct LangDef {
    name: &'static str,
    exts: &'static [&'static str],
    language: Language,
    query: Query,
}

/// Is this extension a language we AST-parse? Cheap check used by the sniffer.
pub fn is_code_ext(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    registry().iter().any(|d| d.exts.contains(&e.as_str()))
}

fn def(name: &'static str, exts: &'static [&'static str], lang: Language, q: &str) -> LangDef {
    let query = Query::new(&lang, q).unwrap_or_else(|e| panic!("bad {name} query: {e}"));
    LangDef {
        name,
        exts,
        language: lang,
        query,
    }
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
            def(
                "typescript",
                &["ts"],
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
        ]
    })
}

pub fn extract(path: &Path, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(def) = registry().iter().find(|d| d.exts.contains(&ext.as_str())) else {
        stats.junk += 1;
        return Ok(stats);
    };
    let Some(bytes) = super::read_whole(path, false, CODE_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);

    // Parse + capture definitions. A parse failure (or an over-deep tree) is not
    // fatal — index the file as plain text so its content is still searchable.
    let symbols = parse_symbols(def, &text).unwrap_or_default();
    emit_code_doc(path, def.name, &text, &symbols, &mut stats, sink);
    Ok(stats)
}

/// (name, kind, 1-based line)
type Symbol = (String, String, usize);

fn parse_symbols(def: &LangDef, text: &str) -> Option<Vec<Symbol>> {
    let mut parser = Parser::new();
    parser.set_language(&def.language).ok()?;
    let tree = parser.parse(text.as_bytes(), None)?;
    let names = def.query.capture_names();
    let mut out: Vec<Symbol> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&def.query, tree.root_node(), text.as_bytes());
    while let Some(m) = it.next() {
        for cap in m.captures {
            let kind = names[cap.index as usize]; // capture name == symbol kind
            let node = cap.node;
            let name = text.get(node.byte_range()).unwrap_or("").trim();
            if name.is_empty() || name.len() > 200 {
                continue;
            }
            out.push((
                name.to_string(),
                kind.to_string(),
                node.start_position().row + 1,
            ));
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
            .filter(|(n, k, _)| seen.insert((k.clone(), n.clone())))
            .map(|(n, k, _)| format!("{k} {n}"))
            .collect();
        fields.insert("defs".into(), Value::String(defs.join("\n")));
        let arr: Vec<Value> = symbols
            .iter()
            .map(|(n, k, line)| {
                let mut m = Map::new();
                m.insert("name".into(), Value::String(n.clone()));
                m.insert("kind".into(), Value::String(k.clone()));
                m.insert("line".into(), Value::Number((*line as u64).into()));
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
    sink(RawRecord {
        fields,
        locator: "code".into(),
        group: None,
    })
}

// ── Per-language capture queries. Capture-name == emitted symbol kind. ─────────

const PYTHON_Q: &str = r#"
(function_definition name: (identifier) @function)
(class_definition name: (identifier) @class)
"#;

const JS_Q: &str = r#"
(function_declaration name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(class_declaration name: (identifier) @class)
(method_definition name: (property_identifier) @method)
(variable_declarator name: (identifier) @function value: [(arrow_function) (function_expression)])
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @function value: (arrow_function))))
"#;

const TS_Q: &str = r#"
(function_declaration name: (identifier) @function)
(class_declaration name: (type_identifier) @class)
(method_definition name: (property_identifier) @method)
(interface_declaration name: (type_identifier) @interface)
(type_alias_declaration name: (type_identifier) @type)
(enum_declaration name: (identifier) @enum)
(variable_declarator name: (identifier) @function value: (arrow_function))
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

// No constant capture here, deliberately. #170 is about files that extract zero
// symbols and so become unreachable; Java barely has that failure mode, because
// every file must declare a type. Measured: 7 of 1043 Java files in one corpus
// and 1 of 72 in another extract zero symbols under this query (0.7%), against
// 20 of 331 for Rust and 186 of 400 for C headers. A clean query does exist if
// the recall is wanted later — `(field_declaration (modifiers "static")
// declarator: (variable_declarator name: (identifier) @const))` captured 372
// constants with 0 method-local false positives, where capturing all fields
// would have pulled in 1311 including private instance state — but it is a
// recall improvement, not this bug, and no Java corpus is wired into the
// end-to-end check that proves it, so it is not shipped unverified.
const JAVA_Q: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(enum_declaration name: (identifier) @enum)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @method)
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
const C_Q: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @function))
(struct_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(type_definition declarator: (type_identifier) @type)
(preproc_def name: (identifier) @const)
(translation_unit (declaration declarator: (init_declarator declarator: (identifier) @static)))
(translation_unit (declaration declarator: (init_declarator declarator: (array_declarator declarator: (identifier) @static))))
"#;

const CPP_Q: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @function))
(function_definition declarator: (function_declarator declarator: (field_identifier) @method))
(class_specifier name: (type_identifier) @class)
(struct_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(namespace_definition name: (namespace_identifier) @module)
"#;

const RUBY_Q: &str = r#"
(method name: (identifier) @method)
(singleton_method name: (identifier) @method)
(class name: (constant) @class)
(module name: (constant) @module)
"#;

const PHP_Q: &str = r#"
(function_definition name: (name) @function)
(method_declaration name: (name) @method)
(class_declaration name: (name) @class)
(interface_declaration name: (name) @interface)
(trait_declaration name: (name) @trait)
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
"#;

const BASH_Q: &str = r#"
(function_definition name: (word) @function)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn syms(lang: &str, src: &str) -> Vec<Symbol> {
        let def = registry().iter().find(|d| d.name == lang).unwrap();
        parse_symbols(def, src).unwrap()
    }
    fn has(s: &[Symbol], name: &str, kind: &str) -> bool {
        s.iter().any(|(n, k, _)| n == name && k == kind)
    }

    #[test]
    fn all_queries_compile() {
        // registry() builds every Query; a malformed query panics here.
        assert!(registry().len() >= 13);
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
    #[test]
    fn ruby() {
        let s = syms("ruby", "class C\n def m\n end\nend\nmodule M\nend\n");
        assert!(has(&s, "C", "class"));
        assert!(has(&s, "m", "method"));
        assert!(has(&s, "M", "module"));
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
    #[test]
    fn bash() {
        let s = syms("bash", "foo() { echo hi; }\nfunction bar { echo yo; }\n");
        assert!(has(&s, "foo", "function"));
    }
}
