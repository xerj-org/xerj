//! Shared scripted-update helpers (`_update`, `_update_by_query`, `_bulk`).
//!
//! The statement interpreter for `ctx._source` painless update scripts used
//! to live in `xerj-api` (`es_compat.rs`), which the bulk engine cannot
//! depend on. It moved here so all three update paths evaluate a script with
//! the same code: a scripted bulk update must behave exactly like the
//! single-document `_update` for the same script, not a second dialect.
//! Issue #3462: `_bulk` `update` used to drop `script` entirely and answer
//! a 200 `noop` with the document unchanged.

use serde_json::{json, Value};

use crate::painless::{eval_painless, PainlessCtx, PainlessValue};

/// Pull the `(source, params)` out of an ES `script` value. Accepts the short
/// string form and the object form `{ source, lang, params }`.
pub fn extract_update_script(script: &Value) -> (String, Value) {
    match script {
        Value::String(s) => (s.trim().to_string(), json!({})),
        Value::Object(_) => {
            let src = script
                .get("source")
                .or_else(|| script.get("inline"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let params = script.get("params").cloned().unwrap_or_else(|| json!({}));
            (src, params)
        }
        _ => (String::new(), json!({})),
    }
}

/// Consume a `ctx._source` accessor path (`.a.b`, `['a']["b"]`) from the start
/// of `rest`, returning the parsed path segments and the number of bytes
/// consumed. Stops at the first byte that is not part of the path.
fn consume_source_path(rest: &str) -> (Vec<String>, usize) {
    let mut path: Vec<String> = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    loop {
        if i < b.len() && b[i] == b'.' {
            let start = i + 1;
            let mut k = start;
            while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                k += 1;
            }
            if k == start {
                break;
            }
            path.push(rest[start..k].to_string());
            i = k;
        } else if i < b.len() && b[i] == b'[' {
            if let Some(rel) = rest[i..].find(']') {
                let close = i + rel;
                let inner = rest[i + 1..close]
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"');
                path.push(inner.to_string());
                i = close + 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    (path, i)
}

/// Rewrite `ctx._source.<path>` / `ctx._source['<path>']` references inside an
/// expression into the `doc['<path>'].value` form understood by the painless
/// evaluator, so the right-hand side of an update assignment can read the
/// document's current source.
fn rewrite_source_refs(s: &str) -> String {
    const PREFIX: &str = "ctx._source";
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(PREFIX) {
            let after = &s[i + PREFIX.len()..];
            let (path, consumed) = consume_source_path(after);
            if !path.is_empty() {
                out.push_str(&format!("doc['{}'].value", path.join(".")));
                i += PREFIX.len() + consumed;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `doc['a.b'].value` form for a parsed source path (used to desugar compound
/// assignment operators).
fn source_path_as_doc_ref(path: &[String]) -> String {
    format!("doc['{}'].value", path.join("."))
}

fn get_source_path(source: &Value, path: &[String]) -> Value {
    let mut cur = source;
    for p in path {
        match cur.get(p) {
            Some(v) => cur = v,
            None => return Value::Null,
        }
    }
    cur.clone()
}

fn set_source_path(source: &mut Value, path: &[String], value: Value) {
    if path.is_empty() {
        return;
    }
    if !source.is_object() {
        *source = json!({});
    }
    let obj = source.as_object_mut().expect("ensured object above");
    if path.len() == 1 {
        obj.insert(path[0].clone(), value);
    } else {
        let child = obj.entry(path[0].clone()).or_insert_with(|| json!({}));
        set_source_path(child, &path[1..], value);
    }
}

/// Convert a `PainlessValue` back to `serde_json::Value`.
fn painless_to_json(v: PainlessValue) -> Value {
    match v {
        PainlessValue::Null => Value::Null,
        PainlessValue::Bool(b) => Value::Bool(b),
        PainlessValue::Number(n) => serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        PainlessValue::String(s) => Value::String(s),
        PainlessValue::Array(a) => Value::Array(a.into_iter().map(painless_to_json).collect()),
        PainlessValue::Object(o) => Value::Object(o),
        // A function value has no JSON representation, same fail-closed
        // treatment as the equivalent conversion in aggs.rs.
        PainlessValue::Closure(..) => Value::Null,
    }
}

/// Integer-preserving conversion of a painless result into JSON (whole numbers
/// stay integers so `ctx._source.x = 42` stores `42`, not `42.0`).
fn painless_update_value(v: PainlessValue) -> Value {
    match v {
        PainlessValue::Number(n)
            if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 =>
        {
            Value::Number((n as i64).into())
        }
        other => painless_to_json(other),
    }
}

/// Parse a self-contained RHS literal (number / string / bool / null / array /
/// object) so its JSON type is preserved exactly. Returns `None` when the RHS
/// is an expression that must be evaluated.
fn parse_rhs_literal(rhs: &str) -> Option<Value> {
    let t = rhs.trim();
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') && !t[1..t.len() - 1].contains('\'')
    {
        return Some(Value::String(t[1..t.len() - 1].to_string()));
    }
    serde_json::from_str::<Value>(t).ok()
}

/// Evaluate an expression against the document's current source using the same
/// painless evaluator that backs `/_scripts/painless/_execute`.
fn eval_update_expr(expr: &str, source: &Value, params: &Value) -> Result<Value, String> {
    let ctx = PainlessCtx::new(source, params, 0.0);
    let pv = eval_painless(expr, &ctx)?;
    Ok(painless_update_value(pv))
}

/// Maximum depth of a `ctx._source` path an update script may address.
///
/// One statement builds one nested object per path segment, and the resulting
/// document is then walked recursively by FTS analysis (`collect_text_fields`),
/// serialisation and `Value` drop, so an unbounded path is a stack overflow on
/// the request thread (issue #3222). 64 is far above any legitimate update path
/// and far below any stack risk, and it sits under the other depth caps in
/// this area (painless `MAX_PARSE_DEPTH` 100, serde_json's 128 nesting cap).
const MAX_UPDATE_PATH_DEPTH: usize = 64;

/// Apply a painless update script to `source` in place. Supports the common
/// `ctx._source.*` mutation forms: assignment (`=`), compound assignment
/// (`+= -= *= /=`), increment / decrement (`++ --`), and `remove(...)`.
pub fn apply_painless_update(
    source: &mut Value,
    script_src: &str,
    params: &Value,
) -> Result<(), String> {
    for raw in split_update_statements(script_src) {
        let stmt = raw.trim();
        if stmt.is_empty() {
            continue;
        }
        apply_one_update_stmt(source, stmt, params)?;
    }
    Ok(())
}

/// Split a script into top-level statements on `;`, ignoring separators inside
/// single- or double-quoted string literals.
fn split_update_statements(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in src.chars() {
        match quote {
            Some(q) => {
                cur.push(ch);
                if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    cur.push(ch);
                } else if ch == ';' {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn apply_one_update_stmt(source: &mut Value, stmt: &str, params: &Value) -> Result<(), String> {
    let s = stmt.trim();
    let Some(rest) = s.strip_prefix("ctx._source") else {
        // Tolerate other `ctx.*` statements (e.g. `ctx.op = 'noop'`) as no-ops;
        // reject anything we genuinely don't understand so it surfaces as 400.
        if s.starts_with("ctx.") || s.starts_with("ctx[") {
            return Ok(());
        }
        return Err(format!("unsupported update script statement: {s}"));
    };

    // ctx._source.remove('field') / ctx._source.remove("field")
    if let Some(after) = rest.strip_prefix(".remove") {
        let after = after.trim_start();
        if let Some(inner) = after.strip_prefix('(') {
            let key = inner
                .trim_end_matches(')')
                .trim()
                .trim_matches(|c| c == '\'' || c == '"');
            if let Some(obj) = source.as_object_mut() {
                obj.remove(key);
            }
            return Ok(());
        }
    }

    let (path, consumed) = consume_source_path(rest);
    if path.is_empty() {
        return Err(format!("invalid update script target: {s}"));
    }
    // Bound the depth before any nesting is built: each segment adds one
    // nested object, and the document is walked recursively downstream, so
    // a path past this cap is a stack overflow, not an update (#3222).
    if path.len() > MAX_UPDATE_PATH_DEPTH {
        return Err(format!(
            "update script path is too deep ({} segments, max {}); split the update into shallower paths",
            path.len(),
            MAX_UPDATE_PATH_DEPTH
        ));
    }
    let opr = rest[consumed..].trim_start();

    // Increment / decrement.
    if opr == "++" || opr == "--" {
        let cur = get_source_path(source, &path).as_f64().unwrap_or(0.0);
        let nv = if opr == "++" { cur + 1.0 } else { cur - 1.0 };
        set_source_path(
            source,
            &path,
            painless_update_value(PainlessValue::Number(nv)),
        );
        return Ok(());
    }

    // Assignment (plain or compound).
    let (compound, rhs) = if let Some(r) = opr.strip_prefix("+=") {
        (Some('+'), r)
    } else if let Some(r) = opr.strip_prefix("-=") {
        (Some('-'), r)
    } else if let Some(r) = opr.strip_prefix("*=") {
        (Some('*'), r)
    } else if let Some(r) = opr.strip_prefix("/=") {
        (Some('/'), r)
    } else if let Some(r) = opr.strip_prefix('=') {
        (None, r)
    } else {
        return Err(format!("unsupported update script statement: {s}"));
    };
    let rhs = rhs.trim();

    let new_value = match compound {
        Some(op) => {
            let expr = format!(
                "({}) {} ({})",
                source_path_as_doc_ref(&path),
                op,
                rewrite_source_refs(rhs)
            );
            eval_update_expr(&expr, source, params)?
        }
        None => match parse_rhs_literal(rhs) {
            Some(v) => v,
            None => eval_update_expr(&rewrite_source_refs(rhs), source, params)?,
        },
    };
    set_source_path(source, &path, new_value);
    Ok(())
}
