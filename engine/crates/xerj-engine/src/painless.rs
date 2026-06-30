//! Minimal Painless-script interpreter for ES script_score / rescore /
//! runtime-field workloads.
//!
//! This is NOT a full Painless implementation. It supports the subset
//! observed across the ES YAML compat test suite, which is sufficient
//! for the script-driven scoring/rescore tests:
//!
//! * Identifiers + members:
//!   - `doc['field'].value` and `doc.field.value` → numeric or string
//!     field value (first if multi-valued)
//!   - `params.NAME` → reference into the script's params object
//!   - `_score` → current document score
//! * Literals:
//!   - integer / float / string / true / false
//! * Operators:
//!   - arithmetic `+ - * / %`
//!   - comparison `< <= > >= == !=`
//!   - logical `&& || !`
//!   - ternary `cond ? a : b`
//!   - unary `- !`
//! * Control flow:
//!   - `if (cond) { ... } else { ... }`
//!   - explicit `return X;` and implicit return (last expression)
//!   - statement separators `;`
//!   - blocks `{ ... }`
//! * Variable bindings:
//!   - `double x = ...;`, `int x = ...;`, `def x = ...;`, `String x = ...;`
//!   - `x` reads, `x = ...` writes
//! * Functions / methods:
//!   - `dotProduct(params.q, 'field')` over a numeric vector field
//!   - `Math.max(a, b)`, `Math.min(a, b)`, `Math.abs(x)`, `Math.log(x)`,
//!     `Math.sqrt(x)`, `Math.pow(a, b)`
//!
//! Anything outside that subset returns an error from `eval()`. Callers
//! should fall back to a no-op score on script error.

use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum PainlessValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<PainlessValue>),
    /// A JSON object — used for `params['_source']` in runtime field
    /// scripts. `.toString()` renders it in ES's HashMap-like format
    /// (`{key=value, key=value}`, keys alphabetically sorted).
    Object(serde_json::Map<String, Value>),
}

impl PainlessValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PainlessValue::Number(n) => Some(*n),
            PainlessValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            PainlessValue::String(s) => s.parse().ok(),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            PainlessValue::Bool(b) => *b,
            PainlessValue::Number(n) => *n != 0.0,
            PainlessValue::Null => false,
            PainlessValue::String(s) => !s.is_empty(),
            PainlessValue::Array(a) => !a.is_empty(),
            PainlessValue::Object(o) => !o.is_empty(),
        }
    }
    pub fn from_json(v: &Value) -> Self {
        match v {
            Value::Null => PainlessValue::Null,
            Value::Bool(b) => PainlessValue::Bool(*b),
            Value::Number(n) => PainlessValue::Number(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => PainlessValue::String(s.clone()),
            Value::Array(arr) => PainlessValue::Array(arr.iter().map(PainlessValue::from_json).collect()),
            Value::Object(o) => PainlessValue::Object(o.clone()),
        }
    }
}

/// Per-evaluation context: doc source, params, score.
pub struct PainlessCtx<'a> {
    pub doc: &'a Value,
    pub params: &'a Value,
    pub score: f32,
    /// Mutable accumulator for runtime-field `emit()` calls. None for
    /// non-runtime contexts (script_score, rescore, etc.) where emit()
    /// is not used.
    pub emits: std::cell::RefCell<Vec<PainlessValue>>,
}

impl<'a> PainlessCtx<'a> {
    pub fn new(doc: &'a Value, params: &'a Value, score: f32) -> Self {
        Self { doc, params, score, emits: std::cell::RefCell::new(Vec::new()) }
    }
    pub fn take_emits(&self) -> Vec<PainlessValue> {
        std::mem::take(&mut *self.emits.borrow_mut())
    }
}

