//! Man pages — roff `man(7)` source read as a document, one record per `.SH`
//! section.
//!
//! Most of a Unix system's own documentation is man pages, and as raw text
//! they index badly: every line is a macro (`.TP`, `.B \-a`), words carry
//! font escapes (`\fBls\fR`), and pod2man pages open with a hundred lines of
//! roff preamble. This reads the macros instead of formatting them: the page
//! title comes from `.TH` (`LS(1)`), the one-line summary from the NAME
//! section (`list directory contents`), and each `.SH` section (`SYNOPSIS`,
//! `OPTIONS`, …) becomes its own record, long ones split like any document.
//! A `.SS` subsection stays inside its section as a heading line.
//!
//! The text is plain: fonts are dropped, special characters (`\(em`,
//! `\[bu]`) become the characters they name, an option list (`.TP`/`.IP`)
//! keeps each term on its own line above its description, no-fill blocks
//! (`.nf`, `.EX`) keep their lines and indentation, and a tbl table becomes
//! one ` | `-joined line per row. Enough of the roff request language is
//! read for real pages — string definitions (`.ds`), conditionals
//! (`.if`/`.ie`/`.el`), macro definitions skipped (`.de`) — so pod2man's
//! preamble needs no special case. A macro the page defines itself (bash's
//! `.FN`) cannot be run, so its arguments are kept as text.
//!
//! Ported from the man parser in all2md (`parsers/man.py`, MIT, © 2025 Tom
//! Villani), reduced to plain text. Out of scope: mdoc(7) pages (`.Dd`/`.Sh`,
//! BSD and macOS); they do not sniff as man pages and are indexed as text,
//! as before. `.so` redirect stubs hold no text of their own and are the
//! same.

use super::{split_sections, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_RECORDS_PER_FILE};
use anyhow::Result;
use serde_json::{Map, Value};
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Decompressed page size read; bash(1), among the largest, is 350 KB.
const MAN_CAP: u64 = 16 << 20;
/// Text one page may produce. String interpolation can make output larger
/// than input, so it is bounded separately.
const MAX_PAGE_TEXT: usize = 32 << 20;
/// Nesting depth of string interpolation (`\*(xx` inside a string).
const MAX_STRING_DEPTH: u8 = 8;
/// String interpolations per page. A string that names other strings several
/// times grows exponentially with depth; this bounds the work.
const MAX_INTERPOLATIONS: u32 = 200_000;

/// `name` is the file as the corpus names it (under durable preparation
/// `path` is a snapshot blob); it titles a page with no `.TH`.
pub fn extract(path: &Path, name: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, MAN_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    emit(parse(&text), name, sink, &mut stats);
    Ok(stats)
}

/// Whether a text prefix is a man(7) page: its first request, after comments
/// and the definitions a generator's preamble makes (and calls of macros it
/// defined: grep(1) sets its date with its own `.dT`), is `.TH`. mdoc pages
/// (`.Dd`), `.so` stubs and anything with text first are not.
pub(crate) fn looks_like_man(prefix: &str) -> bool {
    let mut in_definition = false;
    let mut defined: Vec<&str> = Vec::new();
    for line in prefix.lines() {
        if in_definition {
            in_definition = line.trim_end() != "..";
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix(['.', '\'']) else {
            return false;
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() || rest.starts_with('\\') {
            // A comment (`.\"`, `.\#`) or a brace (`.\}`).
            continue;
        }
        let (name, args) = request_name(rest);
        match name {
            "TH" => return true,
            "de" | "de1" | "am" | "ig" => {
                in_definition = true;
                defined.extend(args.split_whitespace().next());
            }
            "ds" | "ds1" | "as" | "nr" | "rr" | "rm" | "if" | "ie" | "el" | "br" | "ad" | "na"
            | "nh" | "hy" | "ss" | "ll" | "tr" | "ft" | "IX" => {}
            m if defined.contains(&m) => {}
            _ => return false,
        }
    }
    false
}

/// Split a control line (after its `.`) into the request name and the rest.
/// A name ends at whitespace or a backslash: `el\{` is `el` + `\{`.
fn request_name(rest: &str) -> (&str, &str) {
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '\\')
        .unwrap_or(rest.len());
    rest.split_at(end)
}

#[derive(Debug, Default)]
struct Section {
    heading: String,
    blocks: Vec<String>,
}

#[derive(Debug, Default)]
struct Page {
    name: Option<String>,
    section: Option<String>,
    sections: Vec<Section>,
    truncated: bool,
}

impl Page {
    /// The summary after the dash in the NAME section: `ls \- list directory
    /// contents` → `list directory contents`.
    fn description(&self) -> Option<String> {
        let name = self
            .sections
            .iter()
            .find(|s| s.heading.trim().eq_ignore_ascii_case("NAME"))?;
        let text = name.blocks.join(" ");
        let chars: Vec<char> = text.chars().collect();
        let at = (1..chars.len().saturating_sub(1)).find(|&i| {
            matches!(chars[i], '-' | '–' | '—')
                && chars[i - 1].is_whitespace()
                && chars[i + 1].is_whitespace()
        })?;
        let desc: String = chars[at + 1..].iter().collect();
        let desc = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        (!desc.is_empty()).then_some(desc)
    }
}

fn emit(page: Page, name: &Path, sink: Sink, stats: &mut ExtractStats) {
    if page.truncated {
        stats.truncated = true;
    }
    let title = match (&page.name, &page.section) {
        (Some(n), Some(s)) => format!("{n}({s})"),
        (Some(n), None) => n.clone(),
        _ => {
            let file = name
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            match file.strip_suffix(".gz").unwrap_or(&file) {
                "" => "untitled".to_string(),
                f => f.to_string(),
            }
        }
    };
    let description = page.description();
    let mut emitted = false;
    for (k, sec) in page.sections.iter().enumerate() {
        let body = sec.blocks.join("\n\n");
        if body.trim().is_empty() {
            continue;
        }
        for (j, text) in split_sections(&body).into_iter().enumerate() {
            if stats.records as usize >= MAX_RECORDS_PER_FILE {
                stats.truncated = true;
                return;
            }
            let mut fields = Map::new();
            fields.insert("title".into(), Value::String(title.clone()));
            if let Some(s) = &page.section {
                fields.insert("man_section".into(), Value::String(s.clone()));
            }
            if !sec.heading.is_empty() {
                fields.insert("heading".into(), Value::String(sec.heading.clone()));
            }
            if let Some(d) = &description {
                fields.insert("description".into(), Value::String(d.clone()));
            }
            if j > 0 {
                fields.insert("section".into(), Value::Number((j as u64).into()));
            }
            fields.insert("body".into(), Value::String(text));
            stats.records += 1;
            emitted = true;
            if !sink(RawRecord {
                fields,
                locator: format!("sh{k}-s{j}"),
                group: None,
                // title/man_section/heading/description/section/body are this
                // extractor's vocabulary; all but title/body are sometimes
                // absent.
                origin: FieldOrigin::Extractor,
            }) {
                return;
            }
        }
    }
    if !emitted {
        stats.junk += 1;
    }
}

fn parse(text: &str) -> Page {
    let mut p = Parser {
        interpolations: Cell::new(MAX_INTERPOLATIONS),
        ..Parser::default()
    };
    for line in logical_lines(text) {
        p.line(&line);
        if p.page.truncated {
            break;
        }
    }
    p.finish();
    p.page
}

/// Input lines with comments removed and continuation lines joined. A line
/// that held only a comment is dropped, so it does not end a paragraph the
/// way a blank line does.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let (mut line, commented, mut join) = strip_comment(raw);
        if !join {
            let trailing = line.chars().rev().take_while(|&c| c == '\\').count();
            if trailing % 2 == 1 {
                line.pop();
                join = true;
            }
        }
        if commented && matches!(line.trim(), "" | "." | "'") {
            continue;
        }
        pending.push_str(&line);
        if !join {
            out.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        out.push(pending);
    }
    out
}

