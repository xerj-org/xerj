//! The shared `_cat` table renderer — ES's RestTable semantics for the
//! common query parameters, implemented once (#1201).
//!
//! Every `_cat` endpoint accepts the same formatting params in ES
//! (`/_cat` common params, ES 8.13):
//!
//! - `h=a,b` — select and order the displayed columns; `*` selects all in
//!   default order. A name the endpoint does not have is dropped (ES's
//!   behaviour — a miss does not error). If nothing recognised remains the
//!   response body is empty (text) / `[]` (json).
//! - `v` — verbose: adds a header row of the (selected) column names, text
//!   format only. The header prints even when there are no rows.
//! - `bytes=b|k|kb|m|mb|g|gb|t|tb|p|pb` — re-renders byte-size columns in
//!   that unit as a plain integer: single-letter units are decimal (`k` =
//!   10³), two-letter units binary (`kb` = 2¹⁰). An unknown value is a 400
//!   naming the valid ones — the accepted-and-ignored class (#204) is what
//!   this module exists to close.
//! - `format=json` — an array of objects with string values, `h` respected.
//!
//! Text cells join with a single space; under `v` every cell (header
//! included) is additionally right-padded to its column's widest value —
//! the alignment `?v` is the documented way to get in ES.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::ApiError;

/// One rendered cell. `size_bytes` marks a byte-size column: `text` is the
/// default human rendering (`767b`, `4.6mb`), and a `bytes=` param re-renders
/// the cell from the raw count.
pub struct CatCell {
    pub text: String,
    pub size_bytes: Option<u64>,
}

impl CatCell {
    /// A plain (non-size) cell.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            size_bytes: None,
        }
    }

    /// A byte-size cell; `text` should be `human_bytes(bytes)` so the
    /// default rendering matches the endpoint's pre-`bytes=` output.
    pub fn size(bytes: u64, text: String) -> Self {
        Self {
            text,
            size_bytes: Some(bytes),
        }
    }
}

/// The `bytes=` unit. Single letters are decimal powers, two letters binary
/// (ES's documented split).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteUnit {
    B,
    K,
    Kb,
    M,
    Mb,
    G,
    Gb,
    T,
    Tb,
    P,
    Pb,
}

impl ByteUnit {
    /// Parse a `bytes=` value; `None` when the value is not one of ES's.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.trim() {
            "b" => Self::B,
            "k" => Self::K,
            "kb" => Self::Kb,
            "m" => Self::M,
            "mb" => Self::Mb,
            "g" => Self::G,
            "gb" => Self::Gb,
            "t" => Self::T,
            "tb" => Self::Tb,
            "p" => Self::P,
            "pb" => Self::Pb,
            _ => return None,
        })
    }

    /// Render `bytes` in this unit as the integer ES prints (truncated).
    pub fn render(self, bytes: u64) -> String {
        let divisor = match self {
            Self::B => 1.0,
            Self::K => 1e3,
            Self::Kb => 1024.0,
            Self::M => 1e6,
            Self::Mb => 1024.0 * 1024.0,
            Self::G => 1e9,
            Self::Gb => 1024.0 * 1024.0 * 1024.0,
            Self::T => 1e12,
            Self::Tb => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            Self::P => 1e15,
            Self::Pb => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        };
        ((bytes as f64 / divisor) as u64).to_string()
    }
}

/// The resolved formatting params shared by every `_cat` endpoint.
#[derive(Debug, Default, Clone)]
pub struct CatParams {
    /// `v` — verbose header (any of `""`, `true`, `1`, `yes`).
    pub v: bool,
    /// `format=json`.
    pub json: bool,
    /// `h=` raw value (comma-separated column selection).
    pub h: Option<String>,
    /// `bytes=` raw value, validated.
    pub bytes: Option<ByteUnit>,
}

impl CatParams {
    /// Resolve the raw query struct. Unknown `bytes=` values are a 400
    /// naming the valid units (honoured-or-refused, never ignored);
    /// `v=` accepts the boolean spellings and refuses anything else the
    /// same way.
    pub fn resolve(
        v: Option<&str>,
        format: Option<&str>,
        h: Option<&str>,
        bytes: Option<&str>,
    ) -> Result<Self, ApiError> {
        let v = match v {
            None => false,
            Some(raw) => match raw.trim() {
                "" | "true" | "1" | "yes" => true,
                "false" | "0" | "no" => false,
                other => {
                    return Err(ApiError::new(xerj_common::XerjError::invalid_query(
                        format!("unknown v value `{other}` (expected true or false)"),
                    )))
                }
            },
        };
        let bytes = match bytes {
            None => None,
            Some(raw) => Some(ByteUnit::parse(raw).ok_or_else(|| {
                ApiError::new(xerj_common::XerjError::invalid_query(format!(
                    "unknown bytes value `{raw}` (expected one of: b, k, kb, m, mb, g, gb, t, tb, p, pb)"
                )))
            })?),
        };
        Ok(Self {
            v,
            json: format == Some("json"),
            h: h.map(str::to_string),
            bytes,
        })
    }
}