// ── Tokenisation ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Number(f64),
    String(String),
    Ident(String),
    Punct(char),
    PunctMulti(String),
    Keyword(String),
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let bytes = src.as_bytes();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        // Skip whitespace
        if c.is_whitespace() { i += 1; continue; }
        // Comments
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
            while i < bytes.len() && bytes[i] as char != '\n' { i += 1; }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] as char == '*' && bytes[i + 1] as char == '/') { i += 1; }
            i += 2;
            continue;
        }
        // Number literal
        if c.is_ascii_digit() || (c == '.' && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit()) {
            let start = i;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_ascii_digit() || cc == '.' || cc == 'e' || cc == 'E' || cc == '-' || cc == '+' {
                    // Allow signed exponent
                    if (cc == '-' || cc == '+') && !matches!(bytes[i - 1] as char, 'e' | 'E') { break; }
                    i += 1;
                } else { break; }
            }
            // Strip trailing 'L'/'F'/'D' type suffix Painless allows.
            let s_end = i;
            let s = &src[start..s_end];
            // Strip suffix from the parsed string for f64 parsing.
            let mut s_clean = s.to_string();
            i = s_end;
            if i < bytes.len() {
                let t = bytes[i] as char;
                if matches!(t, 'L' | 'l' | 'F' | 'f' | 'D' | 'd') {
                    i += 1;
                }
            }
            // strip possibly trailing "L" already in string for safety
            s_clean = s_clean.trim_end_matches(|c: char| matches!(c, 'L' | 'l' | 'F' | 'f' | 'D' | 'd')).to_string();
            let n: f64 = s_clean.parse().map_err(|e| format!("bad number {s_clean}: {e}"))?;
            out.push(Tok::Number(n));
            continue;
        }
        // String literal
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] as char != quote {
                if bytes[i] as char == '\\' && i + 1 < bytes.len() { i += 2; } else { i += 1; }
            }
            if i >= bytes.len() { return Err("unterminated string".into()); }
            let raw = &src[start..i];
            i += 1;
            // Basic escape handling.
            let mut buf = String::with_capacity(raw.len());
            let mut chars = raw.chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    if let Some(n) = chars.next() {
                        match n {
                            'n' => buf.push('\n'),
                            't' => buf.push('\t'),
                            'r' => buf.push('\r'),
                            '\\' => buf.push('\\'),
                            '"' => buf.push('"'),
                            '\'' => buf.push('\''),
                            other => buf.push(other),
                        }
                    }
                } else {
                    buf.push(ch);
                }
            }
            out.push(Tok::String(buf));
            continue;
        }
        // Identifier / keyword
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_alphanumeric() || cc == '_' || cc == '$' { i += 1; } else { break; }
            }
            let s = &src[start..i];
            match s {
                "if" | "else" | "return" | "true" | "false" | "null" |
                "double" | "int" | "long" | "float" | "boolean" | "String" |
                "def" | "var" | "for" | "while" | "break" | "continue" |
                "new" | "instanceof" => out.push(Tok::Keyword(s.to_string())),
                _ => out.push(Tok::Ident(s.to_string())),
            }
            continue;
        }
        // Multi-char punctuation
        if i + 1 < bytes.len() {
            let two: String = format!("{}{}", c, bytes[i + 1] as char);
            if matches!(two.as_str(), "==" | "!=" | "<=" | ">=" | "&&" | "||" | "->" | "+=" | "-=" | "*=" | "/=" | "%=" | "++" | "--") {
                out.push(Tok::PunctMulti(two));
                i += 2;
                continue;
            }
        }
        // Single-char punctuation
        if matches!(c, '(' | ')' | '{' | '}' | '[' | ']' | ',' | ';' | '.' | ':' | '?' | '+' | '-' | '*' | '/' | '%' | '<' | '>' | '=' | '!' | '&' | '|') {
            out.push(Tok::Punct(c));
            i += 1;
            continue;
        }
        return Err(format!("unexpected char '{}' at {}", c, i));
    }
    Ok(out)
}

// ── AST ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Expr {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Ident(String),
    /// `.field` or `.method(args)` member access.
    Member(Box<Expr>, String, Option<Vec<Expr>>),
    /// `obj[key]` index access.
    Index(Box<Expr>, Box<Expr>),
    /// `f(args)` call on a top-level identifier.
    Call(String, Vec<Expr>),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `var x = expr` (declare); `x = expr` (assign).
    Assign(String, Box<Expr>, bool /* is_decl */),
}

#[derive(Debug, Clone)]
enum Stmt {
    Expr(Expr),
    Return(Option<Expr>),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    Block(Vec<Stmt>),
}

