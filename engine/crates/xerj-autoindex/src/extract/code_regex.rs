//! Line-based definition extractors for languages with no registered
//! tree-sitter grammar (#3270 assembly, #3271 GDScript).
//!
//! No grammar dependency is added. A multi-dialect assembly grammar does not
//! exist in a maintained form, and a GDScript grammar would be a new crate
//! whose licence and query surface we have not reviewed. Both languages
//! declare their definitions at the start of a line, so a per-line scan finds
//! them. A scan cannot see nesting the way an AST can, so it reports only
//! what the first tokens of a line say.

use std::collections::HashSet;

/// One definition found by a line scan. `line` and `end_line` are 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineSym {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
    pub end_line: usize,
    pub code: String,
}

/// Cap on the definitions one file yields, the same as the AST path.
const MAX_SYMS: usize = 5000;
/// Longest name kept. Anything longer is not a hand-written identifier.
const MAX_NAME: usize = 200;
/// Longest declaration line kept in `code`.
const MAX_CODE: usize = 400;

fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn asm_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '@' | '?')
}

/// Leading assembler identifier of `s`: `[A-Za-z_.$][\w.$@?]*`.
fn asm_ident(s: &str) -> Option<&str> {
    let first = s.chars().next()?;
    if !(ident_start(first) || first == '.' || first == '$') {
        return None;
    }
    let end = s
        .char_indices()
        .find(|(_, c)| !asm_ident_char(*c))
        .map_or(s.len(), |(i, _)| i);
    let name = &s[..end];
    (!name.is_empty() && name.len() <= MAX_NAME).then_some(name)
}

fn word(s: &str) -> Option<&str> {
    let first = s.chars().next()?;
    if !ident_start(first) {
        return None;
    }
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map_or(s.len(), |(i, _)| i);
    Some(&s[..end]).filter(|n| n.len() <= MAX_NAME)
}

fn clip(line: &str) -> String {
    line.trim().chars().take(MAX_CODE).collect()
}

/// A local label the assembler does not export: GNU `.L…`, numeric `1:`,
/// and MASM `@@:`. These are jump targets inside a routine, not definitions.
fn asm_local(name: &str) -> bool {
    name.starts_with(".L") || name.starts_with("L$") || name.starts_with("@@")
}