/// `(line without its comment, whether it had one, whether it joins the
/// next line)`. `\"` comments to the end of the line; `\#` does too and also
/// joins the next line.
fn strip_comment(raw: &str) -> (String, bool, bool) {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => return (out, true, false),
            Some('#') => return (out, true, true),
            Some(n) => {
                out.push('\\');
                out.push(n);
            }
            None => out.push('\\'),
        }
    }
    (out, false, false)
}

/// Split request arguments on spaces and tabs. `"..."` quotes one argument
/// (`""` inside it is a literal `"`); escapes are kept as written, so `\ `
/// does not split.
fn split_args(s: &str) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    loop {
        while i < c.len() && (c[i] == ' ' || c[i] == '\t') {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let mut arg = String::new();
        if c[i] == '"' {
            i += 1;
            while i < c.len() {
                match c[i] {
                    '"' if c.get(i + 1) == Some(&'"') => {
                        arg.push('"');
                        i += 2;
                    }
                    '"' => {
                        i += 1;
                        break;
                    }
                    '\\' => {
                        arg.extend(c.get(i..i + 2).unwrap_or(&c[i..]));
                        i += 2;
                    }
                    ch => {
                        arg.push(ch);
                        i += 1;
                    }
                }
            }
        } else {
            while i < c.len() && c[i] != ' ' && c[i] != '\t' {
                if c[i] == '\\' {
                    arg.extend(c.get(i..i + 2).unwrap_or(&c[i..]));
                    i += 2;
                } else {
                    arg.push(c[i]);
                    i += 1;
                }
            }
        }
        args.push(arg);
    }
    args
}

/// Whether `line` is the request `name` (`.TE`, `..`): a control character,
/// optional blanks, the name, then nothing or whitespace.
fn is_request(line: &str, name: &str) -> bool {
    line.strip_prefix(['.', '\''])
        .map(|r| r.trim_start_matches([' ', '\t']))
        .and_then(|r| r.strip_prefix(name))
        .is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
}

/// Evaluate the condition that opens an `.if`/`.ie` body; returns the result
/// and the rest of the body. Output-device tests answer as nroff (`n` true,
/// `t` false); `\n(.g` (running under groff) is true; "is defined" tests
/// (`d`, `r`, …) are true, so a page's fallback definitions are skipped.
/// Anything else — numeric expressions, string comparisons — is false.
fn condition(s: &str) -> (bool, String) {
    let s = s.trim_start();
    let (neg, s) = match s.strip_prefix('!') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let c: Vec<char> = s.chars().collect();
    let token_end = |from: usize| {
        (from..c.len())
            .find(|&i| c[i] == ' ' || c[i] == '\t')
            .unwrap_or(c.len())
    };
    let blank = |ch: Option<&char>| ch.is_none_or(|ch| *ch == ' ' || *ch == '\t');
    let (value, used) = match c.first() {
        Some(&f)
            if "ntoev".contains(f)
                && !c
                    .get(1)
                    .is_some_and(|n| n.is_ascii_alphanumeric() || *n == '(' || *n == '[') =>
        {
            (f == 'n' || f == 'o', 1)
        }
        _ if s.starts_with("\\n(.g") && blank(c.get(5)) => (true, 5),
        _ if s.starts_with("\\n[.g]") && blank(c.get(6)) => (true, 6),
        Some(&f) if "drmcFS".contains(f) && c.get(1).is_some_and(|w| *w == ' ' || *w == '\t') => {
            let start = (1..c.len())
                .find(|&i| c[i] != ' ' && c[i] != '\t')
                .unwrap_or(c.len());
            (true, token_end(start))
        }
        Some(&d) if d == '\'' || d == '"' => {
            // 'a'b' — a string comparison, read up to its third delimiter.
            let third = c
                .iter()
                .enumerate()
                .filter(|(_, ch)| **ch == d)
                .nth(2)
                .map_or(c.len(), |(i, _)| i + 1);
            (false, third)
        }
        _ => (false, token_end(0)),
    };
    (value != neg, c[used..].iter().collect())
}

#[derive(Debug, Clone, Copy)]
enum Capture {
    /// `.SH`/`.SS` with no arguments: the next line is the heading.
    Heading(u8),
    /// `.TP`: the next line is the item's term.
    Term,
    /// `.TQ`: another term for the same item.
    MoreTerm,
}

#[derive(Default)]
struct Parser {
    page: Page,
    strings: HashMap<String, String>,
    /// Macros the page defines with `.de`.
    defined: HashSet<String>,
    para: String,
    /// The last text ended in `\c`: the next joins it with no space.
    glue: bool,
    nofill: bool,
    code: Vec<String>,
    capture: Option<Capture>,
    capture_text: String,
    /// `.UR`/`.MT` target, and the paragraph length when it opened.
    link: Option<(String, usize)>,
    /// Inside `.de`/`.ig`: the request that ends it.
    skip_until: Option<String>,
    /// Inside a false `\{ … \}` block: its brace depth.
    skip_depth: i64,
    last_condition: bool,
    table: Option<Vec<String>>,
    produced: usize,
    interpolations: Cell<u32>,
}

impl Parser {
    fn line(&mut self, line: &str) {
        if let Some(end) = &self.skip_until {
            if is_request(line, end) {
                self.skip_until = None;
            }
            return;
        }
        if self.skip_depth > 0 {
            self.skip_depth = (self.skip_depth + braces(line, "\\{") - braces(line, "\\}")).max(0);
            return;
        }
        if let Some(rows) = self.table.as_mut() {
            if is_request(line, "TE") {
                let rows = std::mem::take(rows);
                self.table = None;
                self.table_block(rows);
            } else {
                rows.push(line.to_string());
            }
            return;
        }
        match line.strip_prefix(['.', '\'']) {
            Some(rest) => self.request(rest),
            None => self.text_line(line),
        }
    }