/// A `_cat` table: default column names in order, and rows of cells.
pub struct CatTable {
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<CatCell>>,
}

impl CatTable {
    /// Column indices selected by `h`, in request order. `*` expands to the
    /// full default order; unknown names are dropped (ES drops a miss, it
    /// does not error); an empty/missing `h` selects everything.
    fn selected(&self, h: Option<&str>) -> Vec<usize> {
        let all: Vec<usize> = (0..self.columns.len()).collect();
        let Some(h) = h else {
            return all;
        };
        let mut out = Vec::new();
        for entry in h.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if entry == "*" {
                out.extend(all.iter().copied());
                continue;
            }
            if let Some(i) = self.columns.iter().position(|c| *c == entry) {
                out.push(i);
            }
        }
        out
    }

    /// The cell's rendered text under an active `bytes=` unit.
    fn cell_text(cell: &CatCell, unit: Option<ByteUnit>) -> String {
        match (unit, cell.size_bytes) {
            (Some(u), Some(b)) => u.render(b),
            _ => cell.text.clone(),
        }
    }

    /// Render text or JSON. Text pads every cell (and the header) to the
    /// column's widest value and separates with one space; with `v` the
    /// header line prints even for an empty table.
    pub fn render(&self, params: &CatParams) -> Response {
        let cols = self.selected(params.h.as_deref());
        if params.json {
            let arr: Vec<Value> = self
                .rows
                .iter()
                .map(|row| {
                    let mut obj = serde_json::Map::new();
                    for &i in &cols {
                        let text = Self::cell_text(&row[i], params.bytes);
                        obj.insert(self.columns[i].to_string(), json!(text));
                    }
                    Value::Object(obj)
                })
                .collect();
            return Json(arr).into_response();
        }

        let header: Vec<&str> = cols.iter().map(|&i| self.columns[i]).collect();
        let body_rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|row| {
                cols.iter()
                    .map(|&i| Self::cell_text(&row[i], params.bytes))
                    .collect()
            })
            .collect();

        // ES pads columns only under `v` — that alignment is the documented
        // reason to send it. Without `v`, cells join with a single space.
        let widths: Vec<usize> = if params.v {
            let mut widths = vec![0usize; cols.len()];
            for (i, name) in header.iter().enumerate() {
                widths[i] = widths[i].max(name.chars().count());
            }
            for row in &body_rows {
                for (i, cell) in row.iter().enumerate() {
                    widths[i] = widths[i].max(cell.chars().count());
                }
            }
            widths
        } else {
            vec![0usize; cols.len()]
        };

        let mut lines: Vec<String> = Vec::new();
        if params.v {
            lines.push(render_row(
                &header.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &widths,
            ));
        }
        for row in &body_rows {
            lines.push(render_row(row, &widths));
        }

        let body = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        };
        (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            body,
        )
            .into_response()
    }
}

