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
//! * Local functions and lambdas (needed by e.g. OpenSearch's UBI sample
//!   dashboards, which filter via a `Supplier`-style boolean helper):
//!   - top-level declarations `<type> name(<type> arg, ...) { ... }`
//!   - no-arg-or-more lambda literals `(a, b) -> expr` / `(a, b) -> { ... }`,
//!     stored as a closure value and invoked either by calling the
//!     function/variable name directly (`compare(...)`) or via any
//!     `.method(args)` call on a closure value (`s.get()`, `fn.apply(x)`,
//!     ...) — the method name is ignored, only positional args matter,
//!     which covers `Supplier`/`Function`/`BiFunction`/`Predicate` etc.
//!     without hard-coding each functional interface.
//!
//! Anything outside that subset returns an error from `eval()`. Callers
//! should fall back to a no-op score on script error.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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
    /// A local function or lambda value: parameter names + `Rc`-shared body
    /// statements (cheap to clone — see [`Expr::Lambda`]). Invoked either
    /// as a bare call (`name(args)`) or via any `.method(args)` call on the
    /// value — see the module doc comment.
    Closure(Vec<String>, std::rc::Rc<Vec<Stmt>>),
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
            // Never meaningfully compared in a valid script — a closure
            // reference is truthy, matching "an object exists" semantics.
            PainlessValue::Closure(..) => true,
        }
    }
    pub fn from_json(v: &Value) -> Self {
        match v {
            Value::Null => PainlessValue::Null,
            Value::Bool(b) => PainlessValue::Bool(*b),
            Value::Number(n) => PainlessValue::Number(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => PainlessValue::String(s.clone()),
            Value::Array(arr) => {
                PainlessValue::Array(arr.iter().map(PainlessValue::from_json).collect())
            }
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
    /// Current expression-evaluation recursion depth. Statement nesting is
    /// independently bounded by the parser, so it must not consume the exact
    /// [`MAX_EVAL_DEPTH`] expression budget.
    eval_depth: std::cell::Cell<usize>,
    /// Current closure call-nesting depth (`call_closure` re-entering
    /// `exec_stmt`/`eval_expr`). Bounded independently of `eval_depth`:
    /// a closure's own statement nesting isn't charged against the
    /// expression-eval budget at all (only the *call* that invoked it is),
    /// so self-application recursion (`f(f, n)`) could otherwise re-enter
    /// `exec_stmt` far past any native stack the eval-depth budget assumes.
    call_depth: std::cell::Cell<usize>,
    /// Total closure invocations across the whole script evaluation. Call
    /// *depth* alone doesn't bound an exponential call *tree* — a script
    /// like `g(g,n-1) + g(g,n-1) + g(g,n-1) + g(g,n-1)` never exceeds a
    /// depth of ~n, but its invocation count is 4^n. This is the step
    /// budget that catches that shape.
    call_count: std::cell::Cell<usize>,
}

impl<'a> PainlessCtx<'a> {
    pub fn new(doc: &'a Value, params: &'a Value, score: f32) -> Self {
        Self {
            doc,
            params,
            score,
            emits: std::cell::RefCell::new(Vec::new()),
            eval_depth: std::cell::Cell::new(0),
            call_depth: std::cell::Cell::new(0),
            call_count: std::cell::Cell::new(0),
        }
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
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Comments
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
            while i < bytes.len() && bytes[i] as char != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] as char == '*' && bytes[i + 1] as char == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // Number literal
        if c.is_ascii_digit()
            || (c == '.' && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_ascii_digit()
                    || cc == '.'
                    || cc == 'e'
                    || cc == 'E'
                    || cc == '-'
                    || cc == '+'
                {
                    // Allow signed exponent
                    if (cc == '-' || cc == '+') && !matches!(bytes[i - 1] as char, 'e' | 'E') {
                        break;
                    }
                    i += 1;
                } else {
                    break;
                }
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
            s_clean = s_clean
                .trim_end_matches(['L', 'l', 'F', 'f', 'D', 'd'])
                .to_string();
            let n: f64 = s_clean
                .parse()
                .map_err(|e| format!("bad number {s_clean}: {e}"))?;
            out.push(Tok::Number(n));
            continue;
        }
        // String literal
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] as char != quote {
                if bytes[i] as char == '\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i >= bytes.len() {
                return Err("unterminated string".into());
            }
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
                if cc.is_alphanumeric() || cc == '_' || cc == '$' {
                    i += 1;
                } else {
                    break;
                }
            }
            let s = &src[start..i];
            match s {
                "if" | "else" | "return" | "true" | "false" | "null" | "double" | "int"
                | "long" | "float" | "boolean" | "String" | "def" | "var" | "for" | "while"
                | "break" | "continue" | "new" | "instanceof" => {
                    out.push(Tok::Keyword(s.to_string()))
                }
                _ => out.push(Tok::Ident(s.to_string())),
            }
            continue;
        }
        // Multi-char punctuation
        if i + 1 < bytes.len() {
            let two: String = format!("{}{}", c, bytes[i + 1] as char);
            if matches!(
                two.as_str(),
                "==" | "!="
                    | "<="
                    | ">="
                    | "&&"
                    | "||"
                    | "->"
                    | "+="
                    | "-="
                    | "*="
                    | "/="
                    | "%="
                    | "++"
                    | "--"
            ) {
                out.push(Tok::PunctMulti(two));
                i += 2;
                continue;
            }
        }
        // Single-char punctuation
        if matches!(
            c,
            '(' | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | ','
                | ';'
                | '.'
                | ':'
                | '?'
                | '+'
                | '-'
                | '*'
                | '/'
                | '%'
                | '<'
                | '>'
                | '='
                | '!'
                | '&'
                | '|'
        ) {
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
pub enum Expr {
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
    /// `(params) -> expr` / `(params) -> { stmts }` — a closure literal.
    /// The body is `Rc`-shared (not owned `Vec<Stmt>`) so producing the
    /// `PainlessValue::Closure` this evaluates to — which happens on every
    /// evaluation of the literal, e.g. once per call when a closure is
    /// itself an argument passed to a recursive call — is a cheap refcount
    /// bump rather than a deep AST clone.
    Lambda(Vec<String>, std::rc::Rc<Vec<Stmt>>),
}

/// Parser-only expression wrapper carrying the exact AST depth in O(1).
///
/// Left-associative operator parsers build their ASTs in loops. Without this
/// metadata, a flat expression can create thousands of nested `Binary` nodes
/// while the recursive-descent parser itself remains only a few frames deep.
struct ParsedExpr {
    expr: Expr,
    depth: usize,
}

impl ParsedExpr {
    fn leaf(expr: Expr) -> Self {
        Self { expr, depth: 1 }
    }

    fn parent(expr: Expr, child_depth: usize) -> Result<Self, String> {
        let depth = child_depth.saturating_add(1);
        if depth > MAX_EVAL_DEPTH {
            return Err(EVAL_TOO_DEEP_MSG.to_string());
        }
        Ok(Self { expr, depth })
    }
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Return(Option<Expr>),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    Block(Vec<Stmt>),
    /// `<type> name(<type> param, ...) { body }` — a local function
    /// declaration. Parameter/return types are parsed and discarded (the
    /// interpreter is dynamically typed); only names and the body matter.
    /// `Rc`-shared body for the same reason as [`Expr::Lambda`].
    FnDecl(String, Vec<String>, std::rc::Rc<Vec<Stmt>>),
}

// ── Resource limits ──────────────────────────────────────────────────────────

/// Maximum recursive nesting depth accepted by the recursive-descent parser
/// (nested parens, unary chains, ternaries, nested blocks). An unauthenticated
/// caller could otherwise submit a ~3 KB script of nested parens whose parse
/// recursion overflows the (2 MB) worker-thread stack and aborts the entire
/// process. ~100 is far below the empirically-observed overflow (~3000) yet
/// comfortably above any legitimate hand-written or generated script.
pub(crate) const MAX_PARSE_DEPTH: usize = 100;

/// Maximum recursive AST-evaluation depth. Bounds `eval_expr`/`exec_stmt`
/// recursion so a deep AST (including flat left-associative operator chains
/// like `1+1+1+…` which the parser builds with a loop, not recursion, and so
/// are NOT limited by [`MAX_PARSE_DEPTH`]) cannot overflow the stack at score
/// time.
pub(crate) const MAX_EVAL_DEPTH: usize = 500;

/// Maximum accepted script source length in bytes. Matches Elasticsearch's
/// default `script.max_size_in_bytes` (64 KiB) and bounds the size of any AST
/// we build (and later drop) from a single request.
pub(crate) const MAX_SCRIPT_LEN: usize = 64 * 1024;

/// Safety ceiling on any single string value produced during evaluation.
/// String concatenation (`+`) is the only operation that can grow a value
/// beyond the size of its parts, so a handful of flat (non-nested, non-deep)
/// `s = s + s;` statements — none of which trip [`MAX_EVAL_DEPTH`] or
/// [`MAX_PARSE_DEPTH`], since they're neither deeply nested nor deeply
/// recursive — doubles a string exponentially per statement. Without this
/// cap a script well under [`MAX_SCRIPT_LEN`] can exhaust available memory
/// before any other limit fires.
const MAX_PAINLESS_STRING_LEN: usize = 1024 * 1024;

/// Sentinel error returned when [`MAX_PARSE_DEPTH`] is exceeded. Callers
/// (`check_script_limits`) match on it to distinguish "too complex" (a 400)
/// from ordinary syntax errors (which degrade gracefully at runtime).
pub(crate) const TOO_DEEP_MSG: &str = "compile error: script exceeds maximum nesting depth";

/// Sentinel returned before constructing an AST that the recursive evaluator
/// cannot safely evaluate within [`MAX_EVAL_DEPTH`].
pub(crate) const EVAL_TOO_DEEP_MSG: &str =
    "script evaluation exceeded maximum depth; split the expression into smaller statements";

/// Maximum closure (local function / lambda) call-nesting depth.
///
/// Closure invocation re-enters `exec_stmt`/`eval_expr` from inside
/// `eval_expr` itself (`call_closure` → `exec_body` → `exec_stmt`), and a
/// closure body's own statement nesting is bounded by [`MAX_PARSE_DEPTH`]
/// but NOT charged against [`MAX_EVAL_DEPTH`] at all — only the *call*
/// expression that invoked the closure is. So self-application recursion
/// (`def f = (g, n) -> g(g, n - 1); return f(f, 9);`) could otherwise
/// re-enter `exec_stmt` up to `MAX_EVAL_DEPTH * MAX_PARSE_DEPTH` times
/// (~50,000) before `eval_depth` ever objects, which is enough native
/// stack frames to abort the process even in a release build. Kept small
/// and independent of the expression budget.
pub(crate) const MAX_CALL_DEPTH: usize = 32;

/// Maximum total closure invocations across one script evaluation.
///
/// Call *depth* alone doesn't bound an exponential call *tree*: a script
/// like `g(g,n-1) + g(g,n-1) + g(g,n-1) + g(g,n-1)` never exceeds a call
/// depth of ~n (well under [`MAX_CALL_DEPTH`]), but its total invocation
/// count is 4^n — measured at 262,144 invocations for n=9 in ~0.8s, with
/// each unit of `n` multiplying the cost by 4. This is the step budget
/// that bounds the call tree's total size rather than its depth.
pub(crate) const MAX_CALL_COUNT: usize = 10_000;

/// Sentinel returned when [`MAX_CALL_DEPTH`] or [`MAX_CALL_COUNT`] is
/// exceeded.
pub(crate) const TOO_MANY_CALLS_MSG: &str =
    "script exceeded the maximum closure call depth or invocation count";

// ── Parser ───────────────────────────────────────────────────────────────────

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    /// Current recursive-descent nesting depth (guards against stack overflow
    /// on adversarial deeply-nested input).
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Self {
            toks,
            pos: 0,
            depth: 0,
        }
    }
    /// Enter one recursion level, failing (instead of overflowing the stack)
    /// once [`MAX_PARSE_DEPTH`] is exceeded. Paired with [`Parser::ascend`] on
    /// the success path; on the error path the whole parse is abandoned so the
    /// unbalanced counter is irrelevant.
    fn descend(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            Err(TOO_DEEP_MSG.to_string())
        } else {
            Ok(())
        }
    }
    fn ascend(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn eat(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn expect_punct(&mut self, c: char) -> Result<(), String> {
        match self.eat() {
            Some(Tok::Punct(p)) if p == c => Ok(()),
            other => Err(format!("expected '{}' got {:?}", c, other)),
        }
    }
    fn match_punct(&mut self, c: char) -> bool {
        if let Some(Tok::Punct(p)) = self.peek() {
            if *p == c {
                self.pos += 1;
                return true;
            }
        }
        false
    }
    fn match_keyword(&mut self, kw: &str) -> bool {
        if let Some(Tok::Keyword(s)) = self.peek() {
            if s == kw {
                self.pos += 1;
                return true;
            }
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
        // Depth-guard statement nesting (blocks / if / for bodies).
        self.descend()?;
        let out = self.parse_stmt_inner();
        self.ascend();
        out
    }
    fn parse_stmt_inner(&mut self) -> Result<Stmt, String> {
        // `if (...) { ... } else { ... }`
        if self.match_keyword("if") {
            self.expect_punct('(')?;
            let cond = self.parse_expr()?;
            self.expect_punct(')')?;
            let then_body = self.parse_block_or_stmt()?;
            let else_body = if self.match_keyword("else") {
                self.parse_block_or_stmt()?
            } else {
                Vec::new()
            };
            return Ok(Stmt::If(cond.expr, then_body, else_body));
        }
        if self.match_keyword("return") {
            // Optional expression then ;
            let e = if self.match_punct(';') {
                None
            } else {
                let e = self.parse_expr()?;
                let _ = self.match_punct(';');
                Some(e.expr)
            };
            return Ok(Stmt::Return(e));
        }
        if let Some(Tok::Punct('{')) = self.peek() {
            let block = self.parse_block_or_stmt()?;
            return Ok(Stmt::Block(block));
        }
        // Variable decl: `<type> NAME = expr;`
        if let Some(Tok::Keyword(kw)) = self.peek().cloned() {
            if matches!(
                kw.as_str(),
                "double" | "int" | "long" | "float" | "boolean" | "String" | "def" | "var"
            ) {
                self.pos += 1;
                let name = match self.eat() {
                    Some(Tok::Ident(n)) => n,
                    other => return Err(format!("expected identifier after type got {:?}", other)),
                };
                if self.match_punct('(') {
                    let params = self.parse_fn_params()?;
                    let body = self.parse_block_or_stmt()?;
                    return Ok(Stmt::FnDecl(name, params, std::rc::Rc::new(body)));
                }
                if !self.match_punct('=') {
                    return Err(format!("expected '=' after var name '{}'", name));
                }
                let val = self.parse_expr()?;
                let _ = self.match_punct(';');
                let depth = val.depth;
                let expr = ParsedExpr::parent(Expr::Assign(name, Box::new(val.expr), true), depth)?;
                return Ok(Stmt::Expr(expr.expr));
            }
        }
        let e = self.parse_expr()?;
        let _ = self.match_punct(';');
        Ok(Stmt::Expr(e.expr))
    }
    fn parse_block_or_stmt(&mut self) -> Result<Vec<Stmt>, String> {
        if self.match_punct('{') {
            let mut out = Vec::new();
            while let Some(t) = self.peek() {
                if matches!(t, Tok::Punct('}')) {
                    break;
                }
                out.push(self.parse_stmt()?);
            }
            self.expect_punct('}')?;
            Ok(out)
        } else {
            Ok(vec![self.parse_stmt()?])
        }
    }
    fn parse_expr(&mut self) -> Result<ParsedExpr, String> {
        self.parse_assign()
    }
    fn parse_assign(&mut self) -> Result<ParsedExpr, String> {
        // Every expression re-entry (parens, index keys, call args, ternary
        // arms, assignment RHS) funnels through parse_assign, so guarding it
        // here bounds the whole expression-grammar recursion by nesting depth.
        self.descend()?;
        let out = self.parse_assign_inner();
        self.ascend();
        out
    }
    fn parse_assign_inner(&mut self) -> Result<ParsedExpr, String> {
        let lhs = self.parse_ternary()?;
        if self.match_punct('=') {
            // Disambiguate from `==` already consumed by parse_compare.
            let rhs = self.parse_assign()?;
            if let Expr::Ident(name) = lhs.expr {
                let depth = rhs.depth;
                return ParsedExpr::parent(Expr::Assign(name, Box::new(rhs.expr), false), depth);
            }
            return Err("assignment target must be identifier".into());
        }
        Ok(lhs)
    }
    fn parse_ternary(&mut self) -> Result<ParsedExpr, String> {
        let cond = self.parse_or()?;
        if self.match_punct('?') {
            let then_e = self.parse_assign()?;
            self.expect_punct(':')?;
            let else_e = self.parse_assign()?;
            let depth = cond.depth.max(then_e.depth).max(else_e.depth);
            return ParsedExpr::parent(
                Expr::Ternary(
                    Box::new(cond.expr),
                    Box::new(then_e.expr),
                    Box::new(else_e.expr),
                ),
                depth,
            );
        }
        Ok(cond)
    }
    fn parse_or(&mut self) -> Result<ParsedExpr, String> {
        let mut lhs = self.parse_and()?;
        while let Some(Tok::PunctMulti(op)) = self.peek() {
            if op == "||" {
                self.pos += 1;
                let rhs = self.parse_and()?;
                let depth = lhs.depth.max(rhs.depth);
                lhs = ParsedExpr::parent(
                    Expr::Binary("||".into(), Box::new(lhs.expr), Box::new(rhs.expr)),
                    depth,
                )?;
            } else {
                break;
            }
        }
        Ok(lhs)
    }
    fn parse_and(&mut self) -> Result<ParsedExpr, String> {
        let mut lhs = self.parse_eq()?;
        while let Some(Tok::PunctMulti(op)) = self.peek() {
            if op == "&&" {
                self.pos += 1;
                let rhs = self.parse_eq()?;
                let depth = lhs.depth.max(rhs.depth);
                lhs = ParsedExpr::parent(
                    Expr::Binary("&&".into(), Box::new(lhs.expr), Box::new(rhs.expr)),
                    depth,
                )?;
            } else {
                break;
            }
        }
        Ok(lhs)
    }
    fn parse_eq(&mut self) -> Result<ParsedExpr, String> {
        let mut lhs = self.parse_compare()?;
        while let Some(t) = self.peek() {
            let op = match t {
                Tok::PunctMulti(s) if s == "==" || s == "!=" => s.clone(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_compare()?;
            let depth = lhs.depth.max(rhs.depth);
            lhs = ParsedExpr::parent(
                Expr::Binary(op, Box::new(lhs.expr), Box::new(rhs.expr)),
                depth,
            )?;
        }
        Ok(lhs)
    }
    fn parse_compare(&mut self) -> Result<ParsedExpr, String> {
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
            let depth = lhs.depth.max(rhs.depth);
            lhs = ParsedExpr::parent(
                Expr::Binary(op, Box::new(lhs.expr), Box::new(rhs.expr)),
                depth,
            )?;
        }
        Ok(lhs)
    }
    fn parse_add(&mut self) -> Result<ParsedExpr, String> {
        let mut lhs = self.parse_mul()?;
        while let Some(t) = self.peek() {
            let op = match t {
                Tok::Punct('+') => "+".to_string(),
                Tok::Punct('-') => "-".to_string(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_mul()?;
            let depth = lhs.depth.max(rhs.depth);
            lhs = ParsedExpr::parent(
                Expr::Binary(op, Box::new(lhs.expr), Box::new(rhs.expr)),
                depth,
            )?;
        }
        Ok(lhs)
    }
    fn parse_mul(&mut self) -> Result<ParsedExpr, String> {
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
            let depth = lhs.depth.max(rhs.depth);
            lhs = ParsedExpr::parent(
                Expr::Binary(op, Box::new(lhs.expr), Box::new(rhs.expr)),
                depth,
            )?;
        }
        Ok(lhs)
    }
    fn parse_unary(&mut self) -> Result<ParsedExpr, String> {
        // Guard unary chains (`----x`, `!!!x`), which recurse through
        // parse_unary itself and so bypass the parse_assign guard.
        self.descend()?;
        let out = self.parse_unary_inner();
        self.ascend();
        out
    }
    fn parse_unary_inner(&mut self) -> Result<ParsedExpr, String> {
        if self.match_punct('-') {
            let e = self.parse_unary()?;
            let depth = e.depth;
            return ParsedExpr::parent(Expr::Unary("-".into(), Box::new(e.expr)), depth);
        }
        if self.match_punct('!') {
            let e = self.parse_unary()?;
            let depth = e.depth;
            return ParsedExpr::parent(Expr::Unary("!".into(), Box::new(e.expr)), depth);
        }
        if self.match_punct('+') {
            return self.parse_unary();
        }
        self.parse_postfix()
    }
    fn parse_postfix(&mut self) -> Result<ParsedExpr, String> {
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
                    let args_depth = args.iter().map(|arg| arg.depth).max().unwrap_or(0);
                    let depth = e.depth.max(args_depth);
                    e = ParsedExpr::parent(
                        Expr::Member(
                            Box::new(e.expr),
                            name,
                            Some(args.into_iter().map(|arg| arg.expr).collect()),
                        ),
                        depth,
                    )?;
                } else {
                    let depth = e.depth;
                    e = ParsedExpr::parent(Expr::Member(Box::new(e.expr), name, None), depth)?;
                }
            } else if self.match_punct('[') {
                let idx = self.parse_expr()?;
                self.expect_punct(']')?;
                let depth = e.depth.max(idx.depth);
                e = ParsedExpr::parent(Expr::Index(Box::new(e.expr), Box::new(idx.expr)), depth)?;
            } else {
                break;
            }
        }
        Ok(e)
    }
    fn parse_args(&mut self, end: char) -> Result<Vec<ParsedExpr>, String> {
        let mut out: Vec<ParsedExpr> = Vec::new();
        if let Some(Tok::Punct(c)) = self.peek() {
            if *c == end {
                self.pos += 1;
                return Ok(out);
            }
        }
        loop {
            out.push(self.parse_expr()?);
            if self.match_punct(',') {
                continue;
            }
            break;
        }
        match self.eat() {
            Some(Tok::Punct(c)) if c == end => Ok(out),
            other => Err(format!("expected '{}' got {:?}", end, other)),
        }
    }
    /// Parameter list for a local function declaration: `(<type> name, ...)`,
    /// with the opening `(` already consumed. Types are accepted (any
    /// Ident or Keyword token) and discarded — only names are kept.
    fn parse_fn_params(&mut self) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        if let Some(Tok::Punct(')')) = self.peek() {
            self.pos += 1;
            return Ok(names);
        }
        loop {
            match self.eat() {
                Some(Tok::Ident(_)) | Some(Tok::Keyword(_)) => {}
                other => return Err(format!("expected parameter type, got {:?}", other)),
            }
            let name = match self.eat() {
                Some(Tok::Ident(n)) => n,
                other => return Err(format!("expected parameter name, got {:?}", other)),
            };
            names.push(name);
            if self.match_punct(',') {
                continue;
            }
            break;
        }
        self.expect_punct(')')?;
        Ok(names)
    }
    /// Try to parse a lambda literal `(a, b, ...) -> expr` / `-> { stmts }`
    /// starting at the current position (which must be at `(`). Backtracks
    /// (restores `self.pos`) and returns `None` on any mismatch, so the
    /// caller falls back to treating `(` as a parenthesized sub-expression —
    /// the two forms share the same opening token and are only
    /// distinguishable by what follows the matching `)`.
    ///
    /// The lambda's own body is depth-guarded independently (its statements
    /// go through the normal `parse_stmt`/descend-ascend path), so the
    /// resulting literal is a leaf from the enclosing expression's depth
    /// budget — it does not consume any of the caller's `ParsedExpr` depth.
    fn try_parse_lambda(&mut self) -> Result<Option<ParsedExpr>, String> {
        let save = self.pos;
        self.pos += 1; // consume '('
        let mut params = Vec::new();
        if let Some(Tok::Punct(')')) = self.peek() {
            self.pos += 1;
        } else {
            loop {
                match self.peek().cloned() {
                    Some(Tok::Ident(n)) => {
                        self.pos += 1;
                        params.push(n);
                    }
                    _ => {
                        self.pos = save;
                        return Ok(None);
                    }
                }
                if let Some(Tok::Punct(',')) = self.peek() {
                    self.pos += 1;
                    continue;
                }
                break;
            }
            if !matches!(self.peek(), Some(Tok::Punct(')'))) {
                self.pos = save;
                return Ok(None);
            }
            self.pos += 1; // consume ')'
        }
        match self.peek() {
            Some(Tok::PunctMulti(s)) if s == "->" => {
                self.pos += 1;
            }
            _ => {
                self.pos = save;
                return Ok(None);
            }
        }
        // Once `->` is consumed, this MUST be a lambda — there is no other
        // valid parse of `(params) ->` in this grammar. So unlike the
        // mismatches above, any error from here (including the parse-depth
        // sentinel) is a genuine error to propagate, not a reason to
        // backtrack and let the caller misparse `(` as a parenthesized
        // expression instead — silently swallowing the depth-limit
        // sentinel here would defeat `check_script_limits`'s up-front 400
        // for a script whose lambda body is what makes it too deep.
        let body = self.parse_block_or_stmt()?;
        Ok(Some(ParsedExpr::leaf(Expr::Lambda(
            params,
            std::rc::Rc::new(body),
        ))))
    }
    fn parse_primary(&mut self) -> Result<ParsedExpr, String> {
        if matches!(self.peek(), Some(Tok::Punct('('))) {
            if let Some(lambda) = self.try_parse_lambda()? {
                return Ok(lambda);
            }
        }
        match self.eat() {
            Some(Tok::Number(n)) => Ok(ParsedExpr::leaf(Expr::Number(n))),
            Some(Tok::String(s)) => Ok(ParsedExpr::leaf(Expr::String(s))),
            Some(Tok::Keyword(k)) => match k.as_str() {
                "true" => Ok(ParsedExpr::leaf(Expr::Bool(true))),
                "false" => Ok(ParsedExpr::leaf(Expr::Bool(false))),
                "null" => Ok(ParsedExpr::leaf(Expr::Null)),
                other => Err(format!("unexpected keyword {} in expression", other)),
            },
            Some(Tok::Ident(name)) => {
                if self.match_punct('(') {
                    let args = self.parse_args(')')?;
                    let depth = args.iter().map(|arg| arg.depth).max().unwrap_or(0);
                    ParsedExpr::parent(
                        Expr::Call(name, args.into_iter().map(|arg| arg.expr).collect()),
                        depth,
                    )
                } else {
                    Ok(ParsedExpr::leaf(Expr::Ident(name)))
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

/// RAII guard that bounds AST-evaluation recursion depth. Incremented on
/// entry to `eval_expr`/`exec_stmt`, decremented on scope exit (including the
/// `?` early-return paths), so a pathological AST returns an error instead of
/// overflowing the worker-thread stack.
struct EvalDepthGuard<'a>(&'a std::cell::Cell<usize>);
impl<'a> EvalDepthGuard<'a> {
    fn enter(cell: &'a std::cell::Cell<usize>) -> Result<Self, String> {
        let d = cell.get();
        if d >= MAX_EVAL_DEPTH {
            return Err(EVAL_TOO_DEEP_MSG.to_string());
        }
        cell.set(d + 1);
        Ok(EvalDepthGuard(cell))
    }
}
impl Drop for EvalDepthGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// RAII guard bounding closure call nesting AND total invocation count,
/// independent of [`EvalDepthGuard`] — see [`MAX_CALL_DEPTH`] and
/// [`MAX_CALL_COUNT`] for why expression-eval depth alone doesn't cover
/// either the call-tree depth or its total size.
struct CallGuard<'a>(&'a std::cell::Cell<usize>);
impl<'a> CallGuard<'a> {
    fn enter(ctx: &'a PainlessCtx) -> Result<Self, String> {
        let depth = ctx.call_depth.get();
        if depth >= MAX_CALL_DEPTH {
            return Err(TOO_MANY_CALLS_MSG.to_string());
        }
        let count = ctx.call_count.get();
        if count >= MAX_CALL_COUNT {
            return Err(TOO_MANY_CALLS_MSG.to_string());
        }
        ctx.call_depth.set(depth + 1);
        ctx.call_count.set(count + 1);
        Ok(CallGuard(&ctx.call_depth))
    }
}
impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

// ── Compiled-script cache ────────────────────────────────────────────────────
//
// A script is evaluated once per *document*. Tokenizing and parsing it per
// document made the per-doc cost scale with the script's SIZE rather than
// with its complexity: a legal 64 KiB script (see [`MAX_SCRIPT_LEN`]) cost
// ~2 ms/doc, essentially all of it re-parsing an AST identical to the one
// built for the previous document. That is also what made the doc-scan's
// cooperative timeout ineffective — `scan_stored_section_into` polls its
// deadline every N docs on the assumption that per-doc work is a few
// microseconds, so a 2 ms/doc script stretched the uninterruptible quantum
// from milliseconds to seconds and a `timeout` could not cut the scan short.
//
// Scripts are pure source text, so the AST is a pure function of the source
// and can simply be memoised. The cache MUST be bounded: the key is
// attacker-supplied source, so an unbounded map is itself a memory-exhaustion
// vector — a caller need only issue queries with unique scripts.

/// Maximum number of distinct compiled scripts retained at once.
const MAX_SCRIPT_CACHE_ENTRIES: usize = 128;

/// Maximum total *source* bytes retained across cached entries. Bounds the
/// cache independently of the entry count, since [`MAX_SCRIPT_LEN`] allows a
/// single 64 KiB script and 128 of those would be 8 MiB of source (and a
/// considerably larger multiple of that in AST nodes).
const MAX_SCRIPT_CACHE_SRC_BYTES: usize = 512 * 1024;

/// A parse result shared between evaluations. Parse *failures* are cached
/// too — a syntactically invalid script is also evaluated once per document,
/// so leaving errors uncached would leave the same per-doc parse cost (and
/// the same unbounded quantum) reachable through a malformed script.
///
/// `Rc`, not `Arc`, because [`Stmt::FnDecl`]/[`Expr::Lambda`] bodies are
/// themselves `Rc`-shared — deliberately non-atomic, since a closure literal
/// is cloned on every invocation. The cache is therefore per-thread (see
/// [`SCRIPT_CACHE`]) rather than a shared static.
type CompiledScript = Rc<Result<Vec<Stmt>, String>>;

#[derive(Default)]
struct ScriptCache {
    entries: HashMap<String, CompiledScript>,
    /// Sum of `entries`' key lengths; maintained incrementally.
    src_bytes: usize,
}

impl ScriptCache {
    fn insert(&mut self, src: &str, compiled: CompiledScript) {
        // Never cacheable on its own — would evict everything and still not fit.
        if src.len() > MAX_SCRIPT_CACHE_SRC_BYTES || self.entries.contains_key(src) {
            return;
        }
        // Evict (in arbitrary hash order — the workload is "one script for a
        // whole query", so recency bookkeeping buys nothing worth its cost)
        // until the newcomer fits under BOTH bounds.
        while self.entries.len() >= MAX_SCRIPT_CACHE_ENTRIES
            || self.src_bytes.saturating_add(src.len()) > MAX_SCRIPT_CACHE_SRC_BYTES
        {
            let Some(victim) = self.entries.keys().next().cloned() else {
                // Empty and still over budget is impossible given the guard
                // above, but never spin.
                return;
            };
            self.entries.remove(&victim);
            self.src_bytes = self.src_bytes.saturating_sub(victim.len());
        }
        self.src_bytes = self.src_bytes.saturating_add(src.len());
        self.entries.insert(src.to_string(), compiled);
    }
}

thread_local! {
    /// One cache per thread. A compiled AST contains `Rc`s and so cannot
    /// cross threads; a `Mutex<Arc<...>>` would not help, since the `Rc`s
    /// *inside* the AST are the non-`Sync` part.
    ///
    /// The bound is therefore per-thread, and the total is bounded by
    /// (search worker threads) × [`MAX_SCRIPT_CACHE_SRC_BYTES`] — still a
    /// fixed ceiling, because the worker pool is sized from the core count,
    /// not from the request rate. Per-thread caching also removes the lock
    /// from a path taken once per document, and each worker scanning the
    /// same segment converges on the same one entry.
    static SCRIPT_CACHE: RefCell<ScriptCache> = RefCell::new(ScriptCache::default());
}

fn compile_script(src: &str) -> Result<Vec<Stmt>, String> {
    let toks = tokenize(src)?;
    let mut p = Parser::new(&toks);
    p.parse_program()
}

/// Tokenize + parse `src`, reusing a previously compiled AST when possible.
fn compile_script_cached(src: &str) -> CompiledScript {
    SCRIPT_CACHE.with(|cache| {
        // Borrow, look up, release — the insert below needs a fresh mutable
        // borrow, and `compile_script` must not run while either is held.
        let hit = cache.borrow().entries.get(src).map(Rc::clone);
        if let Some(hit) = hit {
            return hit;
        }
        let compiled: CompiledScript = Rc::new(compile_script(src));
        cache.borrow_mut().insert(src, Rc::clone(&compiled));
        compiled
    })
}

/// Validate a script against the parser/length resource limits WITHOUT running
/// it, so the request layer can reject an abusive script with a 400 up front.
///
/// Returns `Err` **only** for limit violations (source too long, or nesting
/// depth beyond [`MAX_PARSE_DEPTH`] or [`MAX_EVAL_DEPTH`]). Ordinary syntax errors — including
/// constructs outside our Painless subset — return `Ok(())` so they keep
/// degrading gracefully at runtime (unchanged behavior), rather than becoming
/// spurious 400s that would break otherwise-passing requests.
pub fn check_script_limits(src: &str) -> Result<(), String> {
    if src.len() > MAX_SCRIPT_LEN {
        return Err(format!(
            "compile error: script source is {} bytes, exceeds the {}-byte limit",
            src.len(),
            MAX_SCRIPT_LEN
        ));
    }
    // Anything other than a depth violation — including tokenizer errors and
    // constructs outside our subset — is a plain syntax problem, so let the
    // runtime path handle it (don't 400). Compiled through the cache so that
    // repeated admission checks of the same script are free; whether the
    // evaluating thread reuses this entry depends on which one it is.
    match &*compile_script_cached(src) {
        Err(e) if e == TOO_DEEP_MSG || e == EVAL_TOO_DEEP_MSG => Err(e.clone()),
        _ => Ok(()),
    }
}

pub fn eval_painless(src: &str, ctx: &PainlessCtx) -> Result<PainlessValue, String> {
    if src.len() > MAX_SCRIPT_LEN {
        return Err(format!(
            "script source is {} bytes, exceeds the {}-byte limit",
            src.len(),
            MAX_SCRIPT_LEN
        ));
    }
    // Compiled once per distinct source, not once per document. The AST is
    // shared (and immutable), so execution still gets its own `env` and its
    // own `ctx` counters — the closure guards below are per-evaluation.
    let compiled = compile_script_cached(src);
    let stmts = match &*compiled {
        Ok(stmts) => stmts,
        Err(e) => return Err(e.clone()),
    };
    let mut env: HashMap<String, PainlessValue> = HashMap::new();
    exec_body(stmts, ctx, &mut env)
}

/// Run a statement list with implicit-last-value return semantics: an
/// explicit `return X;` short-circuits with `X`; otherwise the value of the
/// last executed statement is returned. Shared by the top-level script body
/// and by closure (local function / lambda) invocation.
fn exec_body(
    stmts: &[Stmt],
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<PainlessValue, String> {
    let mut last: PainlessValue = PainlessValue::Null;
    for stmt in stmts {
        match exec_stmt(stmt, ctx, env)? {
            ExecOutcome::Return(v) => return Ok(v),
            ExecOutcome::Value(v) => last = v,
        }
    }
    Ok(last)
}

/// Invoke a closure (local function or lambda) with the given positional
/// arguments, in a fresh scope seeded only with the bound parameters — no
/// access to the caller's locals, matching the target scripts' needs
/// (functional-interface bodies only ever reference `doc`/`params`/`_score`,
/// which come from `ctx`, not the enclosing `env`).
fn call_closure(
    params: &[String],
    body: &[Stmt],
    args: &[PainlessValue],
    ctx: &PainlessCtx,
) -> Result<PainlessValue, String> {
    let _guard = CallGuard::enter(ctx)?;
    if args.len() != params.len() {
        return Err(format!(
            "wrong number of arguments: expected {}, got {}",
            params.len(),
            args.len()
        ));
    }
    let mut local_env: HashMap<String, PainlessValue> = HashMap::new();
    for (p, a) in params.iter().zip(args.iter()) {
        local_env.insert(p.clone(), a.clone());
    }
    exec_body(body, ctx, &mut local_env)
}

enum ExecOutcome {
    Return(PainlessValue),
    Value(PainlessValue),
}

fn exec_stmt(
    s: &Stmt,
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<ExecOutcome, String> {
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
        Stmt::FnDecl(name, params, body) => {
            env.insert(
                name.clone(),
                PainlessValue::Closure(params.clone(), body.clone()),
            );
            Ok(ExecOutcome::Value(PainlessValue::Null))
        }
    }
}

fn eval_expr(
    e: &Expr,
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<PainlessValue, String> {
    let _guard = EvalDepthGuard::enter(&ctx.eval_depth)?;
    match e {
        Expr::Number(n) => Ok(PainlessValue::Number(*n)),
        Expr::String(s) => Ok(PainlessValue::String(s.clone())),
        Expr::Bool(b) => Ok(PainlessValue::Bool(*b)),
        Expr::Null => Ok(PainlessValue::Null),
        Expr::Ident(name) => {
            if let Some(v) = env.get(name) {
                return Ok(v.clone());
            }
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
        Expr::Lambda(params, body) => Ok(PainlessValue::Closure(params.clone(), body.clone())),
        Expr::Unary(op, x) => {
            let v = eval_expr(x, ctx, env)?;
            match op.as_str() {
                "-" => v
                    .as_f64()
                    .map(|n| PainlessValue::Number(-n))
                    .ok_or_else(|| "cannot apply unary '-' to a non-numeric value".to_string()),
                "!" => Ok(PainlessValue::Bool(!v.as_bool())),
                _ => Err(format!("bad unary {op}")),
            }
        }
        Expr::Binary(op, a, b) => eval_binary_chain(op, a, b, ctx, env),
        Expr::Ternary(c, t, f) => {
            let cv = eval_expr(c, ctx, env)?;
            if cv.as_bool() {
                eval_expr(t, ctx, env)
            } else {
                eval_expr(f, ctx, env)
            }
        }
        Expr::Index(_, _) | Expr::Member(_, _, _) => eval_access_chain(e, ctx, env),
        Expr::Call(name, args) => {
            let argvs: Vec<PainlessValue> = args
                .iter()
                .map(|a| eval_expr(a, ctx, env))
                .collect::<Result<_, _>>()?;
            // A local function (or a variable bound to a lambda) shadows
            // the builtin call table.
            if let Some(PainlessValue::Closure(params, body)) = env.get(name) {
                return call_closure(params, body, &argvs, ctx);
            }
            global_call(name, &argvs, ctx)
        }
    }
}

/// Evaluate the parser's left-associative binary spine without mirroring it on
/// the native stack. Right-hand operands still use `eval_expr`; grammar
/// recursion bounds those subtrees, while this loop handles the only
/// unbounded binary shape the parser can construct.
fn eval_binary_chain(
    root_op: &str,
    root_left: &Expr,
    root_right: &Expr,
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<PainlessValue, String> {
    let mut pending = vec![(root_op, root_right)];
    let mut left = root_left;
    while let Expr::Binary(op, next_left, right) = left {
        pending.push((op.as_str(), right.as_ref()));
        if pending.len() >= MAX_EVAL_DEPTH {
            return Err(EVAL_TOO_DEEP_MSG.to_string());
        }
        left = next_left;
    }

    let mut value = eval_expr(left, ctx, env)?;
    for (op, right) in pending.into_iter().rev() {
        if op == "&&" && !value.as_bool() {
            value = PainlessValue::Bool(false);
            continue;
        }
        if op == "||" && value.as_bool() {
            value = PainlessValue::Bool(true);
            continue;
        }
        let right = eval_expr(right, ctx, env)?;
        value = apply_binary(op, value, right)?;
    }
    Ok(value)
}

/// Evaluate a left-nested member/index chain without mirroring its postfix
/// depth on the native stack.
///
/// The parser has already enforced the exact [`MAX_EVAL_DEPTH`] contract.
/// This loop preserves the original evaluator's ordering: general indices
/// evaluate base before key, `doc`/`params` special indices evaluate their key
/// directly, and method arguments are evaluated only for the same dispatches
/// that consumed them before this reliability fix.
fn eval_access_chain(
    root: &Expr,
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<PainlessValue, String> {
    enum Step<'a> {
        Index(&'a Expr),
        Member(&'a str, &'a Option<Vec<Expr>>),
    }

    let mut steps = Vec::new();
    let mut base = root;
    loop {
        match base {
            Expr::Index(next_base, index) => {
                steps.push(Step::Index(index));
                base = next_base;
            }
            Expr::Member(next_base, member, args) => {
                steps.push(Step::Member(member, args));
                base = next_base;
            }
            _ => break,
        }
    }
    steps.reverse();

    let root_ident = match base {
        Expr::Ident(name) => Some(name.as_str()),
        _ => None,
    };
    let mut value = None;
    for (position, step) in steps.into_iter().enumerate() {
        if position == 0 {
            match (root_ident, &step) {
                (Some("doc"), Step::Index(index)) => {
                    let key = access_key(eval_expr(index, ctx, env)?)?;
                    value = Some(PainlessValue::String(format!("__docref__:{key}")));
                    continue;
                }
                (Some("params"), Step::Index(index)) => {
                    let key = access_key(eval_expr(index, ctx, env)?)?;
                    value = Some(if key == "_source" {
                        let mut source = ctx.doc.clone();
                        xerj_query::executor::strip_internal_passage_metadata(&mut source);
                        PainlessValue::from_json(&source)
                    } else {
                        PainlessValue::from_json(
                            &ctx.params.get(&key).cloned().unwrap_or(Value::Null),
                        )
                    });
                    continue;
                }
                (Some("doc"), Step::Member(member, args)) if args.is_none() => {
                    value = Some(PainlessValue::String(format!("__docref__:{member}")));
                    continue;
                }
                (Some("params"), Step::Member(member, args)) if args.is_none() => {
                    value = Some(PainlessValue::from_json(
                        &ctx.params.get(*member).cloned().unwrap_or(Value::Null),
                    ));
                    continue;
                }
                (Some("Math"), Step::Member(member, args)) => {
                    let argvs = eval_args(args, ctx, env)?;
                    value = Some(math_call(member, &argvs)?);
                    continue;
                }
                _ => {}
            }
        }

        let current = match value.take() {
            Some(value) => value,
            None => eval_expr(base, ctx, env)?,
        };
        value = Some(match step {
            Step::Index(index) => {
                let key = eval_expr(index, ctx, env)?;
                match (current, key) {
                    (PainlessValue::Array(values), PainlessValue::Number(index)) => values
                        .get(index as usize)
                        .cloned()
                        .unwrap_or(PainlessValue::Null),
                    _ => PainlessValue::Null,
                }
            }
            Step::Member(member, args) => eval_member_value(current, member, args, ctx, env)?,
        });
    }
    value.ok_or_else(|| "internal error: empty access chain".to_string())
}

fn access_key(value: PainlessValue) -> Result<String, String> {
    match value {
        PainlessValue::String(value) => Ok(value),
        PainlessValue::Number(value) => Ok(format_num(value)),
        other => Err(format!("non-string index: {other:?}")),
    }
}

fn eval_args(
    args: &Option<Vec<Expr>>,
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<Vec<PainlessValue>, String> {
    match args {
        Some(args) => args.iter().map(|arg| eval_expr(arg, ctx, env)).collect(),
        None => Ok(Vec::new()),
    }
}

fn eval_member_value(
    value: PainlessValue,
    member: &str,
    args: &Option<Vec<Expr>>,
    ctx: &PainlessCtx,
    env: &mut HashMap<String, PainlessValue>,
) -> Result<PainlessValue, String> {
    // Functional-interface call on a closure value — `s.get()`,
    // `fn.apply(x)`, `pred.test(x)`, ... The interface method name is
    // irrelevant to a dynamically-typed interpreter; only the positional
    // args matter, so any `.method(args)` call on a closure invokes it. A
    // member access with no call parens (`s.get` without `()`) doesn't
    // invoke anything real Painless functional interfaces expose, so it
    // falls through to the error below same as today.
    if let PainlessValue::Closure(params, body) = &value {
        if let Some(args) = args {
            let argvs: Vec<PainlessValue> = args
                .iter()
                .map(|a| eval_expr(a, ctx, env))
                .collect::<Result<_, _>>()?;
            return call_closure(params, body, &argvs, ctx);
        }
    }
    if let PainlessValue::String(text) = &value {
        if let Some(field) = text.strip_prefix("__docref__:") {
            return resolve_doc_member(ctx, field, member, args, env);
        }
        match member {
            "length" => return Ok(PainlessValue::Number(text.chars().count() as f64)),
            "toString" => return Ok(PainlessValue::String(text.clone())),
            "toLowerCase" => return Ok(PainlessValue::String(text.to_lowercase())),
            "toUpperCase" => return Ok(PainlessValue::String(text.to_uppercase())),
            "getHour" | "getMinute" | "getSecond" | "getDayOfMonth" | "getMonthValue"
            | "getYear" | "getDayOfWeek" | "getDayOfWeekEnum" => {
                if let Some(milliseconds) = date_value_millis(text) {
                    return date_component(milliseconds, member);
                }
            }
            // `DayOfWeek.getDisplayName(TextStyle.FULL, Locale.ROOT)` —
            // this interpreter has no enum/locale value types, so
            // `getDayOfWeekEnum()` already returns the display-ready name
            // (see date_component); the style/locale args are accepted and
            // ignored, returning that name unchanged.
            "getDisplayName" => return Ok(PainlessValue::String(text.clone())),
            _ => {}
        }
    }
    if let PainlessValue::Object(map) = &value {
        match member {
            "toString" => return Ok(PainlessValue::String(render_es_map(map))),
            "size" => return Ok(PainlessValue::Number(map.len() as f64)),
            "isEmpty" => return Ok(PainlessValue::Bool(map.is_empty())),
            _ if args.is_none() => {
                if let Some(value) = map.get(member) {
                    return Ok(PainlessValue::from_json(value));
                }
            }
            _ => {}
        }
    }
    if let PainlessValue::Array(values) = &value {
        match member {
            "size" | "length" => return Ok(PainlessValue::Number(values.len() as f64)),
            "isEmpty" => return Ok(PainlessValue::Bool(values.is_empty())),
            _ => {}
        }
    }
    Err(format!("unsupported member access .{member}"))
}

fn apply_binary(
    op: &str,
    left: PainlessValue,
    right: PainlessValue,
) -> Result<PainlessValue, String> {
    if op == "&&" {
        return Ok(PainlessValue::Bool(left.as_bool() && right.as_bool()));
    }
    if op == "||" {
        return Ok(PainlessValue::Bool(left.as_bool() || right.as_bool()));
    }

    // String concatenation for `+`.
    if op == "+"
        && (matches!(left, PainlessValue::String(_)) || matches!(right, PainlessValue::String(_)))
    {
        let render = |value: &PainlessValue| match value {
            PainlessValue::String(s) => s.clone(),
            PainlessValue::Number(n) => format_num(*n),
            PainlessValue::Bool(b) => b.to_string(),
            _ => "null".to_string(),
        };
        let left_r = render(&left);
        let right_r = render(&right);
        if left_r.len().saturating_add(right_r.len()) > MAX_PAINLESS_STRING_LEN {
            return Err(format!(
                "string concatenation result exceeds the {MAX_PAINLESS_STRING_LEN}-byte limit"
            ));
        }
        return Ok(PainlessValue::String(format!("{left_r}{right_r}")));
    }

    // ES Painless compares Strings as strings. Equality is false across
    // unlike types; relational comparisons between two strings use lexical
    // ordering in this intentionally graceful subset.
    let left_is_string = matches!(left, PainlessValue::String(_));
    let right_is_string = matches!(right, PainlessValue::String(_));
    if left_is_string || right_is_string {
        match op {
            "==" | "!=" => {
                let equal = match (&left, &right) {
                    (PainlessValue::String(x), PainlessValue::String(y)) => x == y,
                    _ => false,
                };
                return Ok(PainlessValue::Bool(if op == "==" { equal } else { !equal }));
            }
            "<" | "<=" | ">" | ">=" if left_is_string && right_is_string => {
                if let (PainlessValue::String(x), PainlessValue::String(y)) = (&left, &right) {
                    let ordering = x.cmp(y);
                    let result = match op {
                        "<" => ordering == std::cmp::Ordering::Less,
                        "<=" => ordering != std::cmp::Ordering::Greater,
                        ">" => ordering == std::cmp::Ordering::Greater,
                        _ => ordering != std::cmp::Ordering::Less,
                    };
                    return Ok(PainlessValue::Bool(result));
                }
            }
            _ => {}
        }
    }

    // Null-aware equality. `params` is a `Map<String, Object>` in real
    // Painless, so a key that wasn't supplied reads as `null` and
    // `params.y == null` is an ordinary reference comparison that yields a
    // boolean — the numeric coercion below would otherwise reject the null
    // operand and turn every params-guarded script into an error.
    //
    // Only `==`/`!=` are null-aware. Real Painless's *relational* operators
    // and arithmetic unbox their operands and throw on a null, and so do
    // ours (below) — which is what keeps a typo'd field name from matching
    // every document.
    if matches!(op, "==" | "!=")
        && (matches!(left, PainlessValue::Null) || matches!(right, PainlessValue::Null))
    {
        let equal = matches!((&left, &right), (PainlessValue::Null, PainlessValue::Null));
        return Ok(PainlessValue::Bool(if op == "==" { equal } else { !equal }));
    }

    // A value that can't coerce to a number (most commonly `Null` — a
    // missing `params` field) errors rather than silently acting as
    // 0: real Painless throws unboxing a null into a primitive, and a
    // script that quietly matched-as-zero on every doc with a typo'd field
    // name would be far worse than one that errors and excludes the doc.
    let left = left
        .as_f64()
        .ok_or_else(|| format!("cannot apply '{op}' to a non-numeric value"))?;
    let right = right
        .as_f64()
        .ok_or_else(|| format!("cannot apply '{op}' to a non-numeric value"))?;
    let value = match op {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => {
            if right == 0.0 {
                f64::NAN
            } else {
                left / right
            }
        }
        "%" => {
            if right == 0.0 {
                f64::NAN
            } else {
                left % right
            }
        }
        "<" => return Ok(PainlessValue::Bool(left < right)),
        "<=" => return Ok(PainlessValue::Bool(left <= right)),
        ">" => return Ok(PainlessValue::Bool(left > right)),
        ">=" => return Ok(PainlessValue::Bool(left >= right)),
        "==" => return Ok(PainlessValue::Bool(left == right)),
        "!=" => return Ok(PainlessValue::Bool(left != right)),
        _ => return Err(format!("bad binary {op}")),
    };
    Ok(PainlessValue::Number(value))
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
            // Return first scalar. A field that's genuinely missing (or a
            // multi-valued field with zero actual values) ERRORS — this is
            // literally what Elasticsearch does: since 7.0, an empty
            // `ScriptDocValues` throws
            //   "A document doesn't have a value for a field! Use
            //    doc[<field>].size()==0 to check if a document is missing a
            //    field!"
            // (6.x returned a type default with a deprecation warning; 7.0
            // removed that). Returning `null` here instead would be neither
            // ES's behaviour nor safe: callers (script query filter,
            // script_score, ...) treat an error as "doesn't match" /
            // "no-op score", so erroring is what keeps `doc['typo'].value > -1`
            // from matching every document.
            //
            // Guarding a field that isn't on every document therefore uses
            // ES's own documented idiom — `doc['x'].size() == 0 ? <default>
            // : doc['x'].value` — which works because `.size()` (below) is
            // defined on a missing field and the ternary only evaluates the
            // branch it takes. `params.<key> == null` still works too, since
            // `params` really is a nullable map (see `apply_binary`).
            let missing = || {
                format!(
                    "A document doesn't have a value for a field! \
                     Use doc[{field}].size()==0 to check if a document is missing a field!"
                )
            };
            match raw {
                Value::Array(arr) => arr
                    .first()
                    .map(PainlessValue::from_json)
                    .ok_or_else(missing),
                Value::Number(n) => Ok(PainlessValue::Number(n.as_f64().unwrap_or(0.0))),
                Value::String(s) => Ok(PainlessValue::String(s)),
                Value::Bool(b) => Ok(PainlessValue::Bool(b)),
                _ => Err(missing()),
            }
        }
        // `.size()` / `.length` / `.empty` are defined on a MISSING field —
        // they are how a script asks whether the field is there at all, so
        // they must never error. This is the other half of the fail-closed
        // contract on `.value` above.
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

/// Parse an ISO-8601-ish date string into epoch milliseconds (UTC),
/// reusing the same parser aggregations use for date fields so a date's
/// script-accessor components (getHour, getDayOfWeek, ...) agree with
/// how the same value is bucketed elsewhere.
fn date_value_millis(s: &str) -> Option<i64> {
    crate::aggs::parse_date_ms(&Value::String(s.to_string()))
}

/// Extract one Java `ZonedDateTime`-style component (UTC) from an epoch-ms
/// value, for the `doc['a_date_field'].value.getXxx()` accessor family.
fn date_component(ms: i64, member: &str) -> Result<PainlessValue, String> {
    use chrono::{Datelike, Timelike};
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .ok_or_else(|| format!("invalid date value for .{}", member))?;
    // `getDayOfWeekEnum()` returns Java's `DayOfWeek` enum, not a number —
    // handled separately since this interpreter has no enum value type.
    // Callers pair it with `.getDisplayName(TextStyle.FULL, Locale.ROOT)`
    // (e.g. OpenSearch Dashboards' own UBI sample index-pattern scripted
    // fields); rather than modeling `TextStyle`/`Locale` as real values,
    // this returns the full English name directly and `getDisplayName`
    // (a passthrough on any string, see eval_member_value) returns it
    // unchanged, matching that one real call shape without pretending to
    // support arbitrary locales/styles.
    if member == "getDayOfWeekEnum" {
        // Title-cased to match `getDisplayName(TextStyle.FULL, Locale.ROOT)`
        // directly, the one real call shape this exists for — not Java's
        // own `DayOfWeek.toString()`, which is upper-cased.
        let name = match dt.weekday() {
            chrono::Weekday::Mon => "Monday",
            chrono::Weekday::Tue => "Tuesday",
            chrono::Weekday::Wed => "Wednesday",
            chrono::Weekday::Thu => "Thursday",
            chrono::Weekday::Fri => "Friday",
            chrono::Weekday::Sat => "Saturday",
            chrono::Weekday::Sun => "Sunday",
        };
        return Ok(PainlessValue::String(name.to_string()));
    }
    let n = match member {
        "getHour" => dt.hour(),
        "getMinute" => dt.minute(),
        "getSecond" => dt.second(),
        "getDayOfMonth" => dt.day(),
        "getMonthValue" => dt.month(),
        "getYear" => dt.year() as u32,
        // Java DayOfWeek: MONDAY=1 .. SUNDAY=7 (chrono's weekday() already
        // uses the same Monday-first ordinal via num_days_from_monday).
        "getDayOfWeek" => dt.weekday().num_days_from_monday() + 1,
        _ => return Err(format!("unsupported date member .{}", member)),
    };
    Ok(PainlessValue::Number(n as f64))
}

fn get_doc_value(doc: &Value, field: &str) -> Value {
    if field.starts_with(xerj_query::executor::PASSAGE_METADATA_PREFIX) {
        return Value::Null;
    }
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
                            } else {
                                sub = Value::Null;
                                break;
                            }
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

fn global_call(
    name: &str,
    args: &[PainlessValue],
    ctx: &PainlessCtx,
) -> Result<PainlessValue, String> {
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
                PainlessValue::Array(arr) => {
                    arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
                }
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
                PainlessValue::Array(arr) => {
                    arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
                }
                _ => return Err("dotProduct arg 1 must be field name or array".into()),
            };
            if query.len() != doc_vec.len() {
                return Err(format!(
                    "dim mismatch: {} vs {}",
                    query.len(),
                    doc_vec.len()
                ));
            }
            let dot: f64 = query.iter().zip(doc_vec.iter()).map(|(a, b)| a * b).sum();
            Ok(PainlessValue::Number(dot))
        }
        "cosineSimilarity" => {
            if args.len() != 2 {
                return Err("cosineSimilarity expects 2 args".into());
            }
            let q: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => {
                    arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
                }
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
                PainlessValue::Array(arr) => {
                    arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
                }
                _ => return Err("cosineSimilarity arg 1 must be field name".into()),
            };
            if q.len() != d.len() {
                return Err("dim mismatch".into());
            }
            let dot: f64 = q.iter().zip(&d).map(|(a, b)| a * b).sum();
            let nq: f64 = q.iter().map(|v| v * v).sum::<f64>().sqrt();
            let nd: f64 = d.iter().map(|v| v * v).sum::<f64>().sqrt();
            let denom = nq * nd;
            Ok(PainlessValue::Number(if denom > 0.0 {
                dot / denom
            } else {
                0.0
            }))
        }
        "l1norm" | "l1Norm" => {
            if args.len() != 2 {
                return Err("l1norm expects 2 args".into());
            }
            let q: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => {
                    arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
                }
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
            if args.len() != 2 {
                return Err("l2norm expects 2 args".into());
            }
            let q: Vec<f64> = match &args[0] {
                PainlessValue::Array(arr) => {
                    arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
                }
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
            let s: f64 = q
                .iter()
                .zip(&d)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>()
                .sqrt();
            Ok(PainlessValue::Number(s))
        }
        "sigmoid" => {
            if args.len() != 1 {
                return Err("sigmoid expects 1 arg".into());
            }
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
        let v = eval_painless(
            "doc['num_likes'].value * params.multiplier",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert!((v.as_f64().unwrap() - 1500.0).abs() < 1e-9);
    }

    // ── Day-of-week display name ────────────────────────────────────────────
    // Regression coverage for a real gap found live: OpenSearch Dashboards'
    // own UBI sample index pattern defines a scripted field using
    // `doc['timestamp'].value.getDayOfWeekEnum().getDisplayName(TextStyle.FULL,
    // Locale.ROOT)` — only the numeric `getDayOfWeek()` accessor was
    // supported, so this whole scripted field silently failed to evaluate
    // (dropped from `fields`, not even a visible error) and its dashboard
    // panel rendered empty.

    #[test]
    fn day_of_week_enum_display_name_returns_full_english_name() {
        let doc = json!({"timestamp": "2024-12-09T00:00:00.000Z"}); // a Monday
        let params = json!({});
        let v = eval_painless(
            "doc['timestamp'].value.getDayOfWeekEnum().getDisplayName(TextStyle.FULL, Locale.ROOT)",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        match v {
            PainlessValue::String(s) => assert_eq!(s, "Monday"),
            other => panic!("expected a string, got {:?}", other),
        }
    }

    #[test]
    fn day_of_week_enum_display_name_covers_the_week() {
        let params = json!({});
        let expected = [
            ("2024-12-09T00:00:00.000Z", "Monday"),
            ("2024-12-10T00:00:00.000Z", "Tuesday"),
            ("2024-12-11T00:00:00.000Z", "Wednesday"),
            ("2024-12-12T00:00:00.000Z", "Thursday"),
            ("2024-12-13T00:00:00.000Z", "Friday"),
            ("2024-12-14T00:00:00.000Z", "Saturday"),
            ("2024-12-15T00:00:00.000Z", "Sunday"),
        ];
        for (ts, day_name) in expected {
            let doc = json!({"timestamp": ts});
            let v = eval_painless(
                "doc['timestamp'].value.getDayOfWeekEnum().getDisplayName(TextStyle.FULL, Locale.ROOT)",
                &ctx(&doc, &params, 0.0),
            )
            .unwrap();
            match v {
                PainlessValue::String(s) => assert_eq!(s, day_name, "for {ts}"),
                other => panic!("expected a string for {ts}, got {:?}", other),
            }
        }
    }

    #[test]
    fn passage_metadata_is_hidden_from_doc_and_source_scripts() {
        let doc = json!({
            "content": "visible",
            "__xerj_passage_meta__embedding": {"field": "content", "chunks": [[0, 7]]}
        });
        let params = json!({});
        let size = eval_painless(
            "doc['__xerj_passage_meta__embedding'].size()",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert_eq!(size.as_f64(), Some(0.0));
        let rendered =
            eval_painless("params['_source'].toString()", &ctx(&doc, &params, 0.0)).unwrap();
        let PainlessValue::String(rendered) = rendered else {
            panic!("expected rendered source string");
        };
        assert!(rendered.contains("content=visible"));
        assert!(!rendered.contains("__xerj_passage_meta__"));
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
        let src =
            "double s = dotProduct(params.q, 'vec'); return s < 0 ? 1.0 / (1.0 - s) : s + 1.0;";
        let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
        // dot = 1*1 + 2*0 + 3*-1 = -2 → 1/(1-(-2)) = 1/3
        assert!((v.as_f64().unwrap() - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn if_return() {
        let doc = json!({"x": 10});
        let params = json!({});
        let v = eval_painless(
            "if (doc['x'].value > 5) { return 100; } return 0;",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert!((v.as_f64().unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn math_max() {
        let doc = json!({});
        let params = json!({});
        let v = eval_painless("Math.max(1.5, 2.5)", &ctx(&doc, &params, 0.0)).unwrap();
        assert!((v.as_f64().unwrap() - 2.5).abs() < 1e-9);
    }

    // ── Local functions and lambdas ───────────────────────────────────────────
    // Needed by e.g. OpenSearch's UBI sample dashboards, which filter via a
    // Supplier-style boolean helper (either a top-level local function or a
    // lambda literal bound to a variable).

    #[test]
    fn local_function_declaration_and_call() {
        let doc = json!({});
        let params = json!({});
        let src = "boolean isEven(int n) { return n % 2 == 0; } return isEven(4);";
        let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
        assert!(v.as_bool());
    }

    #[test]
    fn local_function_false_branch() {
        let doc = json!({});
        let params = json!({});
        let src = "boolean isEven(int n) { return n % 2 == 0; } return isEven(3);";
        let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
        assert!(!v.as_bool());
    }

    #[test]
    fn lambda_literal_bare_call() {
        let doc = json!({});
        let params = json!({});
        let v = eval_painless(
            "def add = (a, b) -> a + b; return add(2, 3);",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert!((v.as_f64().unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn lambda_block_body() {
        let doc = json!({});
        let params = json!({});
        let src = "def f = (x) -> { double y = x * 2; return y + 1; }; return f(10);";
        let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
        assert!((v.as_f64().unwrap() - 21.0).abs() < 1e-9);
    }

    #[test]
    fn lambda_invoked_via_functional_interface_method() {
        // Any `.method(args)` call on a closure value invokes it regardless
        // of the method name — covers Supplier::get, Function::apply,
        // Predicate::test, etc. without hard-coding each interface.
        let doc = json!({});
        let params = json!({});
        let v = eval_painless(
            "def s = () -> true; return s.get();",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert!(v.as_bool());

        let v2 = eval_painless(
            "def doubler = (x) -> x * 2; return doubler.apply(21);",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert!((v2.as_f64().unwrap() - 42.0).abs() < 1e-9);
    }

    #[test]
    fn lambda_no_access_to_enclosing_locals() {
        // Closures are invoked in a fresh scope seeded only with the bound
        // parameters — they can't see the caller's other local variables.
        let doc = json!({});
        let params = json!({});
        let src = "int secret = 99; def f = () -> secret; return f();";
        let r = eval_painless(src, &ctx(&doc, &params, 0.0));
        assert!(r.is_err(), "expected an error, got {:?}", r);
    }

    #[test]
    fn closure_as_top_level_result_is_not_a_valid_value() {
        // Returning a bare function value from a script (rather than
        // invoking it) has no scalar/JSON representation.
        let doc = json!({});
        let params = json!({});
        let v = eval_painless("def f = () -> 1; return f;", &ctx(&doc, &params, 0.0)).unwrap();
        assert!(matches!(v, PainlessValue::Closure(..)));
    }

    #[test]
    fn wrong_arity_call_errors_instead_of_defaulting_to_null() {
        let doc = json!({});
        let params = json!({});
        let r = eval_painless(
            "def f = (a, b) -> a + b; return f(1);",
            &ctx(&doc, &params, 0.0),
        );
        assert!(r.is_err(), "expected an arity error, got {:?}", r);
    }

    // ── Closure call-depth / call-count guards ─────────────────────────────────
    // Regression tests for a real crash: `EvalDepthGuard` only bounds
    // expression-eval recursion, not closure *call* nesting or the total
    // size of an exponential call tree. Both of these MUST return an `Err`
    // — if the guard regresses, the test process itself stack-overflows (or
    // takes minutes) and aborts/hangs, a hard failure either way.

    #[test]
    fn self_application_with_nested_statement_body_does_not_abort() {
        // Mirrors the exact shape that stack-overflowed in release: a
        // closure passed as its own argument (`f(f, n)`), with a body
        // containing enough nested `if` blocks that MAX_PARSE_DEPTH's
        // per-call statement-nesting cost, multiplied across many calls
        // via MAX_EVAL_DEPTH, would otherwise blow the native stack before
        // any depth guard fired.
        let doc = json!({});
        let params = json!({});
        let nested_ifs = "if(true){".repeat(10) + "1" + &"}".repeat(10);
        let src = format!(
            "def f = (g, n) -> {{ if(true) {{ {nested_ifs} return g(g, n); }} return 0; }}; return f(f, 1);"
        );
        let r = eval_painless(&src, &ctx(&doc, &params, 0.0));
        assert!(
            r.is_err(),
            "expected the call-depth guard to trip, got {:?}",
            r
        );
    }

    #[test]
    fn exponential_call_tree_is_bounded_not_a_hang() {
        // Call depth here never exceeds ~9 (n counts down to 0), so
        // MAX_CALL_DEPTH alone would never fire — it's the total
        // invocation count (4^n) that must be bounded.
        let doc = json!({});
        let params = json!({});
        let src =
            "def f = (g, n) -> n <= 0 ? 1 : g(g,n-1) + g(g,n-1) + g(g,n-1) + g(g,n-1); return f(f, 9);";
        let r = eval_painless(src, &ctx(&doc, &params, 0.0));
        assert!(
            r.is_err(),
            "expected the call-count guard to trip, got {:?}",
            r
        );
    }

    // ── Resource-limit / stack-overflow guards ───────────────────────────────
    // Regression tests for the unauthenticated remote crash: a deeply nested
    // script used to overflow the parser's (or evaluator's) recursion and abort
    // the whole process. These MUST return an `Err` — if the guard regresses,
    // the test process itself stack-overflows and aborts (a hard failure), so
    // the test can never silently pass.

    #[test]
    fn deeply_nested_parens_do_not_overflow_parser() {
        let doc = json!({});
        let params = json!({});
        // ~5000 nested parens — well beyond the ~3000 that overflowed the
        // real server before the guard.
        let src = format!("{}1.0{}", "(".repeat(5000), ")".repeat(5000));
        let r = eval_painless(&src, &ctx(&doc, &params, 0.0));
        assert!(r.is_err(), "expected depth-limit error, got {:?}", r);
        assert_eq!(r.unwrap_err(), TOO_DEEP_MSG);
    }

    #[test]
    fn deeply_nested_unary_do_not_overflow_parser() {
        let doc = json!({});
        let params = json!({});
        // Unary chains recurse through parse_unary directly. Use `!` (logical
        // NOT): unlike `-`, consecutive `!` are NOT collapsed into a multi-char
        // token by the lexer, so this genuinely drives deep unary recursion.
        let src = format!("{}true", "!".repeat(5000));
        let r = eval_painless(&src, &ctx(&doc, &params, 0.0));
        assert!(r.is_err(), "expected depth-limit error, got {:?}", r);
        assert_eq!(r.unwrap_err(), TOO_DEEP_MSG);
    }

    #[test]
    fn long_flat_binary_chain_does_not_overflow_evaluator() {
        let doc = json!({});
        let params = json!({});
        // A flat `1+1+1+…` chain is parsed with a LOOP (not deep recursion),
        // so it used to build a 5,001-deep left-leaning AST and abort the
        // process on the evaluator's native stack. It must now fail while
        // parsing, before that evaluator is entered.
        let src = format!("1{}", "+1".repeat(5000));
        let error = eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap_err();
        assert_eq!(error, EVAL_TOO_DEEP_MSG);
    }

    #[test]
    fn flat_binary_chain_accepts_exact_eval_depth_boundary() {
        let doc = json!({});
        let params = json!({});
        // A leaf has depth one, so MAX_EVAL_DEPTH - 1 binary operators
        // produce an expression whose exact evaluation depth is the limit.
        let src = format!("1{}", "+1".repeat(MAX_EVAL_DEPTH - 1));
        let value = eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap();
        assert_eq!(value.as_f64(), Some(MAX_EVAL_DEPTH as f64));

        let too_deep = format!("1{}", "+1".repeat(MAX_EVAL_DEPTH));
        assert_eq!(
            eval_painless(&too_deep, &ctx(&doc, &params, 0.0)).unwrap_err(),
            EVAL_TOO_DEEP_MSG
        );
    }

    #[test]
    fn postfix_chain_accepts_exact_eval_depth_boundary() {
        let doc = json!({});
        let params = json!({});
        // Statement evaluation must not consume one level of the expression
        // budget: a leaf plus 499 calls is exactly depth 500.
        let src = format!(
            "\"x\"{}",
            ".toString()".repeat(MAX_EVAL_DEPTH.saturating_sub(1))
        );
        let value = eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap();
        assert!(matches!(value, PainlessValue::String(ref text) if text == "x"));

        let too_deep = format!("\"x\"{}", ".toString()".repeat(MAX_EVAL_DEPTH));
        assert_eq!(
            eval_painless(&too_deep, &ctx(&doc, &params, 0.0)).unwrap_err(),
            EVAL_TOO_DEEP_MSG
        );
    }

    #[test]
    fn index_chain_accepts_exact_boundary_without_native_recursion() {
        let doc = json!({});
        let params = json!({"items": [7]});
        // params.items has depth two; 498 indices reach exactly 500.
        let src = format!(
            "params.items{}",
            "[0]".repeat(MAX_EVAL_DEPTH.saturating_sub(2))
        );
        assert!(eval_painless(&src, &ctx(&doc, &params, 0.0)).is_ok());

        let too_deep = format!(
            "params.items{}",
            "[0]".repeat(MAX_EVAL_DEPTH.saturating_sub(1))
        );
        assert_eq!(
            eval_painless(&too_deep, &ctx(&doc, &params, 0.0)).unwrap_err(),
            EVAL_TOO_DEEP_MSG
        );
    }

    #[test]
    fn argument_depth_contributes_exactly_once() {
        let doc = json!({});
        let params = json!({});
        // The argument has depth 499 and the Math member-call parent makes 500.
        let boundary_arg = format!("1{}", "+1".repeat(MAX_EVAL_DEPTH - 2));
        let boundary = format!("Math.max(0, {boundary_arg})");
        assert_eq!(
            eval_painless(&boundary, &ctx(&doc, &params, 0.0))
                .unwrap()
                .as_f64(),
            Some((MAX_EVAL_DEPTH - 1) as f64)
        );

        let over_limit_arg = format!("1{}", "+1".repeat(MAX_EVAL_DEPTH - 1));
        let over_limit = format!("Math.max(0, {over_limit_arg})");
        assert_eq!(
            eval_painless(&over_limit, &ctx(&doc, &params, 0.0)).unwrap_err(),
            EVAL_TOO_DEEP_MSG
        );
    }

    #[test]
    fn iterative_access_preserves_special_dispatch_and_error_order() {
        let doc = json!({"date": "2024-03-04T05:06:07Z"});
        let params = json!({"obj": {"name": "X"}, "items": [3]});
        let context = ctx(&doc, &params, 0.0);

        assert!(matches!(
            eval_painless("params.obj.name.toLowerCase()", &context).unwrap(),
            PainlessValue::String(value) if value == "x"
        ));
        assert_eq!(
            eval_painless("params.items[0]", &context).unwrap().as_f64(),
            Some(3.0)
        );
        assert_eq!(
            eval_painless("doc['date'].value.getHour()", &context)
                .unwrap()
                .as_f64(),
            Some(5.0)
        );
        assert_eq!(
            eval_painless("Math.max(2, 4).toString()", &context).unwrap_err(),
            "unsupported member access .toString"
        );

        // String methods historically ignore their syntactic argument list;
        // the unknown call must remain unevaluated.
        assert!(matches!(
            eval_painless("\"X\".toLowerCase(missing())", &context).unwrap(),
            PainlessValue::String(value) if value == "x"
        ));
        // General index access evaluates its base before its key.
        assert_eq!(
            eval_painless("missing()[alsoMissing()]", &context).unwrap_err(),
            "unsupported function missing"
        );
        // The doc special form evaluates the key directly.
        assert_eq!(
            eval_painless("doc[missing()]", &context).unwrap_err(),
            "unsupported function missing"
        );
    }

    #[test]
    fn binary_depth_tracks_mixed_precedence_and_associativity() {
        let doc = json!({});
        let params = json!({});
        // The exact AST is (((1 + (2 * 3)) + (4 / 2)) - 5), not a token-count
        // approximation. This also exercises the iterative left-spine
        // evaluator across distinct precedence levels.
        let value = eval_painless("1 + 2 * 3 + 4 / 2 - 5", &ctx(&doc, &params, 0.0)).unwrap();
        assert_eq!(value.as_f64(), Some(4.0));
    }

    #[test]
    fn iterative_binary_chain_preserves_short_circuiting() {
        let doc = json!({});
        let params = json!({});
        // `missing()` would return an error if evaluated. Every RHS must remain
        // skipped even though the left-associated spine is evaluated in a loop.
        let src = format!(
            "false{}",
            " && missing()".repeat(MAX_EVAL_DEPTH.saturating_sub(2))
        );
        let value = eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap();
        assert!(!value.as_bool());
    }

    #[test]
    fn eval_depth_error_is_actionable_and_limit_checks_report_it() {
        let src = format!("1{}", "+1".repeat(MAX_EVAL_DEPTH));
        let error = check_script_limits(&src).unwrap_err();
        assert_eq!(error, EVAL_TOO_DEEP_MSG);
        assert!(error.contains("split the expression into smaller statements"));
    }

    #[test]
    fn oversized_source_rejected() {
        let doc = json!({});
        let params = json!({});
        let src = format!("{} + 1.0", "1.0".repeat(MAX_SCRIPT_LEN));
        let r = eval_painless(&src, &ctx(&doc, &params, 0.0));
        assert!(r.is_err(), "expected length-limit error, got {:?}", r);
    }

    #[test]
    fn check_script_limits_flags_nesting_but_ignores_plain_syntax_errors() {
        // Nesting past the cap → reported (becomes a 400 at the request layer).
        let deep = format!("{}1.0{}", "(".repeat(5000), ")".repeat(5000));
        assert!(check_script_limits(&deep).is_err());

        // Oversized → reported.
        let big = "1.0".repeat(MAX_SCRIPT_LEN);
        assert!(check_script_limits(&big).is_err());

        // Deep unary chain past the cap → reported (use `!`; see note above).
        let deep_unary = format!("{}true", "!".repeat(5000));
        assert!(
            check_script_limits(&deep_unary).is_err(),
            "deep unary should be flagged by the parse-depth guard"
        );

        // A normal, valid script → OK.
        assert!(check_script_limits("doc['x'].value * 2 + _score").is_ok());

        // An unsupported-but-shallow script (syntax our subset rejects) must
        // NOT be flagged — it should keep degrading gracefully at runtime, not
        // turn into a spurious 400.
        assert!(check_script_limits("some garbage )(").is_ok());
    }

    #[test]
    fn deeply_nested_statements_do_not_overflow_parser() {
        let doc = json!({});
        let params = json!({});
        let src = format!(
            "{}return 1;{}",
            "if (true) {".repeat(5000),
            "}".repeat(5000)
        );
        assert_eq!(
            eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap_err(),
            TOO_DEEP_MSG
        );
        assert_eq!(check_script_limits(&src).unwrap_err(), TOO_DEEP_MSG);
    }

    #[test]
    fn normal_script_still_evaluates_after_guards() {
        let doc = json!({"x": 4});
        let params = json!({"m": 3});
        // Moderate nesting well within limits still works.
        let v = eval_painless(
            "((doc['x'].value + 1) * params.m)",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        assert!((v.as_f64().unwrap() - 15.0).abs() < 1e-9);
    }

    // ── String comparison semantics (RC4 W2 item 6) ─────────────────────
    //
    // Regression: string operands used to coerce to 0.0 on both sides of
    // every comparison, so `doc['color'].value == 'red'` was true for ALL
    // docs. Strings must compare as strings (ES `def` equality semantics).

    fn eval_bool(src: &str, doc: &Value) -> bool {
        let params = json!({});
        eval_painless(src, &ctx(doc, &params, 0.0))
            .unwrap()
            .as_bool()
    }

    #[test]
    fn string_equality_compares_content() {
        let doc = json!({"color": "blue"});
        assert!(!eval_bool("doc['color'].value == 'red'", &doc));
        assert!(eval_bool("doc['color'].value == 'blue'", &doc));
        assert!(eval_bool("doc['color'].value != 'red'", &doc));
        assert!(!eval_bool("doc['color'].value != 'blue'", &doc));
    }

    #[test]
    fn string_vs_non_string_is_not_equal() {
        let doc = json!({"color": "red", "n": 5});
        // ES Painless def equality: String.equals(non-String) is false.
        assert!(!eval_bool("doc['color'].value == 5", &doc));
        assert!(eval_bool("doc['color'].value != 5", &doc));
        assert!(!eval_bool("doc['color'].value == null", &doc));
        // Numeric string does NOT numerically equal a number.
        let doc2 = json!({"tag": "5"});
        assert!(!eval_bool("doc['tag'].value == 5", &doc2));
    }

    #[test]
    fn string_relational_is_lexicographic() {
        let doc = json!({"color": "blue"});
        assert!(eval_bool("doc['color'].value < 'red'", &doc));
        assert!(eval_bool("doc['color'].value <= 'blue'", &doc));
        assert!(!eval_bool("doc['color'].value > 'red'", &doc));
        assert!(eval_bool("'9' > '10'", &doc)); // lexicographic, not numeric
    }

    #[test]
    fn string_equality_in_ternary_and_params() {
        let doc = json!({"color": "green"});
        let params = json!({"want": "green"});
        let v = eval_painless(
            "doc['color'].value == params.want ? 'A' : 'B'",
            &ctx(&doc, &params, 0.0),
        )
        .unwrap();
        match v {
            PainlessValue::String(s) => assert_eq!(s, "A"),
            other => panic!("expected string, got {:?}", other),
        }
        // Numbers still compare numerically.
        assert!(eval_bool("1 + 1 == 2", &doc));
        assert!(eval_bool("doc['color'].value.length() == 5", &doc));
    }

    #[test]
    fn missing_doc_field_value_errors() {
        // This is ES's own behaviour, not a local hardening choice: since
        // 7.0 an empty `ScriptDocValues` throws rather than yielding a
        // default (6.x returned 0/"" with a deprecation warning). Returning
        // `null` here instead would reopen the hole this closes — see
        // `comparison_against_missing_field_errors_not_matches_everything`.
        let doc = json!({});
        let params = json!({});
        let r = eval_painless("doc['missing'].value", &ctx(&doc, &params, 0.0));
        let e = r.expect_err("a missing field must error, matching ES 7+");
        // The message points at the supported guard, exactly as ES's does.
        assert!(
            e.contains("size()==0"),
            "the error must name the .size()==0 guard: {e}"
        );
    }

    #[test]
    fn comparison_against_missing_field_errors_not_matches_everything() {
        let doc = json!({});
        let params = json!({});
        let r = eval_painless("doc['missing'].value > -1", &ctx(&doc, &params, 0.0));
        assert!(
            r.is_err(),
            "a comparison against a missing field must error, not silently match: got {:?}",
            r
        );
    }

    #[test]
    fn empty_multi_value_field_errors() {
        let doc = json!({"tags": []});
        let params = json!({});
        let r = eval_painless("doc['tags'].value", &ctx(&doc, &params, 0.0));
        assert!(r.is_err(), "expected an error, got {:?}", r);
    }

    #[test]
    fn present_field_still_works_after_fail_closed_change() {
        let doc = json!({"x": 10});
        let params = json!({});
        let v = eval_painless("doc['x'].value > 5", &ctx(&doc, &params, 0.0)).unwrap();
        assert!(v.as_bool());
    }

    #[test]
    fn exponential_string_doubling_is_bounded_not_a_crash() {
        let doc = json!({});
        let params = json!({});
        // 30 doublings from a 1-byte string would reach ~1 GiB unbounded;
        // the cap must trip well before that.
        let src = format!("def s = \"a\";{}return s;", "s = s + s;".repeat(30));
        let r = eval_painless(&src, &ctx(&doc, &params, 0.0));
        assert!(
            r.is_err(),
            "expected the string-length cap to trip, got {:?}",
            r
        );
    }

    #[test]
    fn ordinary_string_concatenation_still_works() {
        let doc = json!({});
        let params = json!({});
        let v = eval_painless("'a' + 'b' + 'c'", &ctx(&doc, &params, 0.0)).unwrap();
        match v {
            PainlessValue::String(s) => assert_eq!(s, "abc"),
            other => panic!("expected a string, got {:?}", other),
        }
    }

    // ── The null-guard idiom ──────────────────────────────────────────────────
    // Making a missing field error (above) is only safe if a script can still
    // ASK whether the field is there. Elasticsearch's answer is
    // `doc['x'].size() == 0`, which is also the remedy its own exception
    // message names, so that is the idiom that has to work here. It relies on
    // two properties, both asserted below: `.size()` is defined on a missing
    // field, and the ternary evaluates only the branch it takes.

    #[test]
    fn size_guard_is_defined_on_a_missing_field() {
        let params = json!({});
        for (doc, expected_size, expected_empty) in [
            (json!({}), 0.0, true),
            (json!({"x": null}), 0.0, true),
            (json!({"x": []}), 0.0, true),
            (json!({"x": 7}), 1.0, false),
            (json!({"x": [1, 2, 3]}), 3.0, false),
        ] {
            let c = ctx(&doc, &params, 0.0);
            let size = eval_painless("doc['x'].size()", &c)
                .unwrap_or_else(|e| panic!(".size() must never error on {doc}: {e}"));
            assert_eq!(size.as_f64().unwrap(), expected_size, "doc {doc}");
            let empty = eval_painless("doc['x'].empty", &c)
                .unwrap_or_else(|e| panic!(".empty must never error on {doc}: {e}"));
            assert_eq!(empty.as_bool(), expected_empty, "doc {doc}");
        }
    }

    #[test]
    fn null_guard_idiom_evaluates_on_both_shapes() {
        // The canonical ES form, verbatim.
        let src = "doc['x'].size() == 0 ? 0 : doc['x'].value";
        let params = json!({});

        for (doc, expected) in [
            (json!({}), 0.0),
            (json!({"x": []}), 0.0),
            (json!({"x": 42}), 42.0),
            (json!({"x": [7, 8]}), 7.0),
        ] {
            let v = eval_painless(src, &ctx(&doc, &params, 0.0))
                .unwrap_or_else(|e| panic!("the null-guard idiom must not error on {doc}: {e}"));
            assert_eq!(v.as_f64().unwrap(), expected, "doc {doc}");
        }
    }

    #[test]
    fn guarded_script_filters_rather_than_erroring_everywhere() {
        // The whole point of the idiom: one script, run over documents that
        // do and don't have the field, returning a boolean for each.
        let params = json!({"min": 5});
        let src = "doc['x'].size() == 0 ? false : doc['x'].value > params.min";

        for (doc, expected) in [
            (json!({"x": 10}), true),
            (json!({"x": 1}), false),
            (json!({}), false),
        ] {
            let v = eval_painless(src, &ctx(&doc, &params, 0.0))
                .unwrap_or_else(|e| panic!("guarded script errored on {doc}: {e}"));
            assert_eq!(v.as_bool(), expected, "doc {doc}");
        }
    }

    #[test]
    fn null_guard_on_a_missing_param_is_a_boolean() {
        // `params` is a real `Map<String, Object>` in Painless, so a key
        // that wasn't supplied reads as null and comparing it to null is an
        // ordinary reference comparison — unlike `doc[...]`, which throws.
        let doc = json!({});

        let empty = json!({});
        let v = eval_painless("params.y == null", &ctx(&doc, &empty, 0.0))
            .expect("a params null guard must evaluate, not error");
        assert!(v.as_bool(), "missing param: `== null` must be true");

        let supplied = json!({"y": 3});
        let v = eval_painless("params.y == null", &ctx(&doc, &supplied, 0.0))
            .expect("a params null guard must evaluate, not error");
        assert!(!v.as_bool(), "supplied param: `== null` must be false");

        // `!=` is the same comparison inverted, and a present-but-null value
        // is indistinguishable from an absent one, as in ES.
        let explicit_null = json!({"y": null});
        let v = eval_painless("params.y != null", &ctx(&doc, &explicit_null, 0.0)).unwrap();
        assert!(!v.as_bool());
    }

    #[test]
    fn genuinely_invalid_arithmetic_still_errors() {
        let doc = json!({});
        let params = json!({"obj": {"a": 1}});
        // Null-awareness is scoped to `==`/`!=`; nothing else got looser, so
        // the fail-closed property the script query depends on is intact.
        for src in [
            "'abc' - params.obj",
            "params.missing + 1",
            "params.missing > 0",
            "doc['nope'].value * 2",
            "doc['nope'].value >= 0",
            "-params.missing",
        ] {
            let r = eval_painless(src, &ctx(&doc, &params, 0.0));
            assert!(r.is_err(), "`{src}` must still error, got {:?}", r);
        }
    }

    // ── Compiled-script cache ─────────────────────────────────────────────────
    // The cache is thread-local, and each `#[test]` gets its own thread, so
    // every assertion below observes only what its own test evaluated.

    /// `(entries, src_bytes, actual bytes held)` for the calling thread.
    fn cache_stats() -> (usize, usize, usize) {
        SCRIPT_CACHE.with(|c| {
            let c = c.borrow();
            (
                c.entries.len(),
                c.src_bytes,
                c.entries.keys().map(|k| k.len()).sum(),
            )
        })
    }

    #[test]
    fn compiled_script_cache_is_bounded_by_entry_count() {
        let doc = json!({});
        let params = json!({});
        // Far more distinct sources than the cache may retain. Each is tiny,
        // so the entry-count bound is the one under test.
        for i in 0..(MAX_SCRIPT_CACHE_ENTRIES * 8) {
            let src = format!("{i} + 1");
            eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap();
        }
        let (entries, src_bytes, actual) = cache_stats();
        assert!(
            entries <= MAX_SCRIPT_CACHE_ENTRIES,
            "cache grew to {entries} entries, bound is {MAX_SCRIPT_CACHE_ENTRIES}"
        );
        assert!(
            src_bytes <= MAX_SCRIPT_CACHE_SRC_BYTES,
            "cache holds {src_bytes} source bytes, bound is {MAX_SCRIPT_CACHE_SRC_BYTES}"
        );
        // The accounting the bound rests on must match reality.
        assert_eq!(actual, src_bytes, "src_bytes accounting drifted");
    }

    #[test]
    fn compiled_script_cache_is_bounded_by_source_bytes() {
        let doc = json!({});
        let params = json!({});
        // Distinct ~33 KiB scripts (each legal under MAX_SCRIPT_LEN, and
        // flat rather than deep so they compile and run cleanly). Well under
        // MAX_SCRIPT_CACHE_ENTRIES of them already exceed the byte budget,
        // so the byte bound has to be what stops the growth.
        for i in 0..24 {
            let src = format!("{}return {i};", "def a = 1;".repeat(3300));
            assert!(src.len() > 32 * 1024);
            eval_painless(&src, &ctx(&doc, &params, 0.0)).unwrap();
        }
        let (entries, src_bytes, actual) = cache_stats();
        assert!(
            src_bytes <= MAX_SCRIPT_CACHE_SRC_BYTES,
            "cache holds {src_bytes} source bytes, bound is {MAX_SCRIPT_CACHE_SRC_BYTES}"
        );
        assert!(entries <= MAX_SCRIPT_CACHE_ENTRIES);
        assert!(
            entries < 24,
            "the byte bound must have evicted something: {entries} entries retained"
        );
        assert_eq!(actual, src_bytes, "src_bytes accounting drifted");
    }

    #[test]
    fn cached_script_is_parsed_once_and_still_correct() {
        let params = json!({"multiplier": 3});
        let src = "doc['n'].value * params.multiplier /* cache-once probe */";
        for n in 1..=5 {
            let doc = json!({ "n": n });
            let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
            assert!((v.as_f64().unwrap() - (n * 3) as f64).abs() < 1e-9);
        }
        assert!(
            SCRIPT_CACHE.with(|c| c.borrow().entries.contains_key(src)),
            "repeated evaluation of one source must leave a cached AST"
        );
        assert_eq!(
            cache_stats().0,
            1,
            "five evaluations of one source must leave exactly one entry"
        );
    }

    #[test]
    fn cached_parse_failure_still_reports_the_same_error() {
        let doc = json!({});
        let params = json!({});
        let src = "return ( ( ( /* unbalanced, cache-error probe */";
        let first = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap_err();
        let second = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap_err();
        assert_eq!(first, second);
    }

    #[test]
    fn caching_does_not_share_state_between_evaluations() {
        // The AST is shared but mutable evaluation state is not: a script
        // that mutates a local must start from scratch on every document.
        let params = json!({});
        let src = "def acc = 0; acc = acc + doc['n'].value; \
                   acc = acc + doc['n'].value; acc = acc + doc['n'].value; \
                   return acc; /* shared-state probe */";
        for n in 1..=4 {
            let doc = json!({ "n": n });
            let v = eval_painless(src, &ctx(&doc, &params, 0.0)).unwrap();
            assert_eq!(v.as_f64().unwrap(), (n * 3) as f64, "n = {n}");
        }
    }

    #[test]
    fn cached_ast_does_not_leak_the_call_budget_across_evaluations() {
        // MAX_CALL_COUNT lives on the per-evaluation `PainlessCtx`, not on
        // the AST, so memoising the AST must not let one document's closure
        // invocations count against the next document's budget. A script
        // that spends a large slice of the budget has to keep succeeding
        // when it is re-run against document after document.
        let doc = json!({});
        let params = json!({});
        // 2^11 = 2048 invocations per evaluation, comfortably under
        // MAX_CALL_COUNT (10_000) but far more than a per-process budget
        // would survive being run 8 times.
        let src = "def f = (g, n) -> { if (n <= 0) { return 1; } return g(g, n - 1) + g(g, n - 1); }; return f(f, 11); /* budget-reset probe */";
        for round in 0..8 {
            let v = eval_painless(src, &ctx(&doc, &params, 0.0))
                .unwrap_or_else(|e| panic!("round {round} must stay within budget: {e}"));
            assert!((v.as_f64().unwrap() - 2048.0).abs() < 1e-9, "round {round}");
        }
    }
}