    fn request(&mut self, rest: &str) {
        let (name, args) = request_name(rest.trim_start_matches([' ', '\t']));
        let a = || split_args(args);
        match name {
            "" => {}
            "TH" => {
                let a = a();
                let field = |i: usize| {
                    a.get(i)
                        .map(|s| collapse(&self.plain(s)))
                        .filter(|s| !s.is_empty())
                };
                let (name, section) = (field(0), field(1));
                self.page.name = name;
                self.page.section = section;
            }
            "SH" | "SS" => {
                let level = if name == "SH" { 1 } else { 2 };
                self.close_blocks();
                let text = a().join(" ");
                if text.trim().is_empty() {
                    self.capture = Some(Capture::Heading(level));
                } else {
                    let t = collapse(&self.plain(&text));
                    self.heading(level, &t);
                }
            }
            "PP" | "LP" | "P" | "HP" | "RS" | "RE" => self.close_blocks(),
            "TP" => {
                self.close_blocks();
                self.capture = Some(Capture::Term);
            }
            "TQ" => self.capture = Some(Capture::MoreTerm),
            "IP" => {
                self.close_blocks();
                let tag = a()
                    .first()
                    .map(|t| collapse(&self.plain(t)))
                    .unwrap_or_default();
                let numbered = tag.trim_start_matches('(').trim_end_matches(['.', ')']);
                if tag.is_empty() {
                } else if matches!(
                    tag.as_str(),
                    "•" | "·" | "*" | "-" | "+" | "o" | "–" | "—" | "○" | "◦" | "‣"
                ) {
                    self.para = "• ".into();
                    self.glue = true;
                } else if !numbered.is_empty() && numbered.bytes().all(|b| b.is_ascii_digit()) {
                    self.para = format!("{tag} ");
                    self.glue = true;
                } else {
                    self.para = format!("{tag}\n");
                }
            }
            "B" | "I" | "SB" | "SM" => {
                let t = a().iter().map(|s| self.plain(s)).collect::<Vec<_>>();
                self.macro_text(&t.join(" "));
            }
            "BI" | "BR" | "IB" | "IR" | "RB" | "RI" => {
                let t: String = a().iter().map(|s| self.plain(s)).collect();
                self.macro_text(&t);
            }
            "UR" | "MT" => {
                let target = a().first().map(|s| self.plain(s)).unwrap_or_default();
                self.link = Some((target, self.para.len()));
            }
            "UE" | "ME" => {
                if let Some((target, start)) = self.link.take() {
                    if self.para.len() <= start {
                        self.text(&target, false);
                    } else if !target.is_empty() {
                        self.text(&format!("<{target}>"), false);
                    }
                }
                // Punctuation after a link attaches to it: `.UE .`
                let trail: String = a().iter().map(|s| self.plain(s)).collect();
                if !trail.trim().is_empty() {
                    self.glue = true;
                    self.text(&trail, false);
                }
            }
            "SY" => {
                self.close_blocks();
                let t = a().iter().map(|s| self.plain(s)).collect::<Vec<_>>();
                self.text(&t.join(" "), false);
            }
            "OP" => {
                let t = a().iter().map(|s| self.plain(s)).collect::<Vec<_>>();
                self.text(&format!("[{}]", t.join(" ")), false);
            }
            "MR" => {
                let a: Vec<String> = a().iter().map(|s| self.plain(s)).collect();
                if let Some(page) = a.first() {
                    let sect = a.get(1).map(|s| format!("({s})")).unwrap_or_default();
                    let trail = a.get(2).cloned().unwrap_or_default();
                    self.text(&format!("{page}{sect}{trail}"), false);
                }
            }
            "br" => {
                if !self.nofill
                    && self.capture.is_none()
                    && !self.para.is_empty()
                    && !self.para.ends_with('\n')
                {
                    self.para.push('\n');
                }
            }
            "sp" | "Sp" => {
                if self.nofill {
                    self.code.push(String::new());
                } else {
                    self.flush_para();
                }
            }
            "nf" | "EX" | "Vb" => {
                self.flush_para();
                self.nofill = true;
            }
            "fi" | "EE" | "Ve" => {
                self.flush_code();
                self.nofill = false;
            }
            "TS" => {
                self.flush_para();
                self.table = Some(Vec::new());
            }
            "de" | "de1" | "am" => {
                let a = a();
                if let Some(m) = a.first() {
                    self.defined.insert(m.clone());
                }
                self.skip_until = Some(a.get(1).cloned().unwrap_or_else(|| ".".into()));
            }
            "ig" => {
                self.skip_until = Some(a().first().cloned().unwrap_or_else(|| ".".into()));
            }
            "ds" | "ds1" | "as" => {
                let rest = args.trim_start_matches([' ', '\t']);
                let (key, value) = match rest.find([' ', '\t']) {
                    Some(i) => (&rest[..i], &rest[i + 1..]),
                    None => (rest, ""),
                };
                if !key.is_empty() {
                    let value = value.strip_prefix('"').unwrap_or(value);
                    let entry = self.strings.entry(key.to_string()).or_default();
                    if name != "as" {
                        entry.clear();
                    }
                    entry.push_str(value);
                }
            }
            "if" | "ie" => {
                let (c, body) = condition(args);
                if name == "ie" {
                    self.last_condition = c;
                }
                self.branch(c, &body);
            }
            "el" => self.branch(!self.last_condition, args),
            // Before the first section it is preamble (grep's `.dT` date).
            m if !IGNORED.contains(&m)
                && self.defined.contains(m)
                && !self.page.sections.is_empty() =>
            {
                let t = a().iter().map(|s| self.plain(s)).collect::<Vec<_>>();
                self.macro_text(&t.join(" "));
            }
            _ => {}
        }
    }

    fn branch(&mut self, cond: bool, body: &str) {
        let body = body.trim_start();
        let (braced, body) = match body.strip_prefix("\\{") {
            Some(r) => (true, r),
            None => (false, body),
        };
        if cond {
            let b = body.trim_start();
            if !b.is_empty() {
                self.line(b);
            }
        } else if braced {
            self.skip_depth = (1 + braces(body, "\\{") - braces(body, "\\}")).max(0);
        }
    }

    fn text_line(&mut self, line: &str) {
        if self.nofill {
            let (s, _) = self.expand(line);
            self.code.push(s.trim_end().to_string());
            return;
        }
        if line.trim().is_empty() {
            if self.capture.is_none() {
                self.flush_para();
            }
            return;
        }
        if line.starts_with([' ', '\t'])
            && self.capture.is_none()
            && !self.para.is_empty()
            && !self.para.ends_with('\n')
        {
            self.para.push('\n');
        }
        let (s, cont) = self.expand(line);
        // Text that ends in a space before its `\c` keeps that space.
        let cont = cont && !s.ends_with(char::is_whitespace);
        self.text(&collapse(&s), cont);
    }