// ── Parser ───────────────────────────────────────────────────────────────────

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok]) -> Self { Self { toks, pos: 0 } }
    fn peek(&self) -> Option<&Tok> { self.toks.get(self.pos) }
    fn eat(&mut self) -> Option<Tok> { let t = self.toks.get(self.pos).cloned(); if t.is_some() { self.pos += 1; } t }
    fn expect_punct(&mut self, c: char) -> Result<(), String> {
        match self.eat() {
            Some(Tok::Punct(p)) if p == c => Ok(()),
            other => Err(format!("expected '{}' got {:?}", c, other)),
        }
    }
    fn match_punct(&mut self, c: char) -> bool {
        if let Some(Tok::Punct(p)) = self.peek() {
            if *p == c { self.pos += 1; return true; }
        }
        false
    }
    fn match_keyword(&mut self, kw: &str) -> bool {
        if let Some(Tok::Keyword(s)) = self.peek() {
            if s == kw { self.pos += 1; return true; }
        }
        false
    }
    fn parse_program(&mut self) -> Result<Vec<Stmt>, String> {
        let mut out: Vec<Stmt> = Vec::new();
        while self.peek().is_some() {
            out.push(self.parse_stmt()?);
        }
        Ok(out)
    }
    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        // `if (...) { ... } else { ... }`
        if self.match_keyword("if") {
            self.expect_punct('(')?;
            let cond = self.parse_expr()?;
            self.expect_punct(')')?;
            let then_body = self.parse_block_or_stmt()?;
            let else_body = if self.match_keyword("else") {
                self.parse_block_or_stmt()?
            } else { Vec::new() };
            return Ok(Stmt::If(cond, then_body, else_body));
        }
        if self.match_keyword("return") {
            // Optional expression then ;
            let e = if self.match_punct(';') {
                None
            } else {
                let e = self.parse_expr()?;
                let _ = self.match_punct(';');
                Some(e)
            };
            return Ok(Stmt::Return(e));
        }
        if let Some(Tok::Punct('{')) = self.peek() {
            let block = self.parse_block_or_stmt()?;
            return Ok(Stmt::Block(block));
        }
        // Variable decl: `<type> NAME = expr;`
        if let Some(Tok::Keyword(kw)) = self.peek().cloned() {
            if matches!(kw.as_str(), "double" | "int" | "long" | "float" | "boolean" | "String" | "def" | "var") {
                self.pos += 1;
                let name = match self.eat() {
                    Some(Tok::Ident(n)) => n,
                    other => return Err(format!("expected identifier after type got {:?}", other)),
                };
                if !self.match_punct('=') {
                    return Err(format!("expected '=' after var name '{}'", name));
                }
                let val = self.parse_expr()?;
                let _ = self.match_punct(';');
                return Ok(Stmt::Expr(Expr::Assign(name, Box::new(val), true)));
            }
        }
        let e = self.parse_expr()?;
        let _ = self.match_punct(';');
        Ok(Stmt::Expr(e))
    }
    fn parse_block_or_stmt(&mut self) -> Result<Vec<Stmt>, String> {
        if self.match_punct('{') {
            let mut out = Vec::new();
            while let Some(t) = self.peek() {
                if matches!(t, Tok::Punct('}')) { break; }
                out.push(self.parse_stmt()?);
            }
            self.expect_punct('}')?;
            Ok(out)
        } else {
            Ok(vec![self.parse_stmt()?])
        }
    }
    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_assign()
    }
    fn parse_assign(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_ternary()?;
        if self.match_punct('=') {
            // Disambiguate from `==` already consumed by parse_compare.
            let rhs = self.parse_assign()?;
            if let Expr::Ident(name) = lhs {
                return Ok(Expr::Assign(name, Box::new(rhs), false));
            }
            return Err("assignment target must be identifier".into());
        }
        Ok(lhs)
    }
    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_or()?;
        if self.match_punct('?') {
            let then_e = self.parse_assign()?;
            self.expect_punct(':')?;
            let else_e = self.parse_assign()?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(then_e), Box::new(else_e)));
        }
        Ok(cond)
    }
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_and()?;
        while let Some(Tok::PunctMulti(op)) = self.peek() {
            if op == "||" { self.pos += 1; let rhs = self.parse_and()?; lhs = Expr::Binary("||".into(), Box::new(lhs), Box::new(rhs)); }
            else { break; }
        }
        Ok(lhs)
    }
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_eq()?;
        while let Some(Tok::PunctMulti(op)) = self.peek() {
            if op == "&&" { self.pos += 1; let rhs = self.parse_eq()?; lhs = Expr::Binary("&&".into(), Box::new(lhs), Box::new(rhs)); }
            else { break; }
        }
        Ok(lhs)
    }
    fn parse_eq(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_compare()?;
        while let Some(t) = self.peek() {
            let op = match t {
                Tok::PunctMulti(s) if s == "==" || s == "!=" => s.clone(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_compare()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn parse_compare(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_add()?;
        while let Some(t) = self.peek() {
            let op = match t {
                Tok::PunctMulti(s) if s == "<=" || s == ">=" => s.clone(),
                Tok::Punct('<') => "<".to_string(),
                Tok::Punct('>') => ">".to_string(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_add()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_mul()?;
        while let Some(t) = self.peek() {
            let op = match t {
                Tok::Punct('+') => "+".to_string(),
                Tok::Punct('-') => "-".to_string(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_mul()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_unary()?;
        while let Some(t) = self.peek() {
            let op = match t {
                Tok::Punct('*') => "*".to_string(),
                Tok::Punct('/') => "/".to_string(),
                Tok::Punct('%') => "%".to_string(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.match_punct('-') {
            let e = self.parse_unary()?;
            return Ok(Expr::Unary("-".into(), Box::new(e)));
        }
        if self.match_punct('!') {
            let e = self.parse_unary()?;
            return Ok(Expr::Unary("!".into(), Box::new(e)));
        }
        if self.match_punct('+') {
            return self.parse_unary();
        }
        self.parse_postfix()
    }
    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_primary()?;
        loop {
            if self.match_punct('.') {
                // member: ident, possibly followed by call
                let name = match self.eat() {
                    Some(Tok::Ident(n)) => n,
                    Some(Tok::Keyword(n)) => n, // allow .value etc. that hit type kw rare
                    other => return Err(format!("expected member name got {:?}", other)),
                };
                if self.match_punct('(') {
                    let args = self.parse_args(')')?;
                    e = Expr::Member(Box::new(e), name, Some(args));
                } else {
                    e = Expr::Member(Box::new(e), name, None);
                }
            } else if self.match_punct('[') {
                let idx = self.parse_expr()?;
                self.expect_punct(']')?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else { break; }
        }
        Ok(e)
    }
    fn parse_args(&mut self, end: char) -> Result<Vec<Expr>, String> {
        let mut out: Vec<Expr> = Vec::new();
        if let Some(Tok::Punct(c)) = self.peek() {
            if *c == end { self.pos += 1; return Ok(out); }
        }
        loop {
            out.push(self.parse_expr()?);
            if self.match_punct(',') { continue; }
            break;
        }
        match self.eat() {
            Some(Tok::Punct(c)) if c == end => Ok(out),
            other => Err(format!("expected '{}' got {:?}", end, other)),
        }
    }
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.eat() {
            Some(Tok::Number(n)) => Ok(Expr::Number(n)),
            Some(Tok::String(s)) => Ok(Expr::String(s)),
            Some(Tok::Keyword(k)) => match k.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "null" => Ok(Expr::Null),
                other => Err(format!("unexpected keyword {} in expression", other)),
            },
            Some(Tok::Ident(name)) => {
                if self.match_punct('(') {
                    let args = self.parse_args(')')?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            Some(Tok::Punct('(')) => {
                let e = self.parse_expr()?;
                self.expect_punct(')')?;
                Ok(e)
            }
            other => Err(format!("unexpected token {:?}", other)),
        }
    }
}

// ── Evaluation ───────────────────────────────────────────────────────────────

pub fn eval_painless(src: &str, ctx: &PainlessCtx) -> Result<PainlessValue, String> {
    let toks = tokenize(src)?;
    let mut p = Parser::new(&toks);
    let stmts = p.parse_program()?;
    let mut env: HashMap<String, PainlessValue> = HashMap::new();
    let mut ret: Option<PainlessValue> = None;
    let mut last: PainlessValue = PainlessValue::Null;
    for stmt in &stmts {
        match exec_stmt(stmt, ctx, &mut env)? {
            ExecOutcome::Return(v) => { ret = Some(v); break; }
            ExecOutcome::Value(v) => { last = v; }
        }
    }
    Ok(ret.unwrap_or(last))
}

enum ExecOutcome { Return(PainlessValue), Value(PainlessValue) }

fn exec_stmt(s: &Stmt, ctx: &PainlessCtx, env: &mut HashMap<String, PainlessValue>) -> Result<ExecOutcome, String> {
    match s {
        Stmt::Return(opt) => {
            let v = match opt {
                Some(e) => eval_expr(e, ctx, env)?,
                None => PainlessValue::Null,
            };
            Ok(ExecOutcome::Return(v))
        }
        Stmt::Expr(e) => Ok(ExecOutcome::Value(eval_expr(e, ctx, env)?)),
        Stmt::If(cond, then_b, else_b) => {
            let cv = eval_expr(cond, ctx, env)?;
            let body = if cv.as_bool() { then_b } else { else_b };
            for stmt in body {
                match exec_stmt(stmt, ctx, env)? {
                    o @ ExecOutcome::Return(_) => return Ok(o),
                    ExecOutcome::Value(_) => {}
                }
            }
            Ok(ExecOutcome::Value(PainlessValue::Null))
        }
        Stmt::Block(stmts) => {
            for st in stmts {
                match exec_stmt(st, ctx, env)? {
                    o @ ExecOutcome::Return(_) => return Ok(o),
                    ExecOutcome::Value(_) => {}
                }
            }
            Ok(ExecOutcome::Value(PainlessValue::Null))
        }
    }
}

fn eval_expr(e: &Expr, ctx: &PainlessCtx, env: &mut HashMap<String, PainlessValue>) -> Result<PainlessValue, String> {
    match e {
        Expr::Number(n) => Ok(PainlessValue::Number(*n)),
        Expr::String(s) => Ok(PainlessValue::String(s.clone())),
        Expr::Bool(b) => Ok(PainlessValue::Bool(*b)),
        Expr::Null => Ok(PainlessValue::Null),
        Expr::Ident(name) => {
            if let Some(v) = env.get(name) { return Ok(v.clone()); }
            match name.as_str() {
                "_score" => Ok(PainlessValue::Number(ctx.score as f64)),
                "doc" => Ok(PainlessValue::Null), // marker; resolved via Member/Index
                "params" => Ok(PainlessValue::Null), // marker; resolved via Member
                _ => Err(format!("unknown identifier '{}'", name)),
            }
        }
        Expr::Assign(name, val, _is_decl) => {
            let v = eval_expr(val, ctx, env)?;
            env.insert(name.clone(), v.clone());
            Ok(v)
        }
        Expr::Unary(op, x) => {
            let v = eval_expr(x, ctx, env)?;
            match op.as_str() {
                "-" => Ok(PainlessValue::Number(-v.as_f64().unwrap_or(0.0))),
                "!" => Ok(PainlessValue::Bool(!v.as_bool())),
                _ => Err(format!("bad unary {op}")),
            }
        }
        Expr::Binary(op, a, b) => {
            // Short-circuit && ||
            if op == "&&" {
                let av = eval_expr(a, ctx, env)?;
                if !av.as_bool() { return Ok(PainlessValue::Bool(false)); }
                return Ok(PainlessValue::Bool(eval_expr(b, ctx, env)?.as_bool()));
            }
            if op == "||" {
                let av = eval_expr(a, ctx, env)?;
                if av.as_bool() { return Ok(PainlessValue::Bool(true)); }
                return Ok(PainlessValue::Bool(eval_expr(b, ctx, env)?.as_bool()));
            }
            let av = eval_expr(a, ctx, env)?;
            let bv = eval_expr(b, ctx, env)?;
            // String concatenation for `+`.
            if op == "+" {
                if matches!(av, PainlessValue::String(_)) || matches!(bv, PainlessValue::String(_)) {
                    let sa = match &av {
                        PainlessValue::String(s) => s.clone(),
                        PainlessValue::Number(n) => format_num(*n),
                        PainlessValue::Bool(b) => b.to_string(),
                        _ => "null".to_string(),
                    };
                    let sb = match &bv {
                        PainlessValue::String(s) => s.clone(),
                        PainlessValue::Number(n) => format_num(*n),
                        PainlessValue::Bool(b) => b.to_string(),
                        _ => "null".to_string(),
                    };
                    return Ok(PainlessValue::String(format!("{sa}{sb}")));
                }
            }
            let an = av.as_f64().unwrap_or(0.0);
            let bn = bv.as_f64().unwrap_or(0.0);
            let r = match op.as_str() {
                "+" => an + bn,
                "-" => an - bn,
                "*" => an * bn,
                "/" => if bn == 0.0 { f64::NAN } else { an / bn },
                "%" => if bn == 0.0 { f64::NAN } else { an % bn },
                "<" => return Ok(PainlessValue::Bool(an < bn)),
                "<=" => return Ok(PainlessValue::Bool(an <= bn)),
                ">" => return Ok(PainlessValue::Bool(an > bn)),
                ">=" => return Ok(PainlessValue::Bool(an >= bn)),
                "==" => return Ok(PainlessValue::Bool(an == bn)),
                "!=" => return Ok(PainlessValue::Bool(an != bn)),
                _ => return Err(format!("bad binary {op}")),
            };
            Ok(PainlessValue::Number(r))
        }
        Expr::Ternary(c, t, f) => {
            let cv = eval_expr(c, ctx, env)?;
            if cv.as_bool() { eval_expr(t, ctx, env) } else { eval_expr(f, ctx, env) }
        }
        Expr::Index(base, idx) => {
            // Special-case `doc['field']` / `params['x']`.
            if let Expr::Ident(name) = base.as_ref() {
                let key = match eval_expr(idx, ctx, env)? {
                    PainlessValue::String(s) => s,
                    PainlessValue::Number(n) => format_num(n),
                    other => return Err(format!("non-string index: {:?}", other)),
                };
                if name == "doc" {
                    // Return a marker via DocField wrapper using PainlessValue::Array
                    // hack — we represent doc-field references as "doc:field" so that
                    // .value can resolve them. Stored as a String value.
                    return Ok(PainlessValue::String(format!("__docref__:{}", key)));
                }
                if name == "params" {
                    // `params['_source']` → the doc source object.
                    // ES exposes the source under that key for runtime
                    // field scripts.
                    if key == "_source" {
                        return Ok(PainlessValue::from_json(ctx.doc));
                    }
                    let v = ctx.params.get(&key).cloned().unwrap_or(Value::Null);
                    return Ok(PainlessValue::from_json(&v));
                }
            }
            // General index access on arrays.
            let bv = eval_expr(base, ctx, env)?;
            let key = eval_expr(idx, ctx, env)?;
            match (bv, key) {
                (PainlessValue::Array(arr), PainlessValue::Number(n)) => {
                    let i = n as usize;
                    Ok(arr.get(i).cloned().unwrap_or(PainlessValue::Null))
                }
                _ => Ok(PainlessValue::Null),
            }
        }
        Expr::Member(base, member, args) => {
            // doc.field.value
            // doc['field'].value
            // params.foo
            // Math.foo(args)
            if let Expr::Ident(name) = base.as_ref() {
                if name == "params" && args.is_none() {
                    let v = ctx.params.get(member).cloned().unwrap_or(Value::Null);
                    return Ok(PainlessValue::from_json(&v));
                }
                if name == "doc" && args.is_none() {
                    // doc.field → marker
                    return Ok(PainlessValue::String(format!("__docref__:{}", member)));
                }
                if name == "Math" {
                    let argvs: Vec<PainlessValue> = match args {
                        Some(args) => args.iter().map(|a| eval_expr(a, ctx, env)).collect::<Result<_, _>>()?,
                        None => Vec::new(),
                    };
                    return math_call(member, &argvs);
                }
            }
            let bv = eval_expr(base, ctx, env)?;
            // String marker → resolve doc field then access .value or .size or .length
            if let PainlessValue::String(s) = &bv {
                if let Some(field) = s.strip_prefix("__docref__:") {
                    return resolve_doc_member(ctx, field, member, args, env);
                }
                // Methods on String: .length(), .toString(), .toLowerCase(), .toUpperCase().
                match member.as_str() {
                    "length" => return Ok(PainlessValue::Number(s.chars().count() as f64)),
                    "toString" => return Ok(PainlessValue::String(s.clone())),
                    "toLowerCase" => return Ok(PainlessValue::String(s.to_lowercase())),
                    "toUpperCase" => return Ok(PainlessValue::String(s.to_uppercase())),
                    _ => {}
                }
            }
            // Object methods: .toString() renders as ES-compatible
            // HashMap.toString format `{key=value, key=value, ...}` with
            // keys alphabetically sorted (matches Java HashMap toString
            // for the YAML test expectation).
            if let PainlessValue::Object(map) = &bv {
                match member.as_str() {
                    "toString" => return Ok(PainlessValue::String(render_es_map(map))),
                    "size" => return Ok(PainlessValue::Number(map.len() as f64)),
                    "isEmpty" => return Ok(PainlessValue::Bool(map.is_empty())),
                    _ => {
                        // Unknown member — fall through to dotted-key
                        // lookup.
                        if args.is_none() {
                            if let Some(v) = map.get(member) {
                                return Ok(PainlessValue::from_json(v));
                            }
                        }
                    }
                }
            }
            if let PainlessValue::Array(arr) = &bv {
                match member.as_str() {
                    "size" | "length" => return Ok(PainlessValue::Number(arr.len() as f64)),
                    "isEmpty" => return Ok(PainlessValue::Bool(arr.is_empty())),
                    _ => {}
                }
            }
            Err(format!("unsupported member access .{}", member))
        }
        Expr::Call(name, args) => {
            let argvs: Vec<PainlessValue> = args.iter().map(|a| eval_expr(a, ctx, env)).collect::<Result<_, _>>()?;
            global_call(name, &argvs, ctx)
        }
    }
}

fn format_num(n: f64) -> String {
    if (n - n.trunc()).abs() < f64::EPSILON && n.abs() < 1e16 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Render a serde_json::Map as ES's HashMap.toString format
/// (`{key=value, key=value, ...}`). Keys are emitted in INSERTION
/// order — matches Java LinkedHashMap.toString and ES's runtime
/// field rendering of `params['_source']`.
fn render_es_map(map: &serde_json::Map<String, Value>) -> String {
    fn render_val(v: &Value) -> String {
        match v {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => format_num(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                let parts: Vec<String> = arr.iter().map(render_val).collect();
                format!("[{}]", parts.join(", "))
            }
            Value::Object(o) => render_es_map(o),
        }
    }
    let mut parts: Vec<String> = Vec::with_capacity(map.len());
    for (k, v) in map {
        parts.push(format!("{}={}", k, render_val(v)));
    }
    format!("{{{}}}", parts.join(", "))
}

fn resolve_doc_member(
    ctx: &PainlessCtx,
    field: &str,
    member: &str,
    args: &Option<Vec<Expr>>,
    _env: &mut HashMap<String, PainlessValue>,
) -> Result<PainlessValue, String> {
    let raw = get_doc_value(ctx.doc, field);
    match member {
        "value" => {
            // Return first scalar.
            match raw {
                Value::Array(arr) => Ok(arr.first().map(|v| PainlessValue::from_json(v)).unwrap_or(PainlessValue::Number(0.0))),
                Value::Number(n) => Ok(PainlessValue::Number(n.as_f64().unwrap_or(0.0))),
                Value::String(s) => Ok(PainlessValue::String(s)),
                Value::Bool(b) => Ok(PainlessValue::Bool(b)),
                _ => Ok(PainlessValue::Number(0.0)),
            }
        }
        "size" | "length" => {
            if args.is_some() {
                // doc[...].size() with explicit call
            }
            let len = match raw {
                Value::Array(arr) => arr.len(),
                Value::Null => 0,
                _ => 1,
            };
            Ok(PainlessValue::Number(len as f64))
        }
        "empty" => {
            let len = match raw {
                Value::Array(arr) => arr.len(),
                Value::Null => 0,
                _ => 1,
            };
            Ok(PainlessValue::Bool(len == 0))
        }
        _ => Err(format!("unsupported doc member .{}", member)),
    }
}

fn get_doc_value(doc: &Value, field: &str) -> Value {
    let parts: Vec<&str> = field.split('.').collect();
    let mut cur = doc.clone();
    for part in &parts {
        match cur {
            Value::Object(obj) => {
                cur = obj.get(*part).cloned().unwrap_or(Value::Null);
            }
            Value::Array(arr) => {
                // Re-walk each element and collect.
                let collected: Vec<Value> = arr
                    .iter()
                    .map(|e| {
                        let mut sub = e.clone();
                        for p in parts.iter() {
                            if let Value::Object(obj) = &sub {
                                sub = obj.get(*p).cloned().unwrap_or(Value::Null);
                            } else { sub = Value::Null; break; }
                        }
                        sub
                    })
                    .collect();
                return Value::Array(collected);
            }
            _ => return Value::Null,
        }
    }
    cur
}

fn math_call(name: &str, args: &[PainlessValue]) -> Result<PainlessValue, String> {
    let nums: Vec<f64> = args.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
    let r = match (name, nums.len()) {
        ("max", 2) => nums[0].max(nums[1]),
        ("min", 2) => nums[0].min(nums[1]),
        ("abs", 1) => nums[0].abs(),
        ("log", 1) => nums[0].ln(),
        ("log10", 1) => nums[0].log10(),
        ("sqrt", 1) => nums[0].sqrt(),
        ("pow", 2) => nums[0].powf(nums[1]),
        ("exp", 1) => nums[0].exp(),
        ("floor", 1) => nums[0].floor(),
        ("ceil", 1) => nums[0].ceil(),
        ("round", 1) => nums[0].round(),
        ("PI", 0) => std::f64::consts::PI,
        ("E", 0) => std::f64::consts::E,
        _ => return Err(format!("unsupported Math.{} arity {}", name, nums.len())),
    };
    Ok(PainlessValue::Number(r))
}

fn global_call(name: &str, args: &[PainlessValue], ctx: &PainlessCtx) -> Result<PainlessValue, String> {
    match name {
        "emit" => {
            // Runtime-field emit — records each call's value into the
            // ctx accumulator. Script source then returns Null
            // (irrelevant).
            for a in args {
                ctx.emits.borrow_mut().push(a.clone());
            }
            Ok(PainlessValue::Null)
        }
        "dotProduct" => {
            // dotProduct(query_vec, 'field') OR dotProduct(query_vec, [doc_vec])
            if args.len() != 2 {
                return Err(format!("dotProduct expects 2 args, got {}", args.len()));
            }
            let query: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
                _ => return Err("dotProduct arg 0 must be array".into()),
            };
            let doc_vec: Vec<f64> = match &args[1] {
                PainlessValue::String(s) => {
                    // Field reference (literal name).
                    let raw = get_doc_value(ctx.doc, s);
                    match raw {
                        Value::Array(arr) => arr.iter().filter_map(|v| v.as_f64()).collect(),
                        _ => Vec::new(),
                    }
                }
                PainlessValue::Array(arr) => arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
                _ => return Err("dotProduct arg 1 must be field name or array".into()),
            };
            if query.len() != doc_vec.len() {
                return Err(format!("dim mismatch: {} vs {}", query.len(), doc_vec.len()));
            }
            let dot: f64 = query.iter().zip(doc_vec.iter()).map(|(a, b)| a * b).sum();
            Ok(PainlessValue::Number(dot))
        }
        "cosineSimilarity" => {
            if args.len() != 2 { return Err("cosineSimilarity expects 2 args".into()); }
            let q: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
                _ => return Err("cosineSimilarity arg 0 must be array".into()),
            };
            let d: Vec<f64> = match &args[1] {
                PainlessValue::String(s) => {
                    let raw = get_doc_value(ctx.doc, s);
                    match raw {
                        Value::Array(arr) => arr.iter().filter_map(|v| v.as_f64()).collect(),
                        _ => Vec::new(),
                    }
                }
                PainlessValue::Array(arr) => arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
                _ => return Err("cosineSimilarity arg 1 must be field name".into()),
            };
            if q.len() != d.len() { return Err("dim mismatch".into()); }
            let dot: f64 = q.iter().zip(&d).map(|(a, b)| a * b).sum();
            let nq: f64 = q.iter().map(|v| v * v).sum::<f64>().sqrt();
            let nd: f64 = d.iter().map(|v| v * v).sum::<f64>().sqrt();
            let denom = nq * nd;
            Ok(PainlessValue::Number(if denom > 0.0 { dot / denom } else { 0.0 }))
        }
        "l1norm" | "l1Norm" => {
            if args.len() != 2 { return Err("l1norm expects 2 args".into()); }
            let q: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
                _ => return Err("l1norm arg 0 must be array".into()),
            };
            let d: Vec<f64> = match &args[1] {
                PainlessValue::String(s) => {
                    let raw = get_doc_value(ctx.doc, s);
                    match raw {
                        Value::Array(arr) => arr.iter().filter_map(|v| v.as_f64()).collect(),
                        _ => Vec::new(),
                    }
                }
                _ => return Err("l1norm arg 1 must be field name".into()),
            };
            let s: f64 = q.iter().zip(&d).map(|(a, b)| (a - b).abs()).sum();
            Ok(PainlessValue::Number(s))
        }
        "l2norm" | "l2Norm" => {
            if args.len() != 2 { return Err("l2norm expects 2 args".into()); }
            let q: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
                _ => return Err("l2norm arg 0 must be array".into()),
            };
            let d: Vec<f64> = match &args[1] {
                PainlessValue::String(s) => {
                    let raw = get_doc_value(ctx.doc, s);
                    match raw {
                        Value::Array(arr) => arr.iter().filter_map(|v| v.as_f64()).collect(),
                        _ => Vec::new(),
                    }
                }
                _ => return Err("l2norm arg 1 must be field name".into()),
            };
            let s: f64 = q.iter().zip(&d).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
            Ok(PainlessValue::Number(s))
        }
        "sigmoid" => {
            if args.len() != 1 { return Err("sigmoid expects 1 arg".into()); }
            let x = args[0].as_f64().unwrap_or(0.0);
            Ok(PainlessValue::Number(1.0 / (1.0 + (-x).exp())))
        }
        _ => Err(format!("unsupported function {}", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx<'a>(doc: &'a Value, params: &'a Value, score: f32) -> PainlessCtx<'a> {
        PainlessCtx::new(doc, params, score)
    }

    #[test]
    fn doc_value_times_param() {
        let doc = json!({"num_likes": 150});
        let params = json!({"multiplier": 10});
        let v = eval_painless("doc['num_likes'].value * params.multiplier", &ctx(&doc, &params, 0.0)).unwrap();
        assert!((v.as_f64().unwrap() - 1500.0).abs() < 1e-9);
    }

    #[test]
    fn score_plus_field() {
        let doc = json!({"x": 5});
        let params = json!({});
        let v = eval_painless("_score + doc['x'].value", &ctx(&doc, &params, 2.5)).unwrap();
        assert!((v.as_f64().unwrap() - 7.5).abs() < 1e-9);
    }

    #[test]
    fn ternary_dot_product() {
        let doc = json!({"vec": [1.0, 2.0, 3.0]});
        let params = json!({"q": [1.0, 0.0, -1.0]});
        let src = "double s = dotProduct(params.q, 'vec'); return s < 0 ? 1.0 / (1.0 - s) : s + 1.0;";
        let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
        // dot = 1*1 + 2*0 + 3*-1 = -2 → 1/(1-(-2)) = 1/3
        assert!((v.as_f64().unwrap() - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn if_return() {
        let doc = json!({"x": 10});
        let params = json!({});
        let v = eval_painless("if (doc['x'].value > 5) { return 100; } return 0;", &ctx(&doc, &params, 0.0)).unwrap();
        assert!((v.as_f64().unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn math_max() {
        let doc = json!({});
        let params = json!({});
        let v = eval_painless("Math.max(1.5, 2.5)", &ctx(&doc, &params, 0.0)).unwrap();
        assert!((v.as_f64().unwrap() - 2.5).abs() < 1e-9);
    }
}
