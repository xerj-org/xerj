//! Source code — AST-aware extraction via tree-sitter.
//!
//! Source files used to fall through to the plain-text extractor, so a repo was
//! searchable only as prose. This parses each file with the matching tree-sitter
//! grammar and captures its DEFINITIONS (functions, classes, methods, structs,
//! traits, interfaces, modules, …). Each file becomes one document carrying:
//! - `language`  the detected language
//! - `symbols`   a structured array of {name, kind, line}
//! - `defs`      "kind name" per symbol, newline-joined so BM25 matches a query
//!   like `class User` or `def save` to the file that owns it
//! - `body`      the full source text (still full-text searchable)
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
        // `defs`/`symbols`/`symbol_count` appear only when this extractor finds
        // something, so a better grammar would otherwise move the file to a
        // different dataset and orphan its old document (#178).
        origin: FieldOrigin::Extractor,
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

const RUST_Q: &str = r#"
(function_item name: (identifier) @function)
(struct_item name: (type_identifier) @struct)
(enum_item name: (type_identifier) @enum)
(trait_item name: (type_identifier) @trait)
(mod_item name: (identifier) @module)
(macro_definition name: (identifier) @macro)
(type_item name: (type_identifier) @type)
"#;

const GO_Q: &str = r#"
(function_declaration name: (identifier) @function)
(method_declaration name: (field_identifier) @method)
(type_declaration (type_spec name: (type_identifier) @type))
"#;

const JAVA_Q: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(enum_declaration name: (identifier) @enum)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @method)
"#;

const C_Q: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @function))
(struct_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(type_definition declarator: (type_identifier) @type)
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