    /// Text from a font macro: a line of its own in a no-fill block.
    fn macro_text(&mut self, t: &str) {
        if self.nofill {
            self.code.push(t.to_string());
        } else {
            self.text(&collapse(t), false);
        }
    }

    /// Add filled text to the paragraph, or to a pending capture. `cont`: it
    /// ended in `\c`, so the next text joins it with no space.
    fn text(&mut self, t: &str, cont: bool) {
        let t = t.trim();
        if let Some(cap) = self.capture {
            if !t.is_empty() {
                if !self.capture_text.is_empty() && !self.glue {
                    self.capture_text.push(' ');
                }
                self.capture_text.push_str(t);
            }
            self.glue = cont;
            if cont || self.capture_text.is_empty() {
                return;
            }
            self.capture = None;
            let done = std::mem::take(&mut self.capture_text);
            match cap {
                Capture::Heading(level) => self.heading(level, &done),
                Capture::Term => self.para = format!("{done}\n"),
                Capture::MoreTerm => {
                    if self.para.ends_with('\n') {
                        self.para.pop();
                        self.para.push_str(", ");
                    }
                    self.para.push_str(&done);
                    self.para.push('\n');
                }
            }
            return;
        }
        if t.is_empty() {
            return;
        }
        if !self.para.is_empty() && !self.glue && !self.para.ends_with(['\n', ' ']) {
            self.para.push(' ');
        }
        self.para.push_str(t);
        self.glue = cont;
    }

    fn heading(&mut self, level: u8, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.close_blocks();
        if level == 1 {
            self.page.sections.push(Section {
                heading: text.to_string(),
                blocks: Vec::new(),
            });
        } else {
            self.push_block(text.to_string());
        }
    }

    /// End the paragraph and any no-fill block.
    fn close_blocks(&mut self) {
        self.flush_para();
        self.flush_code();
        self.nofill = false;
    }

    fn flush_para(&mut self) {
        let p = std::mem::take(&mut self.para);
        self.glue = false;
        if let Some((_, start)) = self.link.as_mut() {
            *start = 0;
        }
        let p = p.trim();
        if !p.is_empty() {
            self.push_block(p.to_string());
        }
    }

    fn flush_code(&mut self) {
        let lines = std::mem::take(&mut self.code);
        let first = lines.iter().position(|l| !l.trim().is_empty());
        let last = lines.iter().rposition(|l| !l.trim().is_empty());
        if let (Some(a), Some(b)) = (first, last) {
            self.push_block(lines[a..=b].join("\n"));
        }
    }

    fn push_block(&mut self, block: String) {
        self.produced += block.len();
        if self.produced > MAX_PAGE_TEXT {
            self.page.truncated = true;
            return;
        }
        if self.page.sections.is_empty() {
            self.page.sections.push(Section::default());
        }
        if let Some(s) = self.page.sections.last_mut() {
            s.blocks.push(block);
        }
    }

    fn finish(&mut self) {
        if self.capture.take().is_some() {
            let done = std::mem::take(&mut self.capture_text);
            self.text(&done, false);
        }
        if let Some(rows) = self.table.take() {
            self.table_block(rows);
        }
        self.close_blocks();
    }

    /// A tbl table (the lines between `.TS` and `.TE`) as one block, one line
    /// per row with cells joined by ` | `.
    fn table_block(&mut self, lines: Vec<String>) {
        let mut tab = '\t';
        let mut i = 0;
        if let Some(opts) = lines.first().filter(|l| l.trim_end().ends_with(';')) {
            if let Some(t) = opts.find("tab").and_then(|at| {
                let rest = opts[at + 3..].trim_start().strip_prefix('(')?;
                rest.chars().next()
            }) {
                tab = t;
            }
            i = 1;
        }
        // Format lines, up to the one ending in `.`.
        while i < lines.len() {
            let done = lines[i].trim_end().ends_with('.');
            i += 1;
            if done {
                break;
            }
        }
        let mut rows = Vec::new();
        while i < lines.len() {
            let line = &lines[i];
            i += 1;
            let t = line.trim();
            if t == ".T&" {
                // A new format section, again ended by a line ending in `.`.
                while i < lines.len() {
                    let done = lines[i].trim_end().ends_with('.');
                    i += 1;
                    if done {
                        break;
                    }
                }
                continue;
            }
            if t.is_empty() || t == "_" || t == "=" || line.starts_with(['.', '\'']) {
                continue;
            }
            let mut fields: VecDeque<String> = line.split(tab).map(str::to_string).collect();
            let mut cells = Vec::new();
            while let Some(f) = fields.pop_front() {
                if f.trim() == "T{" {
                    // A text block: the lines up to `T}`, whose rest of line
                    // carries on with the row's next fields.
                    let mut words = Vec::new();
                    while i < lines.len() {
                        let l = &lines[i];
                        i += 1;
                        if let Some(rest) = l.strip_prefix("T}") {
                            fields.extend(rest.split(tab).skip(1).map(str::to_string));
                            break;
                        }
                        if !l.starts_with(['.', '\'']) {
                            words.push(collapse(&self.plain(l)));
                        }
                    }
                    cells.push(words.join(" ").trim().to_string());
                } else {
                    let f = f.trim();
                    cells.push(if matches!(f, "_" | "=" | "\\^") {
                        String::new()
                    } else {
                        collapse(&self.plain(f))
                    });
                }
            }
            while cells.last().is_some_and(|c| c.is_empty()) {
                cells.pop();
            }
            if !cells.is_empty() {
                rows.push(cells.join(" | "));
            }
        }
        if !rows.is_empty() {
            self.push_block(rows.join("\n"));
        }
    }

    fn plain(&self, s: &str) -> String {
        self.expand(s).0
    }

    /// Expand escapes; also returns whether the text ends in `\c`.
    fn expand(&self, s: &str) -> (String, bool) {
        let mut out = String::with_capacity(s.len());
        let cont = self.expand_into(s, &mut out, 0);
        (out, cont)
    }

