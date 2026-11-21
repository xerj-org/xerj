//! What the agent sees: at most 5 ranked `path:line` lines beside the real
//! output, hard-capped at 300 tokens. No obligation language, no product pitch.
use super::detect::{Rule, Trigger};
use super::tier0::Hit;
use std::path::Path;

pub const MAX_LINES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Def,
    Terms(usize, usize),
    SeenBefore(u32),
}

#[derive(Clone, Debug)]
pub struct Line {
    pub abs: String,
    pub shown: String,
    pub line: usize,
    pub text: String,
    pub tag: Tag,
}

pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

fn ordinal(n: usize) -> String {
    match n {
        1 => "1st".into(),
        2 => "2nd".into(),
        3 => "3rd".into(),
        n => format!("{n}th"),
    }
}

pub fn display_path(abs: &Path, cwd: &Path) -> String {
    abs.strip_prefix(cwd)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| abs.display().to_string())
}

pub fn line_from_hit(h: &Hit, cwd: &Path, total: usize) -> Line {
    let tag = if h.hist >= 2 && h.covered < 2 {
        Tag::SeenBefore(h.hist)
    } else if h.def {
        Tag::Def
    } else {
        Tag::Terms(h.covered, total)
    };
    Line {
        abs: h.abs.display().to_string(),
        shown: display_path(&h.abs, cwd),
        line: h.line,
        text: h.text.clone(),
        tag,
    }
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// Build the injected block. Lines are dropped from the bottom until the
/// whole block fits `max_tokens`.
pub fn render(
    t: &Trigger,
    lines: &[Line],
    absent: &[String],
    tier1_age: Option<u64>,
    max_tokens: usize,
) -> String {
    let why = match t.rule {
        Rule::R1 => format!("{} empty related search (R1)", ordinal(t.zeros.max(2))),
        Rule::R2 => format!("{} related search (R2)", ordinal(t.calls)),
        Rule::R3 => "capped search, then narrower (R3)".into(),
        Rule::R4 => "reads without an edit (R4)".into(),
        Rule::R5 => "binary match (R5)".into(),
    };
    let age = tier1_age
        .map(|s| format!(" · index {s}s old"))
        .unwrap_or_default();
    let head =
        format!("[xerj hook · {why} · your command ran unchanged{age} · ignore if unhelpful]");
    let mut body: Vec<String> = lines
        .iter()
        .take(MAX_LINES)
        .map(|l| {
            let tag = match &l.tag {
                Tag::Def => "def".to_string(),
                Tag::Terms(a, b) => format!("{a}/{b} terms"),
                Tag::SeenBefore(n) => format!("seen before ({n} sessions)"),
            };
            format!("  {}:{}  {}  {}", l.shown, l.line, clip(&l.text, 60), tag)
        })
        .collect();
    for a in absent.iter().take(2) {
        body.push(format!("absent everywhere (exact): \"{}\"", clip(a, 40)));
    }
    if let Some(top) = lines.first().filter(|l| l.tag == Tag::Def) {
        if let Some(tok) = t.idents.first().or(t.toks.first()) {
            body.push(format!(
                "narrower: grep -n -i '{}' {}",
                clip(tok, 40),
                top.shown
            ));
        }
    }
    loop {
        let out = std::iter::once(head.clone())
            .chain(body.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        if estimate_tokens(&out) <= max_tokens || body.is_empty() {
            return out;
        }
        body.pop();
    }
}