/// Directives that make a name global. GNU `.globl` / `.global`, NASM
/// `global`, MASM `PUBLIC`, and `.type name, @function`.
fn asm_exported(lines: &[&str]) -> HashSet<String> {
    let mut out = HashSet::new();
    for raw in lines {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        let rest = [".globl", ".global", "global", "public"]
            .iter()
            .find_map(|kw| {
                lower
                    .strip_prefix(kw)
                    .filter(|r| r.starts_with([' ', '\t']))
                    .map(|_| line[kw.len()..].trim())
            });
        if let Some(rest) = rest {
            for part in rest.split([',', ' ', '\t']) {
                let part = part.trim().trim_end_matches(":function").trim();
                if let Some(name) = asm_ident(part) {
                    out.insert(name.to_string());
                }
            }
            continue;
        }
        if let Some(rest) = lower.strip_prefix(".type") {
            if rest.contains("function") {
                let rest = line[5..].trim();
                if let Some(name) = asm_ident(rest.split(',').next().unwrap_or("").trim()) {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// Assembly: GNU as (`.s`, `.S`), NASM/YASM and MASM (`.asm`).
///
/// - a label at column 0 (`name:`) is a definition; exported names
///   (`.globl`, `global`, `PUBLIC`, `.type name, @function`) are `function`,
///   the rest `label`. Local labels (`.L…`, numeric) are skipped.
/// - MASM `name PROC` is a `function`, `name MACRO` a `macro`.
/// - `.macro name` and NASM `%macro name` are `macro`.
/// - `.equ name, v`, `.set name, v`, `name EQU v`, `%define name`,
///   `#define name` and a column-0 `name = v` are `const`.
///
/// A routine runs from its label to the line before the next column-0
/// definition, which is what `end_line` reports.
pub(crate) fn asm_symbols(text: &str) -> Vec<LineSym> {
    let lines: Vec<&str> = text.lines().collect();
    let exported = asm_exported(&lines);
    let mut out: Vec<LineSym> = Vec::new();
    for (row, raw) in lines.iter().enumerate() {
        if out.len() >= MAX_SYMS {
            break;
        }
        let line_no = row + 1;
        let trimmed = raw.trim_start();
        let at_col0 = trimmed.len() == raw.len();
        let lower = trimmed.to_ascii_lowercase();
        let mut push = |name: &str, kind: &'static str| {
            out.push(LineSym {
                name: name.to_string(),
                kind,
                line: line_no,
                end_line: line_no,
                code: clip(raw),
            });
        };
        // Directive forms, at any indentation.
        let directive = [
            (".macro", "macro"),
            ("%macro", "macro"),
            ("%imacro", "macro"),
            (".equ", "const"),
            (".set", "const"),
            ("%define", "const"),
            ("%xdefine", "const"),
            ("%assign", "const"),
            ("#define", "const"),
        ]
        .iter()
        .find_map(|(kw, kind)| {
            lower
                .strip_prefix(kw)
                .filter(|r| r.starts_with([' ', '\t']))
                .map(|_| (trimmed[kw.len()..].trim_start(), *kind))
        });
        if let Some((rest, kind)) = directive {
            if let Some(name) = asm_ident(rest) {
                push(name, kind);
            }
            continue;
        }
        if !at_col0 {
            continue;
        }
        let Some(name) = asm_ident(trimmed) else {
            continue;
        };
        if asm_local(name) {
            continue;
        }
        let after = &trimmed[name.len()..];
        if after.starts_with(':') && !after.starts_with("::=") {
            let kind = if exported.contains(name) {
                "function"
            } else {
                "label"
            };
            push(name, kind);
            continue;
        }
        let after_trim = after.trim_start();
        let after_lower = after_trim.to_ascii_lowercase();
        let keyword = after_lower
            .split(|c: char| c.is_whitespace() || c == ';')
            .next()
            .unwrap_or("");
        match keyword {
            "proc" => push(name, "function"),
            "macro" => push(name, "macro"),
            "equ" | "textequ" => push(name, "const"),
            _ if after_trim.starts_with('=') && !after_trim.starts_with("==") => {
                push(name, "const")
            }
            _ => {}
        }
    }
    // A routine ends where the next column-0 label or proc starts.
    let starts: Vec<usize> = out
        .iter()
        .filter(|s| matches!(s.kind, "function" | "label"))
        .map(|s| s.line)
        .collect();
    let total = lines.len().max(1);
    for sym in &mut out {
        if matches!(sym.kind, "function" | "label") {
            let next = starts.iter().find(|&&l| l > sym.line).copied();
            sym.end_line = next.map_or(total, |n| n - 1).max(sym.line);
        }
    }
    out
}

fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// GDScript (`.gd`, Godot).
///
/// - `func name(` and `static func name(` are `function` at the top level and
///   `method` inside an inner `class`;
/// - `class_name Name` and `class Name:` are `class`;
/// - `signal name` is `signal`, `enum Name` is `enum`, `const NAME` is
///   `const`;
/// - a top-level `var name` (after any `@export…` / `@onready` annotations)
///   is `field`: a member of the script's own class.
pub(crate) fn gdscript_symbols(text: &str) -> Vec<LineSym> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<LineSym> = Vec::new();
    // The open inner class: its own indent, and the indent of its members
    // once the first member line is seen.
    let mut class_indent: Option<usize> = None;
    let mut member_indent: Option<usize> = None;
    for (row, raw) in lines.iter().enumerate() {
        if out.len() >= MAX_SYMS {
            break;
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = indent_of(raw);
        match class_indent {
            Some(ci) if indent <= ci => {
                class_indent = None;
                member_indent = None;
            }
            Some(_) if member_indent.is_none() => member_indent = Some(indent),
            _ => {}
        }
        // Annotations may share the line: `@export var speed := 10.0`.
        let mut rest = trimmed;
        while let Some(stripped) = rest.strip_prefix('@') {
            let end = stripped
                .find(|c: char| c.is_whitespace())
                .unwrap_or(stripped.len());
            // `@export_range(0, 10)` carries parenthesised arguments.
            let mut cut = end;
            if let Some(open) = stripped[..end].find('(') {
                cut = stripped[open..]
                    .find(')')
                    .map_or(stripped.len(), |c| open + c + 1);
            }
            rest = stripped[cut..].trim_start();
        }
        let top = indent == 0;
        let in_class = class_indent.is_some() && member_indent == Some(indent);
        let (kw, after) = match rest.split_once(|c: char| c.is_whitespace()) {
            Some((kw, after)) => (kw, after.trim_start()),
            None => (rest, ""),
        };
        let (kind, after): (&'static str, &str) = match kw {
            "static" => match after.strip_prefix("func") {
                Some(r) if r.starts_with([' ', '\t']) => {
                    (if in_class { "method" } else { "function" }, r.trim_start())
                }
                _ => continue,
            },
            "func" => (if in_class { "method" } else { "function" }, after),
            "class_name" => ("class", after),
            "class" => ("class", after),
            "signal" if top || in_class => ("signal", after),
            "enum" if top || in_class => ("enum", after),
            "const" if top || in_class => ("const", after),
            "var" if top || in_class => ("field", after),
            _ => continue,
        };
        // A `func` nested deeper than the script or a class body is a lambda
        // or a local; only top level and inner-class members are definitions.
        if matches!(kind, "function" | "method") && !(top || in_class) {
            continue;
        }
        let Some(name) = word(after) else {
            continue;
        };
        if kw == "class" {
            class_indent = Some(indent);
            member_indent = None;
        }
        out.push(LineSym {
            name: name.to_string(),
            kind,
            line: row + 1,
            end_line: row + 1,
            code: clip(raw),
        });
    }
    // A function or class body is every following line indented deeper.
    for sym in &mut out {
        if !matches!(sym.kind, "function" | "method" | "class")
            || sym.code.starts_with("class_name")
        {
            continue;
        }
        let own = indent_of(lines[sym.line - 1]);
        let mut end = sym.line;
        for (row, raw) in lines.iter().enumerate().skip(sym.line) {
            if raw.trim().is_empty() {
                continue;
            }
            if indent_of(raw) <= own {
                break;
            }
            end = row + 1;
        }
        sym.end_line = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(syms: &[LineSym]) -> Vec<(&str, &str, usize)> {
        syms.iter()
            .map(|s| (s.name.as_str(), s.kind, s.line))
            .collect()
    }

    #[test]
    fn gnu_as_labels_and_directives() {
        let src = "\
\t.text
\t.globl\tfoo
\t.type\tfoo, @function
foo:
\tpushq\t%rbp
.L2:
\tret
bar:\t# helper
1:\tjmp 1b
\t.macro SAVE_REGS
\t.endm
\t.equ STACK_SIZE, 64
# comment: not a label
";
        let syms = asm_symbols(src);
        assert_eq!(
            names(&syms),
            vec![
                ("foo", "function", 4),
                ("bar", "label", 8),
                ("SAVE_REGS", "macro", 10),
                ("STACK_SIZE", "const", 12),
            ]
        );
        assert_eq!(syms[0].end_line, 7, "foo runs to the line before bar");
        assert_eq!(syms[0].code, "foo:");
    }

    #[test]
    fn nasm_and_masm_forms() {
        let src = "\
global start
section .text
start:
    mov eax, 1
%define BUF_LEN 256
%macro PROLOGUE 0
%endmacro
Delay PROC NEAR
    ret
Delay ENDP
PUBLIC Delay
COUNT EQU 10
LIMIT = 4
";
        let syms = asm_symbols(src);
        assert_eq!(
            names(&syms),
            vec![
                ("start", "function", 3),
                ("BUF_LEN", "const", 5),
                ("PROLOGUE", "macro", 6),
                ("Delay", "function", 8),
                ("COUNT", "const", 12),
                ("LIMIT", "const", 13),
            ]
        );
    }

    #[test]
    fn gdscript_members() {
        let src = "\
extends CharacterBody2D
class_name Player

signal died(reason)
enum State { IDLE, RUN }
const MAX_SPEED := 300.0
@export var speed := 10.0
@export_range(0, 10) var jumps: int = 2
var _hp = 3

func _ready() -> void:
\tvar local = 1
\tpass

static func make() -> Player:
\treturn Player.new()

func jump():
\tvelocity.y = -400
\tvar f = func(x): return x

class Inventory:
\tvar items = []
\tfunc add(item):
\t\tvar n = 1
\t\titems.append(item)
";
        let syms = gdscript_symbols(src);
        assert_eq!(
            names(&syms),
            vec![
                ("Player", "class", 2),
                ("died", "signal", 4),
                ("State", "enum", 5),
                ("MAX_SPEED", "const", 6),
                ("speed", "field", 7),
                ("jumps", "field", 8),
                ("_hp", "field", 9),
                ("_ready", "function", 11),
                ("make", "function", 15),
                ("jump", "function", 18),
                ("Inventory", "class", 22),
                ("items", "field", 23),
                ("add", "method", 24),
            ]
        );
        let jump = syms.iter().find(|s| s.name == "jump").unwrap();
        assert_eq!(jump.end_line, 20);
        let inv = syms.iter().find(|s| s.name == "Inventory").unwrap();
        assert_eq!(inv.end_line, 26);
    }
}