    fn expand_into(&self, s: &str, out: &mut String, depth: u8) -> bool {
        let c: Vec<char> = s.chars().collect();
        let mut i = 0;
        let mut cont = false;
        while i < c.len() {
            if c[i] != '\\' {
                out.push(c[i]);
                i += 1;
                continue;
            }
            let Some(&e) = c.get(i + 1) else { break };
            i += 2;
            match e {
                'f' => {
                    read_name(&c, &mut i);
                }
                '(' => {
                    let n: String = c[i..(i + 2).min(c.len())].iter().collect();
                    i = (i + 2).min(c.len());
                    out.push_str(&special(&n));
                }
                '[' => {
                    i -= 1;
                    out.push_str(&special(&read_name(&c, &mut i)));
                }
                '*' => {
                    let name = read_name(&c, &mut i);
                    let name = name.split_whitespace().next().unwrap_or("");
                    self.interpolate(name, out, depth);
                }
                '-' => out.push('-'),
                'e' | '\\' => out.push('\\'),
                '.' => out.push('.'),
                ' ' | '~' | '0' => out.push(' '),
                't' => out.push('\t'),
                '\'' => out.push('\''),
                '`' => out.push('`'),
                'c' => cont = i >= c.len(),
                's' | 'S' => skip_size(&c, &mut i),
                'N' => {
                    let n = delimited(&c, &mut i);
                    if let Some(ch) = n.parse::<u32>().ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                // A forward move separates words: DocBook numbers its list
                // items `1.\h'+01'\c` and joins the item's text on.
                'h' => {
                    let arg = delimited(&c, &mut i);
                    let arg = arg.trim().trim_start_matches('+');
                    if !arg.starts_with('-') && !arg.trim_start_matches(['0', '.']).is_empty() {
                        out.push(' ');
                    }
                }
                'v' | 'w' | 'o' | 'X' | 'Z' | 'b' | 'B' | 'l' | 'L' | 'D' | 'R' | 'A' | 'x' => {
                    delimited(&c, &mut i);
                }
                'n' => {
                    if matches!(c.get(i), Some('+' | '-')) {
                        i += 1;
                    }
                    read_name(&c, &mut i);
                }
                'm' | 'M' | 'F' | 'Y' | 'V' | 'g' | 'k' | '$' => {
                    read_name(&c, &mut i);
                }
                'z' | '&' | '|' | '^' | ',' | '/' | ':' | '%' | ')' | '{' | '}' | '!' | 'p'
                | 'd' | 'u' | 'r' | 'a' => {}
                other => out.push(other),
            }
        }
        cont
    }

    fn interpolate(&self, name: &str, out: &mut String, depth: u8) {
        let left = self.interpolations.get();
        if left == 0 {
            return;
        }
        self.interpolations.set(left - 1);
        let value = match self.strings.get(name) {
            Some(v) => v.as_str(),
            None => match name {
                "R" => "®",
                "Tm" => "™",
                "lq" => "“",
                "rq" => "”",
                "Aq" => "'",
                _ => "",
            },
        };
        if value.contains('\\') && depth < MAX_STRING_DEPTH {
            self.expand_into(value, out, depth + 1);
        } else {
            out.push_str(value);
        }
    }
}

/// Requests and macros with no effect on the text.
const IGNORED: &[&str] = &[
    "PD",
    "ad",
    "na",
    "hy",
    "nh",
    "ne",
    "in",
    "ti",
    "ll",
    "ps",
    "vs",
    "ta",
    "bp",
    "ns",
    "rs",
    "nr",
    "rr",
    "rm",
    "rn",
    "tr",
    "cc",
    "c2",
    "ec",
    "eo",
    "lf",
    "mso",
    "do",
    "pc",
    "lg",
    "ss",
    "cs",
    "fam",
    "ev",
    "DT",
    "UC",
    "AT",
    "IX",
    "ul",
    "cu",
    "ce",
    "pl",
    "po",
    "pn",
    "nm",
    "nn",
    "mk",
    "rt",
    "fl",
    "hc",
    "hw",
    "hla",
    "hlm",
    "hym",
    "hys",
    "char",
    "fchar",
    "schar",
    "tm",
    "ab",
    "so",
    "blm",
    "lsm",
    "warn",
    "cp",
    "sy",
    "pso",
    "open",
    "close",
    "write",
    "ch",
    "wh",
    "it",
    "itc",
    "em",
    "kern",
    "ftr",
    "fspecial",
    "special",
    "sizes",
    "fp",
    "fzoom",
    "pm",
    "pev",
    "pnr",
    "ptr",
    "als",
    "aln",
    "chop",
    "substring",
    "length",
    "BT",
    "PT",
    "YS",
    "ft",
];

fn braces(s: &str, which: &str) -> i64 {
    s.matches(which).count() as i64
}

/// Whitespace runs to one space, trimmed.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An escape's name argument at `c[*i]`: one character, `(xx`, or `[name]`.
fn read_name(c: &[char], i: &mut usize) -> String {
    match c.get(*i) {
        Some('(') => {
            let end = (*i + 3).min(c.len());
            let n = c[*i + 1..end].iter().collect();
            *i = end;
            n
        }
        Some('[') => {
            let close = (*i + 1..c.len()).find(|&j| c[j] == ']');
            let n = c[*i + 1..close.unwrap_or(c.len())].iter().collect();
            *i = close.map_or(c.len(), |j| j + 1);
            n
        }
        Some(&ch) => {
            *i += 1;
            ch.to_string()
        }
        None => String::new(),
    }
}

/// A delimited escape argument (`\h'2n'`, `\w[...]`). Unterminated, it runs
/// to the end of the text.
fn delimited(c: &[char], i: &mut usize) -> String {
    let Some(&open) = c.get(*i) else {
        return String::new();
    };
    let close = if open == '[' { ']' } else { open };
    let start = *i + 1;
    let end = (start..c.len()).find(|&j| c[j] == close);
    let arg = c[start.min(c.len())..end.unwrap_or(c.len())]
        .iter()
        .collect();
    *i = end.map_or(c.len(), |j| j + 1);
    arg
}

/// Skip a size argument: `\s-1`, `\s+2`, `\s0`, `\s12`, `\s(12`, `\s[12]`,
/// `\s'12'`.
fn skip_size(c: &[char], i: &mut usize) {
    if matches!(c.get(*i), Some('+' | '-')) {
        *i += 1;
    }
    match c.get(*i) {
        Some('(') => *i = (*i + 3).min(c.len()),
        Some('[') | Some('\'') => {
            delimited(c, i);
        }
        Some(&d) if d.is_ascii_digit() => {
            *i += 1;
            if ('1'..='3').contains(&d) && c.get(*i).is_some_and(|n| n.is_ascii_digit()) {
                *i += 1;
            }
        }
        _ => {}
    }
}

/// The character a special-character name stands for (`\(em`, `\[bu]`,
/// `\[u00E9]`); empty when unknown.
fn special(name: &str) -> String {
    let s = match name {
        "em" => "—",
        "en" => "–",
        "hy" | "mi" => "-",
        "pl" => "+",
        "mu" => "×",
        "di" => "÷",
        "+-" => "±",
        "eq" => "=",
        "==" => "≡",
        ">=" => "≥",
        "<=" => "≤",
        "!=" => "≠",
        "->" => "→",
        "<-" => "←",
        "<>" => "↔",
        "rA" => "⇒",
        "lA" => "⇐",
        "ua" => "↑",
        "da" => "↓",
        "bu" => "•",
        "ci" => "○",
        "sq" => "□",
        "lq" => "“",
        "rq" => "”",
        "oq" => "‘",
        "cq" => "’",
        "aq" => "'",
        "dq" => "\"",
        "Fo" => "«",
        "Fc" => "»",
        "fo" => "‹",
        "fc" => "›",
        "ga" => "`",
        "aa" => "´",
        "ti" | "a~" => "~",
        "ha" | "a^" => "^",
        "rs" => "\\",
        "sl" => "/",
        "ba" | "or" => "|",
        "br" => "│",
        "rn" => "‾",
        "ul" => "_",
        "co" => "©",
        "rg" => "®",
        "tm" => "™",
        "de" => "°",
        "ss" => "ß",
        "sc" => "§",
        "ps" => "¶",
        "dg" => "†",
        "dd" => "‡",
        "ct" => "¢",
        "Po" => "£",
        "Eu" | "eu" => "€",
        "Ye" => "¥",
        "at" => "@",
        "sh" => "#",
        "Do" => "$",
        "lB" => "[",
        "rB" => "]",
        "lC" => "{",
        "rC" => "}",
        "la" => "⟨",
        "ra" => "⟩",
        "%0" => "‰",
        "12" => "½",
        "14" => "¼",
        "34" => "¾",
        "if" => "∞",
        _ => "",
    };
    if !s.is_empty() {
        return s.to_string();
    }
    let code = |hex: &str, radix| {
        u32::from_str_radix(hex, radix)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
    };
    if let Some(hex) = name.strip_prefix('u') {
        let hex = hex.split('_').next().unwrap_or("");
        if (4..=6).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return code(hex, 16).unwrap_or_default();
        }
    }
    if let Some(dec) = name.strip_prefix("char") {
        if !dec.is_empty() && dec.bytes().all(|b| b.is_ascii_digit()) {
            return code(dec, 10).unwrap_or_default();
        }
    }
    // An accented letter (`'e`, `:u`) reads as its letter.
    let mut it = name.chars();
    if let (Some(a), Some(l), None) = (it.next(), it.next(), it.next()) {
        if "'`^~:,".contains(a) && l.is_ascii_alphabetic() {
            return l.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(src: &str) -> (Vec<RawRecord>, ExtractStats) {
        let mut out = Vec::new();
        let mut stats = ExtractStats::default();
        emit(
            parse(src),
            Path::new("man1/frob.1.gz"),
            &mut |r| {
                out.push(r);
                true
            },
            &mut stats,
        );
        (out, stats)
    }

    fn field<'a>(r: &'a RawRecord, k: &str) -> &'a Value {
        static NULL: Value = Value::Null;
        r.fields.get(k).unwrap_or(&NULL)
    }