/// One text row: pad every cell but the last to its column width, join
/// with a single space.
fn render_row(cells: &[String], widths: &[usize]) -> String {
    let last = cells.len().saturating_sub(1);
    cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            if i == last {
                cell.clone()
            } else {
                let pad = widths[i].saturating_sub(cell.chars().count());
                format!("{cell}{}", " ".repeat(pad))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> CatTable {
        CatTable {
            columns: vec!["index", "docs.count", "store.size"],
            rows: vec![
                vec![
                    CatCell::plain("books"),
                    CatCell::plain("120"),
                    CatCell::size(767, "767b".into()),
                ],
                vec![
                    CatCell::plain("films"),
                    CatCell::plain("9"),
                    CatCell::size(4_710_400, "4.5mb".into()),
                ],
            ],
        }
    }

    fn params(h: Option<&str>, v: bool, bytes: Option<&str>, json: bool) -> CatParams {
        let mut p = CatParams::resolve(None, if json { Some("json") } else { None }, h, bytes)
            .expect("params resolve");
        p.v = v;
        p
    }

    fn text(t: &CatTable, p: &CatParams) -> String {
        let resp = t.render(p);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX);
        // tests are sync; the body is a Full<Bytes> — extract via block_on
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let bytes = rt.block_on(body).unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn json_out(t: &CatTable, p: &CatParams) -> Value {
        let resp = t.render(p);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let bytes = rt.block_on(body).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// #1201: `h` selects and reorders columns, in text and json.
    #[test]
    fn h_selects_and_orders_columns() {
        let t = table();
        let out = text(&t, &params(Some("docs.count,index"), false, None, false));
        assert_eq!(out, "120 books\n9 films\n");

        let j = json_out(&t, &params(Some("index,store.size"), false, None, true));
        assert_eq!(
            j,
            serde_json::json!([
                {"index": "books", "store.size": "767b"},
                {"index": "films", "store.size": "4.5mb"},
            ])
        );
    }

    /// #1201: `h=*` is the full default order; an unknown name drops.
    #[test]
    fn h_star_selects_all_and_unknown_names_drop() {
        let t = table();
        let out = text(&t, &params(Some("*"), false, None, false));
        assert_eq!(out, "books 120 767b\nfilms 9 4.5mb\n");

        let dropped = text(&t, &params(Some("index,nosuch"), false, None, false));
        assert_eq!(dropped, "books\nfilms\n");
    }

    /// #1201: `v` adds a header row, aligned to the widest cell; it prints
    /// even when the table has no rows.
    #[test]
    fn v_adds_an_aligned_header_even_with_no_rows() {
        let mut t = table();
        let out = text(&t, &params(None, true, None, false));
        assert_eq!(
            out,
            "index docs.count store.size\nbooks 120        767b\nfilms 9          4.5mb\n"
        );

        t.rows.clear();
        let empty = text(&t, &params(None, true, None, false));
        assert_eq!(empty, "index docs.count store.size\n");
    }

    /// #1201: `bytes=b` re-renders size columns as plain integers; `kb` is
    /// binary (2¹⁰) and `k` decimal (10³); non-size columns are untouched.
    #[test]
    fn bytes_re_renders_size_columns_only() {
        let t = table();
        let b = text(&t, &params(None, false, Some("b"), false));
        assert_eq!(b, "books 120 767\nfilms 9 4710400\n");
        let kb = text(&t, &params(None, false, Some("kb"), false));
        assert_eq!(kb, "books 120 0\nfilms 9 4600\n");
        let k = text(&t, &params(None, false, Some("k"), false));
        assert_eq!(k, "books 120 0\nfilms 9 4710\n");

        let j = json_out(&t, &params(None, false, Some("b"), true));
        assert_eq!(j[0]["store.size"], json!("767"));
    }

    /// #1201: an unknown `bytes=` or `v=` value is a 400 naming the valid
    /// ones — never silently ignored.
    #[test]
    fn unknown_bytes_or_v_values_are_refused() {
        let err = CatParams::resolve(None, None, None, Some("mbb")).unwrap_err();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let resp = rt.block_on(async {
            let r = err.into_response();
            let b = axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap();
            String::from_utf8(b.to_vec()).unwrap()
        });
        assert!(resp.contains("bytes"), "{resp}");
        assert!(resp.contains("kb"), "{resp}");

        let err = CatParams::resolve(Some("maybe"), None, None, None).unwrap_err();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let resp = rt.block_on(async {
            let r = err.into_response();
            let b = axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap();
            String::from_utf8(b.to_vec()).unwrap()
        });
        assert!(resp.contains("v"), "{resp}");
    }

    /// `v` accepts the flag forms ES clients send (`?v`, `?v=true`) and the
    /// negatives.
    #[test]
    fn v_parses_its_boolean_forms() {
        for raw in ["", "true", "1", "yes"] {
            let p = CatParams::resolve(Some(raw), None, None, None).unwrap();
            assert!(p.v, "v={raw} must be verbose");
        }
        for raw in ["false", "0", "no"] {
            let p = CatParams::resolve(Some(raw), None, None, None).unwrap();
            assert!(!p.v, "v={raw} must not be verbose");
        }
    }

    /// An empty table with no `v` renders an empty body — ES's empty-listing
    /// shape (not a bare newline).
    #[test]
    fn empty_table_without_v_is_an_empty_body() {
        let mut t = table();
        t.rows.clear();
        let out = text(&t, &params(None, false, None, false));
        assert_eq!(out, "");
    }
}