    /// The text of the first section headed `heading`.
    fn body(src: &str, heading: &str) -> String {
        let page = parse(src);
        page.sections
            .iter()
            .find(|s| s.heading == heading)
            .map(|s| s.blocks.join("\n\n"))
            .unwrap_or_else(|| panic!("no section {heading}: {page:?}"))
    }

    const LS: &str = r#".\" DO NOT MODIFY THIS FILE!  It was generated by help2man 1.50.1.
.TH LS "1" "April 2024" "GNU coreutils 9.4" "User Commands"
.SH NAME
ls \- list directory contents
.SH SYNOPSIS
.B ls
[\fI\,OPTION\/\fR]... [\fI\,FILE\/\fR]...
.SH DESCRIPTION
.\" Add any additional description here
.PP
List information about the FILEs (the current directory by default).
Sort entries alphabetically if none of \fB\-cftuvSUX\fR nor \fB\-\-sort\fR is specified.
.TP
\fB\-a\fR, \fB\-\-all\fR
do not ignore entries starting with .
.TP
\fB\-A\fR, \fB\-\-almost\-all\fR
do not list implied . and ..
.SS "Exit status:"
.TP
0
if OK,
.SH "SEE ALSO"
Full documentation <https://www.gnu.org/software/coreutils/ls>
"#;

    #[test]
    fn each_section_is_a_record_titled_from_th_with_the_name_line_as_description() {
        let (recs, stats) = records(LS);
        assert_eq!((stats.records, stats.junk, stats.truncated), (4, 0, false));
        let headings: Vec<&Value> = recs.iter().map(|r| field(r, "heading")).collect();
        assert_eq!(
            headings,
            [
                &json("NAME"),
                &json("SYNOPSIS"),
                &json("DESCRIPTION"),
                &json("SEE ALSO")
            ]
        );
        for r in &recs {
            assert_eq!(*field(r, "title"), "LS(1)");
            assert_eq!(*field(r, "man_section"), "1");
            assert_eq!(*field(r, "description"), "list directory contents");
            assert_eq!(r.origin, FieldOrigin::Extractor);
            assert!(field(r, "section").is_null());
        }
        assert_eq!(recs[0].locator, "sh0-s0");
        assert_eq!(recs[3].locator, "sh3-s0");
        assert_eq!(*field(&recs[1], "body"), "ls [OPTION]... [FILE]...");
        assert_eq!(
            *field(&recs[2], "body"),
            "List information about the FILEs (the current directory by default). \
             Sort entries alphabetically if none of -cftuvSUX nor --sort is specified.\n\n\
             -a, --all\ndo not ignore entries starting with .\n\n\
             -A, --almost-all\ndo not list implied . and ..\n\n\
             Exit status:\n\n\
             0\nif OK,"
        );
    }

    fn json(s: &str) -> Value {
        Value::String(s.into())
    }

    #[test]
    fn a_page_without_th_is_titled_from_its_file_name() {
        let (recs, _) = records(".SH NAME\nfrob \\- frobnicate\n");
        assert_eq!(*field(&recs[0], "title"), "frob.1");
        assert!(field(&recs[0], "man_section").is_null());
        assert_eq!(*field(&recs[0], "description"), "frobnicate");
    }

    #[test]
    fn a_page_with_no_text_is_junk() {
        let (recs, stats) = records(".\\\" only a comment\n.TH X 1\n.SH NAME\n");
        assert!(recs.is_empty());
        assert_eq!(stats.junk, 1);
    }

    #[test]
    fn escapes_become_the_characters_they_name() {
        let src = ".SH X\n\
            a\\-b \\(em \\[bu] \\e \\&.dot \\(lqq\\(rq \\*(lqs\\*(rq it\\(aqs \\[u00E9] x\\ y\n\
            \\s-1SMALL\\s0 \\h'2n'x\\v'-1'y\\w'abc'z \\N'65' \\[char66] \\['e] \\(*a \\[u110000]\n";
        assert_eq!(
            body(src, "X"),
            "a-b — • \\ .dot “q” “s” it's é x y SMALL xyz A B e"
        );
    }

    #[test]
    fn fonts_are_dropped_and_alternating_macros_join_without_spaces() {
        let src = ".SH X\n\
            Use \\fBbold\\fR and \\fIitalic\\fP and \\f(CWcode\\fR and \\f[B]named\\f[].\n\
            .B bold words\n\
            .BR ls (1),\n\
            .IR file .txt\n\
            \\fBfoo\\fR\\c\n\
            bar\n";
        assert_eq!(
            body(src, "X"),
            "Use bold and italic and code and named. bold words ls(1), file.txt foobar"
        );
    }

    #[test]
    fn strings_comments_and_continuations() {
        let src = ".ds Nm frob\n\
            .ds Q \"quoted\n\
            .as Nm nicate\n\
            .SH X\n\
            Run \\*(Nm, \\*Q. \\\" a comment\n\
            .\\\" a whole-line comment does not end the paragraph\n\
            more \\\n\
            joined\n";
        assert_eq!(body(src, "X"), "Run frobnicate, quoted. more joined");
    }

    /// How DocBook numbers a list item under nroff.
    #[test]
    fn a_forward_move_separates_words() {
        let src = ".SH X\n\
            .ie n \\{\\\n\\h'-04' 1.\\h'+01'\\c\n.\\}\n.el \\{\\\n.IP \"  1.\" 4.2\n.\\}\n\
            by using git-add\n";
        assert_eq!(body(src, "X"), "1. by using git-add");
    }

    #[test]
    fn paragraphs_breaks_and_indented_lines() {
        let src = ".SH X\none\ntwo\n.PP\nthree\n\nfour\n.br\nfive\n    six\n";
        assert_eq!(body(src, "X"), "one two\n\nthree\n\nfour\nfive\nsix");
    }

    #[test]
    fn option_lists_keep_each_term_above_its_description() {
        let src = ".SH OPTIONS\n\
            .TP\n.B \\-v\n.TQ\n.B \\-\\-verbose\nbe loud\n\
            .TP\n.IX Item \"-q\"\n\\fB\\-q\\fR\nbe quiet\n\
            .IP \\(bu 2\nfirst\n\
            .IP \\(bu 2\nsecond\n\
            .IP 3. 4\nthird\n\
            .IP \"\\fBFOO\\fR\" 4\nset FOO\n\
            .IP\ncontinued\n";
        assert_eq!(
            body(src, "OPTIONS"),
            "-v, --verbose\nbe loud\n\n-q\nbe quiet\n\n• first\n\n• second\n\n3. third\n\n\
             FOO\nset FOO\n\ncontinued"
        );
    }

    #[test]
    fn headings_on_the_next_line_and_subsections() {
        let src = ".SH\nEXIT STATUS\nzero\n.SS\nDetails\nmore\n";
        let page = parse(src);
        assert_eq!(page.sections.len(), 1);
        assert_eq!(page.sections[0].heading, "EXIT STATUS");
        assert_eq!(page.sections[0].blocks, ["zero", "Details", "more"]);
    }

    #[test]
    fn no_fill_blocks_keep_their_lines() {
        let src = ".SH X\n\
            .nf\n.B bold line\n  indented \\fIx\\fR\n\nlast\n.fi\nafter\n\
            .EX\n$ ls \\-l\n.EE\n\
            .Vb 1\n\\&    use Foo;\n.Ve\n";
        assert_eq!(
            body(src, "X"),
            "bold line\n  indented x\n\nlast\n\nafter\n\n$ ls -l\n\n    use Foo;"
        );
    }

    #[test]
    fn links_keep_their_target() {
        let src = ".SH X\n\
            See\n.UR https://example.com\nthe site\n.UE .\n\
            Mail\n.MT a@example.com\n.ME\n\
            .SY ls\n.OP \\-l\n.OP \\-w cols\n.YS\n\
            See\n.MR ls 1 .\n";
        assert_eq!(
            body(src, "X"),
            "See the site <https://example.com>. Mail a@example.com\n\n\
             ls [-l] [-w cols] See ls(1)."
        );
    }

    #[test]
    fn tables_become_one_line_per_row() {
        let src = ".SH X\n\
            .TS\ntab(;);\nlB cB rB\nl c r.\nName;Size;Count\n_\nfoo;\\fBbig\\fR;3\nbar;small\n\
            .T&\nl s.\nwide;T{\nlong\ntext\nT};tail\n.TE\nafter\n";
        assert_eq!(
            body(src, "X"),
            "Name | Size | Count\nfoo | big | 3\nbar | small\nwide | long text | tail\n\nafter"
        );
    }

    #[test]
    fn definitions_and_ignore_blocks_are_skipped() {
        let src = ".de XX\nhidden text\n..\n\
            .ig END\nignored\n..\nstill ignored\n.END\n\
            .SH X\nshown\n";
        assert_eq!(body(src, "X"), "shown");
    }

    /// A macro the page defines cannot be run; its arguments are its text.
    #[test]
    fn a_page_defined_macro_keeps_its_arguments() {
        // Before the first section, a call is preamble (grep's `.dT` date).
        let src = ".de FN\n\\fI\\|\\\\$1\\|\\fP\n..\n.FN preamble\n\
                   .SH FILES\n.FN ~/.bashrc\nper-shell file\n.XYZ unknown\n";
        let page = parse(src);
        assert_eq!(page.sections.len(), 1);
        assert_eq!(body(src, "FILES"), "~/.bashrc per-shell file");
    }

    #[test]
    fn conditionals_answer_as_nroff_under_groff() {
        let src = ".SH X\n\
            .if n nroff\n\
            .if t troff\n\
            .ie t \\{\\\ntroff branch\n.\\}\n\
            .el \\{\\\nelse branch\n.\\}\n\
            .if !d XX defined\n\
            .if t \\{ one-line false block \\}\n\
            after\n\
            .ie \\n(.g .ds Aq \\(aq\n.el .ds Aq '\n\
            it\\*(Aqs\n\
            .if '\\*(.T'utf8' string compare\n\
            .if \\n(.g==0 numeric\n";
        assert_eq!(body(src, "X"), "nroff else branch after it's");
    }

    /// The preamble pod2man (5.x) writes, then a small page.
    const POD2MAN: &str = r#".\" -*- mode: troff; coding: utf-8 -*-
.\" Automatically generated by Pod::Man 5.0102 (Pod::Simple 3.45)
.\"
.\" Standard preamble:
.\" ========================================================================
.de Sp \" Vertical space (when we can't use .PP)
.if t .sp .5v
.if n .sp
..
.de Vb \" Begin verbatim text
.ft CW
.nf
.ne \\$1
..
.de Ve \" End verbatim text
.ft R
.fi
..
.\" \*(C` and \*(C' are quotes in nroff, nothing in troff, for use with C<>.
.ie n \{\
.    ds C` ""
.    ds C' ""
'br\}
.el\{\
.    ds C`
.    ds C'
'br\}
.\"
.\" Escape single quotes in literal strings from groff's Unicode transform.
.ie \n(.g .ds Aq \(aq
.el       .ds Aq '
.\"
.\" If the F register is >0, we'll generate index entries on stderr for
.\" titles (.TH), headers (.SH), subsections (.SS), items (.Ip), and index
.\" entries marked with X<> in POD.  Of course, you'll have to process the
.\" output yourself in some meaningful fashion.
.\"
.\" Avoid warning from groff about undefined register 'F'.
.de IX
..
.nr rF 0
.if \n(.g .if rF .nr rF 1
.if (\n(rF:(\n(.g==0)) \{\
.    if \nF \{\
.        de IX
.        tm Index:\\$1\t\\n%\t"\\$2"
..
.        if !\nF==2 \{\
.            nr % 0
.            nr F 2
.        \}
.    \}
.\}
.rr rF
.\" ========================================================================
.\"
.IX Title "FOO 1"
.TH FOO 1 2024-01-01 "perl v5.36.0" "User Contributed Perl Documentation"
.\" For nroff, turn off justification.  Always turn off hyphenation; it makes
.\" way too many mistakes in technical documents.
.if n .ad l
.nh
.SH NAME
Foo \- frobnicate the widgets
.SH SYNOPSIS
.IX Header "SYNOPSIS"
.Vb 2
\&    use Foo;
\&    my $x = Foo\->new;
.Ve
.SH DESCRIPTION
.IX Header "DESCRIPTION"
Call \f(CW\*(C`frob\*(C'\fR to \fBfrobnicate\fR, it\*(Aqs easy.
.IP \fB\-\-verbose\fR 4
.IX Item "--verbose"
Be loud.
.IP \(bu 4
first
"#;

    #[test]
    fn a_pod2man_preamble_is_read_not_indexed() {
        assert!(looks_like_man(POD2MAN));
        let page = parse(POD2MAN);
        assert_eq!(page.name.as_deref(), Some("FOO"));
        assert_eq!(
            page.description().as_deref(),
            Some("frobnicate the widgets")
        );
        let headings: Vec<&str> = page.sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, ["NAME", "SYNOPSIS", "DESCRIPTION"]);
        assert_eq!(
            body(POD2MAN, "SYNOPSIS"),
            "    use Foo;\n    my $x = Foo->new;"
        );
        assert_eq!(
            body(POD2MAN, "DESCRIPTION"),
            "Call \"frob\" to frobnicate, it's easy.\n\n--verbose\nBe loud.\n\n• first"
        );
    }

    #[test]
    fn man_pages_are_told_from_mdoc_stubs_and_text() {
        assert!(looks_like_man(".TH LS 1\n"));
        assert!(looks_like_man(
            "'\\\" t\n.\\\" comment\n\n.TH \"GIT\" \"1\"\n"
        ));
        assert!(looks_like_man(LS));
        // grep(1): a macro the page defines, called before `.TH`; an
        // undefined one is not.
        assert!(looks_like_man(
            ".de dT\n.ds Dt \\\\$2\n..\n.dT Time-stamp: \"2019-12-29\"\n.TH GREP 1 \\*(Dt\n"
        ));
        assert!(!looks_like_man(".dT x\n.TH X 1\n"));
        // mdoc, a redirect stub, text first, a Makefile, nothing.
        assert!(!looks_like_man(
            ".Dd $Mdocdate: December 4 2024 $\n.Dt SSH 1\n"
        ));
        assert!(!looks_like_man(".so man1/other.1\n"));
        assert!(!looks_like_man("hello\n.TH X 1\n"));
        assert!(!looks_like_man(".PHONY: all\nall:\n\tcc x.c\n"));
        assert!(!looks_like_man(".\\\" only comments\n"));
        assert!(!looks_like_man(""));
    }

    /// As a system stores it: gzipped, and (under durable preparation) read
    /// from a snapshot blob, so the title must not come from the blob's name.
    #[test]
    fn a_gzipped_page_sniffs_and_extracts_end_to_end() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("00000000");
        let page = ".SH NAME\nsqlite3 \\- A command line interface for SQLite\n\
            .SH EXAMPLES\n.nf\nsqlite> CREATE TABLE t(a, b);\nsqlite> INSERT INTO t VALUES (1, 2);\n.fi\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(page.as_bytes()).unwrap();
        std::fs::write(&blob, gz.finish().unwrap()).unwrap();
        // No `.TH`: not a man page by content. With one, it is — even though
        // its examples would pass for a SQL dump.
        let sn = crate::sniff::sniff_with_name(&blob, Path::new("man1/sqlite3.1.gz")).unwrap();
        assert_ne!(sn.family, crate::sniff::Family::Man);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(format!(".TH SQLITE3 1\n{page}").as_bytes())
            .unwrap();
        std::fs::write(&blob, gz.finish().unwrap()).unwrap();
        let sn = crate::sniff::sniff_with_name(&blob, Path::new("man1/sqlite3.1.gz")).unwrap();
        assert_eq!(sn.family, crate::sniff::Family::Man);
        assert!(sn.gzip);
        let mut recs = Vec::new();
        let stats = super::super::extract(&blob, &sn, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(stats.records, 2);
        assert_eq!(*field(&recs[1], "title"), "SQLITE3(1)");
        assert_eq!(
            *field(&recs[1], "description"),
            "A command line interface for SQLite"
        );
        assert_eq!(
            *field(&recs[1], "body"),
            "sqlite> CREATE TABLE t(a, b);\nsqlite> INSERT INTO t VALUES (1, 2);"
        );
        // Without `.TH`, the logical name, not the blob, titles the page.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(page.as_bytes()).unwrap();
        std::fs::write(&blob, gz.finish().unwrap()).unwrap();
        let mut recs = Vec::new();
        extract(&blob, Path::new("man1/sqlite3.1.gz"), true, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(*field(&recs[0], "title"), "sqlite3.1");
    }

    #[test]
    fn a_long_section_is_split_and_numbered() {
        let para = "word ".repeat(100);
        let src = format!(".TH X 1\n.SH LONG\n{}", format!("{para}\n\n").repeat(20));
        let (recs, _) = records(&src);
        assert!(recs.len() > 1);
        assert!(field(&recs[0], "section").is_null());
        assert_eq!(*field(&recs[1], "section"), 1);
        assert_eq!(recs[1].locator, "sh0-s1");
        assert!(recs.iter().all(|r| *field(r, "heading") == "LONG"));
    }

    #[test]
    fn string_interpolation_is_bounded() {
        // Each string names the next twice: 2^30 copies if unbounded.
        let mut src = String::from(".ds s0 x\n");
        for i in 1..=30 {
            src.push_str(&format!(".ds s{i} \\*[s{}]\\*[s{}]\n", i - 1, i - 1));
        }
        src.push_str(".SH X\n\\*[s30]\n");
        let text = body(&src, "X");
        assert!(text.len() < 1 << 20, "{}", text.len());
    }

    #[test]
    fn unbalanced_input_does_not_lose_earlier_text() {
        // An unterminated table, link, capture and indent at end of file.
        let src = ".SH X\nbefore\n.RS\n.UR http://x\nlink\n.TS\nl.\ncell\n";
        assert_eq!(body(src, "X"), "before\n\nlink\n\ncell");
        let src = ".SH X\nbefore\n.TP\n";
        assert_eq!(body(src, "X"), "before");
    }
}
