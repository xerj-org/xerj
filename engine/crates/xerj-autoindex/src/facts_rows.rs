//! Rows to facts: table-shaped sources become XERJ Query-1 facts.
//!
//! A row is one subject. Its primary key (or a declared or inferred key
//! column) becomes `subject_id`, every other non-empty column becomes one fact
//! (`relation` = the column name in words, a typed `value`), and the row
//! itself is rendered as the source document the facts quote from, so every
//! fact carries an exact span. Counting needs `subject_id` on every counted
//! subject (docs/FACTS_API.md, "The counting cascade"); this is the path that
//! gives every row one.
//!
//! The mapping is a per-change function ([`RowFactMapper::map_change`]) so the
//! same code serves a whole-file run (`xerj autoindex <path> --facts <ns>`,
//! [`run`]) and a streamed source that sees one insert, update or delete at a
//! time (a database change feed). The caller owns the "what did I write last
//! time" state: it passes the previous document version of a row, and gets
//! back the new one to remember.
//!
//! The write contract is the node's `/_facts/{ns}/_bulk` (`documents`,
//! `facts`, `edits`). Measured on rc.101 before choosing the update shape:
//! a changed row is ONE batch of `withdraw_document` (old version) + the new
//! document + its facts; two `correct` edits sharing one new document version
//! are refused ("Document version identity cannot be overwritten"), and a
//! `correct` without its own `doc` is refused ("edit without doc"), so a
//! multi-column change cannot be expressed as per-fact supersedes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::extract::{self, RawRecord};
use crate::sniff::{self, Family};

/// Text values longer than this are not facts: a fact is one short value a
/// question can name. Long text stays searchable through the ordinary index.
pub const MAX_TEXT_VALUE_CHARS: usize = 500;

/// Default actions (documents + facts + edits) per `_bulk` request. The node
/// refuses a batch over `limits.max_actions_per_bulk` (50,000 by default) with
/// `413 facts_batch_too_large`; 5,000 keeps each commit small and the progress
/// line moving.
pub const DEFAULT_BATCH_ACTIONS: usize = 5_000;

// ───────────────────────────── the per-row mapping ─────────────────────────────

/// One change to one row, as a whole-file pass or a change feed sees it.
#[derive(Debug, Clone)]
pub enum RowChange {
    /// Insert or update. `prev_version` is the document version this caller
    /// last committed for the row (`None` for a row never written). An equal
    /// new version is a no-op.
    Upsert {
        key: String,
        columns: Map<String, Value>,
        prev_version: Option<String>,
    },
    /// The row is gone: withdraw the document version last committed for it.
    Delete { key: String, prev_version: String },
}

/// Why a cell did not become a fact. Counted, never silent.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkipCounts {
    /// NULL, missing or blank cells.
    pub empty: u64,
    /// Arrays, objects, binary: no fact kind for them.
    pub unsupported: u64,
    /// Text longer than [`MAX_TEXT_VALUE_CHARS`].
    pub long_text: u64,
}

impl SkipCounts {
    pub fn add(&mut self, o: &SkipCounts) {
        self.empty += o.empty;
        self.unsupported += o.unsupported;
        self.long_text += o.long_text;
    }
}

/// The `_bulk` actions one change produces. Append several into one
/// [`FactsBatch`] and post it; one batch is all-or-nothing on the node.
#[derive(Debug, Default, Clone)]
pub struct MappedChange {
    pub documents: Vec<Value>,
    pub facts: Vec<Value>,
    pub edits: Vec<Value>,
    /// The document version to remember for this row after the batch commits;
    /// `None` after a delete.
    pub new_version: Option<String>,
    /// The row's document id (stable across versions).
    pub doc_id: String,
    pub skipped: SkipCounts,
    /// True when nothing changed (same version as `prev_version`).
    pub unchanged: bool,
}

impl MappedChange {
    pub fn actions(&self) -> usize {
        self.documents.len() + self.facts.len() + self.edits.len()
    }
}

/// Maps rows of one table to facts. Pure: no I/O, no state.
#[derive(Debug, Clone)]
pub struct RowFactMapper {
    /// Scopes document ids so two sources with a table of the same name in one
    /// namespace do not collide (a file stem, or a connection name).
    pub source: String,
    pub table: String,
    /// The column whose value is the row key. When set, it is rendered in the
    /// document but is not itself a fact.
    pub key_column: Option<String>,
    /// The column whose value names the subject (`"Ana Bel"`). Falls back to
    /// `"<table> <key>"`.
    pub subject_column: Option<String>,
}

impl RowFactMapper {
    pub fn doc_id(&self, key: &str) -> String {
        format!("{}/{}/{}", self.source, self.table, key)
    }

    pub fn subject_id(&self, key: &str) -> String {
        format!("{}:{}", self.table, key)
    }

    /// The facts `_bulk` actions for one row change.
    pub fn map_change(&self, change: &RowChange) -> MappedChange {
        match change {
            RowChange::Delete { key, prev_version } => {
                let doc_id = self.doc_id(key);
                MappedChange {
                    edits: vec![withdraw(&doc_id, prev_version)],
                    doc_id,
                    ..Default::default()
                }
            }
            RowChange::Upsert {
                key,
                columns,
                prev_version,
            } => self.map_upsert(key, columns, prev_version.as_deref()),
        }
    }

    fn map_upsert(
        &self,
        key: &str,
        columns: &Map<String, Value>,
        prev: Option<&str>,
    ) -> MappedChange {
        let doc_id = self.doc_id(key);
        let subject_id = self.subject_id(key);
        let mut skipped = SkipCounts::default();

        // Typed cells in a stable (sorted) column order, so the rendered
        // document and its version do not depend on extractor map order.
        let mut cells: Vec<(String, String, TypedValue)> = Vec::new();
        let mut ordered: Vec<(&String, &Value)> = columns.iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(b.0));
        for (col, v) in ordered {
            match classify(v) {
                Ok(tv) => cells.push((col.clone(), relation_words(col), tv)),
                Err(Skip::Empty) => skipped.empty += 1,
                Err(Skip::Unsupported) => skipped.unsupported += 1,
                Err(Skip::LongText) => skipped.long_text += 1,
            }
        }

        let subject = self
            .subject_column
            .as_ref()
            .and_then(|c| cells.iter().find(|(col, _, _)| col == c))
            .map(|(_, _, tv)| tv.quote.clone())
            .unwrap_or_else(|| format!("{} {}", self.table, key));

        // Render: one "relation: value" line per cell. Spans are half-open
        // CODE-POINT offsets into this text (the contract), so count chars.
        let mut text = String::new();
        let mut cp = 0usize;
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(cells.len());
        for (i, (_, rel, tv)) in cells.iter().enumerate() {
            if i > 0 {
                text.push('\n');
                cp += 1;
            }
            text.push_str(rel);
            text.push_str(": ");
            cp += rel.chars().count() + 2;
            let start = cp;
            text.push_str(&tv.quote);
            cp += tv.quote.chars().count();
            spans.push((start, cp));
        }
        let version = content_version(&text);
        if prev == Some(version.as_str()) {
            return MappedChange {
                new_version: Some(version),
                doc_id,
                skipped,
                unchanged: true,
                ..Default::default()
            };
        }

        let mut out = MappedChange {
            doc_id: doc_id.clone(),
            new_version: Some(version.clone()),
            skipped,
            ..Default::default()
        };
        if let Some(p) = prev {
            out.edits.push(withdraw(&doc_id, p));
        }
        out.documents
            .push(json!({"doc_id": doc_id, "version": version, "text": text}));
        for ((col, rel, tv), (start, end)) in cells.iter().zip(spans) {
            if self.key_column.as_deref() == Some(col.as_str()) {
                continue;
            }
            let mut f = json!({
                "fact_id": format!("{doc_id}/{col}@{version}"),
                "subject": subject,
                "subject_id": subject_id,
                "relation": rel,
                "kind": tv.kind,
                "doc_id": doc_id,
                "version": version,
                "start": start,
                "end": end,
                "quote": tv.quote,
            });
            f["value"] = tv.value.clone();
            out.facts.push(f);
        }
        out
    }
}

fn withdraw(doc_id: &str, version: &str) -> Value {
    json!({"op": "withdraw_document", "doc_id": doc_id, "version": version})
}

/// Content-addressed document version: same rendered row, same version, so an
/// unchanged row is a no-op without asking the node.
pub fn content_version(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// `start_date` / `startDate` / `Start Date` → `start date`: the words a
/// question would use, which is what `_lookup` and the model match on.
pub fn relation_words(col: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in col.chars() {
        if ch == '_' || ch == '-' || ch == '.' || ch.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !out.ends_with(' ') {
            out.push(' ');
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        out.extend(ch.to_lowercase());
    }
    let t = out.trim().to_string();
    if t.is_empty() {
        col.to_string()
    } else {
        t
    }
}

#[derive(Debug, Clone, PartialEq)]
struct TypedValue {
    kind: &'static str,
    /// The contract's `value`: a string for text/number/date, a JSON bool for
    /// boolean.
    value: Value,
    /// The exact characters rendered into the document and quoted.
    quote: String,
}

enum Skip {
    Empty,
    Unsupported,
    LongText,
}

fn classify(v: &Value) -> std::result::Result<TypedValue, Skip> {
    match v {
        Value::Null => Err(Skip::Empty),
        Value::Bool(b) => Ok(TypedValue {
            kind: "boolean",
            value: Value::Bool(*b),
            quote: b.to_string(),
        }),
        Value::Number(n) => {
            let s = n.to_string();
            Ok(TypedValue {
                kind: "number",
                value: Value::String(s.clone()),
                quote: s,
            })
        }
        Value::String(s) => classify_str(s),
        Value::Array(_) | Value::Object(_) => Err(Skip::Unsupported),
    }
}

fn classify_str(raw: &str) -> std::result::Result<TypedValue, Skip> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(Skip::Empty);
    }
    if s.chars().count() > MAX_TEXT_VALUE_CHARS {
        return Err(Skip::LongText);
    }
    let text = |kind: &'static str| TypedValue {
        kind,
        value: Value::String(s.to_string()),
        quote: s.to_string(),
    };
    if is_iso_date(s) {
        return Ok(text("date"));
    }
    if is_plain_number(s) {
        return Ok(text("number"));
    }
    match s.to_ascii_lowercase().as_str() {
        "true" => {
            return Ok(TypedValue {
                kind: "boolean",
                value: Value::Bool(true),
                quote: s.to_string(),
            })
        }
        "false" => {
            return Ok(TypedValue {
                kind: "boolean",
                value: Value::Bool(false),
                quote: s.to_string(),
            })
        }
        _ => {}
    }
    Ok(text("text"))
}

/// `YYYY-MM-DD` and a real calendar date (the contract refuses anything else
/// as a date; a datetime stays text).
fn is_iso_date(s: &str) -> bool {
    s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// A decimal number as written in a table cell. A leading zero on a
/// multi-digit integer part (`00123`, a zip code or a code) stays text, so an
/// identifier never turns into a different number.
fn is_plain_number(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if int.len() > 1 && int.starts_with('0') {
        return false;
    }
    match frac {
        None => true,
        Some(f) => !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
    }
}

// ───────────────────────────── key and subject choice ─────────────────────────────

const KEY_NAMES: [&str; 6] = ["id", "uuid", "key", "pk", "code", "guid"];
const SUBJECT_NAMES: [&str; 9] = [
    "name",
    "full_name",
    "fullname",
    "display_name",
    "title",
    "label",
    "username",
    "email",
    "subject",
];

/// The key column for a table: an explicit choice, else a conventional name
/// (`id`, `<table>_id`, `<singular>_id`, `uuid`, …) present in every row.
pub fn infer_key_column(table: &str, columns: &[String]) -> Option<String> {
    let lc: Vec<String> = columns.iter().map(|c| c.to_ascii_lowercase()).collect();
    let t = table.to_ascii_lowercase();
    let singular = t.strip_suffix('s').unwrap_or(&t).to_string();
    let mut wanted: Vec<String> = KEY_NAMES.iter().map(|s| s.to_string()).collect();
    wanted.insert(1, format!("{t}_id"));
    wanted.insert(2, format!("{singular}_id"));
    for w in wanted {
        if let Some(i) = lc.iter().position(|c| *c == w) {
            return Some(columns[i].clone());
        }
    }
    None
}

pub fn infer_subject_column(columns: &[String]) -> Option<String> {
    let lc: Vec<String> = columns.iter().map(|c| c.to_ascii_lowercase()).collect();
    SUBJECT_NAMES
        .iter()
        .find_map(|w| lc.iter().position(|c| c == w).map(|i| columns[i].clone()))
}

/// A cell rendered as a key string (`"42"`, `"emp-7"`).
pub fn key_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ───────────────────────────── the node client ─────────────────────────────

/// A blocking client for the node's `/_facts` surface.
pub struct FactsClient {
    base: String,
    api_key: Option<String>,
    http: reqwest::blocking::Client,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct NamespaceStatus {
    #[serde(default)]
    pub records: u64,
    #[serde(default)]
    pub revision: u64,
}

impl FactsClient {
    pub fn new(url: &str, api_key: Option<String>) -> Result<Self> {
        let http = crate::esclient::node_blocking_builder(url)
            .timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self {
            base: url.trim_end_matches('/').to_string(),
            api_key,
            http,
        })
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let r = self.http.request(m, format!("{}{}", self.base, path));
        match &self.api_key {
            Some(k) => r.header("Authorization", format!("ApiKey {k}")),
            None => r,
        }
    }

    /// Fail early, in the node's own words, when XERJ Query-1 is off.
    pub fn preflight(&self) -> Result<()> {
        let resp = self
            .req(reqwest::Method::GET, "/_facts")
            .send()
            .with_context(|| format!("no XERJ node answered at {}", self.base))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body: Value = resp.json().unwrap_or(Value::Null);
        let ty = body["error"]["type"].as_str().unwrap_or("");
        let reason = body["error"]["reason"].as_str().unwrap_or("");
        if ty == "xerj_query_not_enabled" {
            bail!("xerj_query_not_enabled: {reason}");
        }
        if status.as_u16() == 404 {
            bail!(
                "the node at {} does not serve /_facts (XERJ Query-1 needs rc.101 or later, \
                 started with --facts-model-dir <DIR>)",
                self.base
            );
        }
        bail!("GET /_facts answered {status}: {body}");
    }

    /// `None` when the namespace was never written.
    pub fn namespace(&self, ns: &str) -> Result<Option<NamespaceStatus>> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/_facts/{ns}"))
            .send()?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("GET /_facts/{ns} answered {}", resp.status());
        }
        Ok(Some(resp.json()?))
    }

    /// Post one all-or-nothing batch; returns the committed revision.
    pub fn bulk(&self, ns: &str, batch: &FactsBatch) -> Result<u64> {
        let mut attempt = 0u32;
        loop {
            let resp = self
                .req(reqwest::Method::POST, &format!("/_facts/{ns}/_bulk"))
                .json(&batch.to_json())
                .send();
            match resp {
                Ok(r) if r.status().is_success() => {
                    let v: Value = r.json()?;
                    return Ok(v["committed"]["to_rev"].as_u64().unwrap_or(0));
                }
                Ok(r) => {
                    let status = r.status();
                    let retry = status.as_u16() == 503 || status.as_u16() == 429;
                    let body: Value = r.json().unwrap_or(Value::Null);
                    if retry && attempt < 5 {
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(500 << attempt));
                        continue;
                    }
                    let ty = body["error"]["type"].as_str().unwrap_or("error");
                    let reason = body["error"]["reason"].as_str().unwrap_or("");
                    bail!("POST /_facts/{ns}/_bulk answered {status} {ty}: {reason}");
                }
                Err(e) if attempt < 3 => {
                    attempt += 1;
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(500 << attempt));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Mapped changes accumulated into one `_bulk` body.
#[derive(Debug, Default)]
pub struct FactsBatch {
    pub documents: Vec<Value>,
    pub facts: Vec<Value>,
    pub edits: Vec<Value>,
}

impl FactsBatch {
    pub fn push(&mut self, m: &MappedChange) {
        self.documents.extend(m.documents.iter().cloned());
        self.facts.extend(m.facts.iter().cloned());
        self.edits.extend(m.edits.iter().cloned());
    }
    pub fn actions(&self) -> usize {
        self.documents.len() + self.facts.len() + self.edits.len()
    }
    pub fn is_empty(&self) -> bool {
        self.actions() == 0
    }
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        if !self.documents.is_empty() {
            m.insert("documents".into(), Value::Array(self.documents.clone()));
        }
        if !self.facts.is_empty() {
            m.insert("facts".into(), Value::Array(self.facts.clone()));
        }
        if !self.edits.is_empty() {
            m.insert("edits".into(), Value::Array(self.edits.clone()));
        }
        Value::Object(m)
    }
}

// ───────────────────────────── committed-version state ─────────────────────────────

/// What this machine last committed per row for one (namespace, source):
/// `doc_id → version`. An append-only journal, replayed on load and
/// compacted at the end of a run, so a crash mid-run loses nothing that
/// committed.
pub struct VersionState {
    path: PathBuf,
    pub versions: HashMap<String, String>,
    journal: Option<std::fs::File>,
}

impl VersionState {
    pub fn load(path: PathBuf) -> Result<Self> {
        let mut versions = HashMap::new();
        if let Ok(f) = std::fs::File::open(&path) {
            for line in std::io::BufReader::new(f).lines() {
                let line = line?;
                let mut it = line.splitn(3, '\t');
                match (it.next(), it.next(), it.next()) {
                    (Some("+"), Some(d), Some(v)) => {
                        versions.insert(d.to_string(), v.to_string());
                    }
                    (Some("-"), Some(d), _) => {
                        versions.remove(d);
                    }
                    _ => {}
                }
            }
        }
        Ok(Self {
            path,
            versions,
            journal: None,
        })
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn record(&mut self, doc_id: &str, version: Option<&str>) -> Result<()> {
        if self.journal.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            self.journal = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let j = self.journal.as_mut().expect("opened above");
        match version {
            Some(v) => {
                writeln!(j, "+\t{doc_id}\t{v}")?;
                self.versions.insert(doc_id.to_string(), v.to_string());
            }
            None => {
                writeln!(j, "-\t{doc_id}")?;
                self.versions.remove(doc_id);
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        if let Some(j) = self.journal.as_mut() {
            j.sync_data()?;
        }
        Ok(())
    }

    /// Rewrite the journal as one line per live row.
    pub fn compact(&mut self) -> Result<()> {
        self.journal = None;
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            let mut keys: Vec<_> = self.versions.iter().collect();
            keys.sort();
            for (d, v) in keys {
                writeln!(f, "+\t{d}\t{v}")?;
            }
            f.flush()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// `$XDG_STATE_HOME/xerj/facts` (or `~/.local/state/xerj/facts`).
pub fn default_state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XERJ_FACTS_STATE_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("xerj").join("facts")
}

fn state_file(dir: &Path, url: &str, ns: &str, source_path: &Path) -> PathBuf {
    let key = format!("{url}\n{ns}\n{}", source_path.display());
    let h = content_version(&key);
    dir.join(format!("{ns}-{h}.journal"))
}

// ───────────────────────────── the whole-file run ─────────────────────────────

/// `xerj autoindex <path> --facts <namespace>`.
#[derive(Debug, Clone)]
pub struct FactsCfg {
    pub path: PathBuf,
    pub namespace: String,
    pub url: String,
    pub api_key: Option<String>,
    pub key_column: Option<String>,
    pub subject_column: Option<String>,
    pub tables: Vec<String>,
    pub batch_actions: usize,
    pub state_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub json: bool,
    pub quiet: bool,
}

/// Per-table outcome.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct TableReport {
    pub source: String,
    pub table: String,
    pub key: String,
    pub subject: String,
    pub rows: u64,
    pub written: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub withdrawn: u64,
    pub facts: u64,
    pub skipped: SkipCounts,
    pub rows_without_key: u64,
    pub warnings: Vec<String>,
}

const TABLE_FAMILIES: [Family; 4] = [Family::Sqlite, Family::SqlDump, Family::Csv, Family::Xlsx];

fn table_files(root: &Path) -> Result<Vec<(PathBuf, sniff::Sniffed)>> {
    let mut out = Vec::new();
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.to_path_buf());
    } else if root.is_dir() {
        for e in walkdir::WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
        {
            let e = e?;
            if e.file_type().is_file() {
                paths.push(e.path().to_path_buf());
            }
        }
    } else {
        bail!("{} is not a file or folder", root.display());
    }
    for p in paths {
        let sn = match sniff::sniff(&p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if TABLE_FAMILIES.contains(&sn.family) {
            out.push((p, sn));
        }
    }
    Ok(out)
}

fn source_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "source".into());
    stem.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// One pass over a table file, grouped by table. `limit` stops early.
fn for_each_row(
    path: &Path,
    sn: &sniff::Sniffed,
    f: &mut dyn FnMut(RawRecord) -> bool,
) -> Result<()> {
    let mut sink = |r: RawRecord| f(r);
    extract::extract(path, sn, None, &mut sink)?;
    Ok(())
}

struct Progress {
    tty: bool,
    quiet: bool,
    started: Instant,
    last: Instant,
    total: u64,
}

impl Progress {
    fn tick(&mut self, done: u64, force: bool) {
        if self.quiet {
            return;
        }
        let every = if self.tty {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(10)
        };
        if !force && self.last.elapsed() < every {
            return;
        }
        self.last = Instant::now();
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = done as f64 / secs;
        let eta = if rate > 0.0 && self.total > done {
            format!("{:.0}s", (self.total - done) as f64 / rate)
        } else {
            "-".into()
        };
        let pct = if self.total > 0 {
            done as f64 * 100.0 / self.total as f64
        } else {
            100.0
        };
        let line = format!(
            "facts: {done}/{} rows ({pct:.0}%) · {rate:.0} rows/s · ETA {eta}",
            self.total
        );
        if self.tty {
            eprint!("\r{line}\x1b[K");
            let _ = std::io::stderr().flush();
        } else {
            eprintln!("{line}");
        }
    }
    fn done(&self) {
        if self.tty && !self.quiet {
            eprintln!();
        }
    }
}

/// Per table: its column names and row count (pass 1).
type TableColumns = BTreeMap<String, (Vec<String>, u64)>;

pub fn run(cfg: FactsCfg) -> Result<i32> {
    let files = table_files(&cfg.path)?;
    if files.is_empty() {
        bail!(
            "{} has no table-shaped file (SQLite database, SQL dump, CSV/TSV or XLSX); \
             `--facts` writes table rows as facts. Index other files for search without --facts",
            cfg.path.display()
        );
    }
    let client = FactsClient::new(&cfg.url, cfg.api_key.clone())?;
    if !cfg.dry_run {
        client.preflight()?;
    }
    let ns_status = if cfg.dry_run {
        None
    } else {
        client.namespace(&cfg.namespace)?
    };
    let state_dir = cfg.state_dir.clone().unwrap_or_else(default_state_dir);

    // Pass 1: columns and row counts per table (the ETA, and the key choice).
    let mut plans: Vec<(PathBuf, sniff::Sniffed, TableColumns)> = Vec::new();
    let mut total_rows = 0u64;
    for (p, sn) in files {
        let mut tables: TableColumns = BTreeMap::new();
        for_each_row(&p, &sn, &mut |r| {
            let t = r.group.clone().unwrap_or_else(|| source_name(&p));
            let e = tables.entry(t).or_default();
            for k in r.fields.keys() {
                if !e.0.contains(k) {
                    e.0.push(k.clone());
                }
            }
            e.1 += 1;
            true
        })?;
        tables.retain(|t, _| cfg.tables.is_empty() || cfg.tables.contains(t));
        total_rows += tables.values().map(|v| v.1).sum::<u64>();
        plans.push((p, sn, tables));
    }

    let mut reports: Vec<TableReport> = Vec::new();
    let mut prog = Progress {
        tty: std::io::stderr().is_terminal(),
        quiet: cfg.quiet || cfg.json,
        started: Instant::now(),
        last: Instant::now() - Duration::from_secs(60),
        total: total_rows,
    };
    let mut done_rows = 0u64;

    for (path, sn, tables) in plans {
        let source = source_name(&path);
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let mut state =
            VersionState::load(state_file(&state_dir, &cfg.url, &cfg.namespace, &canon))?;
        let _ = &ns_status;

        let mut mappers: HashMap<String, (RowFactMapper, TableReport, HashSet<String>)> =
            HashMap::new();
        for (t, (cols, rows)) in &tables {
            let key = match &cfg.key_column {
                Some(k) if cols.contains(k) => Some(k.clone()),
                Some(k) => bail!(
                    "--key {k}: table `{t}` has no such column (columns: {})",
                    cols.join(", ")
                ),
                None => infer_key_column(t, cols),
            };
            let subject = cfg
                .subject_column
                .clone()
                .filter(|c| cols.contains(c))
                .or_else(|| infer_subject_column(cols));
            let mut rep = TableReport {
                source: source.clone(),
                table: t.clone(),
                key: key.clone().unwrap_or_else(|| "row position".into()),
                subject: subject.clone().unwrap_or_else(|| format!("\"{t} <key>\"")),
                rows: *rows,
                ..Default::default()
            };
            if key.is_none() {
                let stable = sn.family == Family::Sqlite;
                rep.warnings.push(if stable {
                    format!("`{t}`: no key column found; using SQLite rowid. Pass --key <column> to choose one")
                } else {
                    format!(
                        "`{t}`: no key column found; using row position, which renumbers subjects when \
                         rows are inserted or removed. Pass --key <column>"
                    )
                });
            }
            mappers.insert(
                t.clone(),
                (
                    RowFactMapper {
                        source: source.clone(),
                        table: t.clone(),
                        key_column: key,
                        subject_column: subject,
                    },
                    rep,
                    HashSet::new(),
                ),
            );
        }

        // Pass 2: map and write.
        let mut batch = FactsBatch::default();
        let mut pending: Vec<(String, Option<String>)> = Vec::new();
        let mut err: Option<anyhow::Error> = None;
        let flush = |batch: &mut FactsBatch,
                     pending: &mut Vec<(String, Option<String>)>,
                     state: &mut VersionState|
         -> Result<()> {
            if batch.is_empty() {
                pending.clear();
                return Ok(());
            }
            if !cfg.dry_run {
                client.bulk(&cfg.namespace, batch)?;
                for (d, v) in pending.drain(..) {
                    state.record(&d, v.as_deref())?;
                }
                state.flush()?;
            } else {
                pending.clear();
            }
            *batch = FactsBatch::default();
            Ok(())
        };

        for_each_row(&path, &sn, &mut |r: RawRecord| {
            let t = r.group.clone().unwrap_or_else(|| source.clone());
            let Some((mapper, rep, seen)) = mappers.get_mut(&t) else {
                return true;
            };
            let key = match &mapper.key_column {
                Some(k) => match r.fields.get(k).and_then(key_string) {
                    Some(v) => v,
                    None => {
                        rep.rows_without_key += 1;
                        return true;
                    }
                },
                None => r
                    .locator
                    .rsplit(':')
                    .next()
                    .unwrap_or(&r.locator)
                    .to_string(),
            };
            if !seen.insert(key.clone()) {
                rep.warnings.push(format!(
                    "`{t}`: key {key} appears twice; the later row was skipped"
                ));
                return true;
            }
            let doc_id = mapper.doc_id(&key);
            let prev = state.versions.get(&doc_id).cloned();
            let m = mapper.map_change(&RowChange::Upsert {
                key,
                columns: r.fields,
                prev_version: prev.clone(),
            });
            rep.skipped.add(&m.skipped);
            done_rows += 1;
            if m.unchanged {
                rep.unchanged += 1;
            } else {
                if prev.is_some() {
                    rep.updated += 1;
                } else {
                    rep.written += 1;
                }
                rep.facts += m.facts.len() as u64;
                if batch.actions() + m.actions() > cfg.batch_actions && !batch.is_empty() {
                    if let Err(e) = flush(&mut batch, &mut pending, &mut state) {
                        err = Some(e);
                        return false;
                    }
                }
                batch.push(&m);
                pending.push((m.doc_id.clone(), m.new_version.clone()));
            }
            prog.tick(done_rows, false);
            true
        })?;
        if let Some(e) = err {
            prog.done();
            return Err(e);
        }

        // Rows this source wrote before and no longer has: withdraw.
        for (mapper, rep, seen) in mappers.values_mut() {
            let prefix = format!("{}/{}/", mapper.source, mapper.table);
            let gone: Vec<(String, String)> = state
                .versions
                .iter()
                .filter(|(d, _)| d.starts_with(&prefix) && !seen.contains(&d[prefix.len()..]))
                .map(|(d, v)| (d.clone(), v.clone()))
                .collect();
            for (d, v) in gone {
                let key = d[prefix.len()..].to_string();
                let m = mapper.map_change(&RowChange::Delete {
                    key,
                    prev_version: v,
                });
                if batch.actions() + m.actions() > cfg.batch_actions && !batch.is_empty() {
                    flush(&mut batch, &mut pending, &mut state)?;
                }
                batch.push(&m);
                pending.push((d, None));
                rep.withdrawn += 1;
            }
        }
        flush(&mut batch, &mut pending, &mut state)?;
        if !cfg.dry_run {
            state.compact()?;
        }
        prog.tick(done_rows, true);
        reports.extend(mappers.into_values().map(|(_, r, _)| r));
    }
    prog.done();
    reports.sort_by(|a, b| (&a.source, &a.table).cmp(&(&b.source, &b.table)));
    print_report(&cfg, &reports);
    Ok(0)
}

fn print_report(cfg: &FactsCfg, reports: &[TableReport]) {
    if cfg.json {
        println!(
            "{}",
            json!({"namespace": cfg.namespace, "dry_run": cfg.dry_run, "tables": reports})
        );
        return;
    }
    let verb = if cfg.dry_run { "would write" } else { "wrote" };
    for r in reports {
        println!(
            "{}/{}: {} rows · key {} · subject {} · {verb} {} facts ({} new, {} changed, {} unchanged, {} withdrawn)",
            r.source, r.table, r.rows, r.key, r.subject, r.facts, r.written, r.updated, r.unchanged, r.withdrawn
        );
        let s = &r.skipped;
        if s.empty + s.unsupported + s.long_text + r.rows_without_key > 0 {
            println!(
                "  skipped cells: {} empty, {} unsupported type, {} text over {} chars; rows without a key: {}",
                s.empty, s.unsupported, s.long_text, MAX_TEXT_VALUE_CHARS, r.rows_without_key
            );
        }
        for w in r.warnings.iter().take(5) {
            println!("  note: {w}");
        }
        if r.warnings.len() > 5 {
            println!("  note: {} more", r.warnings.len() - 5);
        }
    }
    if !cfg.dry_run && !cfg.quiet {
        println!(
            "ask: curl -s -XPOST {}/_facts/{}/_ask -H 'content-type: application/json' -d '{{\"question\":\"How many <rows> have <column> <value>?\"}}'",
            cfg.url.trim_end_matches('/'),
            cfg.namespace
        );
    }
}

/// Parse `xerj autoindex <path> --facts <namespace> [options]`.
pub fn parse_args(args: &[String]) -> std::result::Result<FactsCfg, String> {
    let mut path: Option<PathBuf> = None;
    let mut ns: Option<String> = None;
    let mut url = "http://localhost:9200".to_string();
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut key_column = None;
    let mut subject_column = None;
    let mut tables = Vec::new();
    let mut batch_actions = DEFAULT_BATCH_ACTIONS;
    let mut state_dir = None;
    let (mut dry_run, mut json_out, mut quiet) = (false, false, false);
    let mut it = args.iter();
    let val =
        |it: &mut std::slice::Iter<String>, flag: &str| -> std::result::Result<String, String> {
            match it.next() {
                Some(v) if !v.starts_with("--") => Ok(v.clone()),
                _ => Err(format!("{flag} needs a value")),
            }
        };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--facts" => ns = Some(val(&mut it, "--facts")?),
            "--url" => url = val(&mut it, "--url")?,
            "--api-key" => api_key = Some(val(&mut it, "--api-key")?),
            "--key" => key_column = Some(val(&mut it, "--key")?),
            "--subject" => subject_column = Some(val(&mut it, "--subject")?),
            "--table" => tables.push(val(&mut it, "--table")?),
            "--batch-actions" => {
                let v = val(&mut it, "--batch-actions")?;
                batch_actions = v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=50_000).contains(n))
                    .ok_or("--batch-actions takes a number from 1 to 50000 (the node's default per-request limit)")?;
            }
            "--state-dir" => state_dir = Some(PathBuf::from(val(&mut it, "--state-dir")?)),
            "--dry-run" => dry_run = true,
            "--json" => json_out = true,
            "--quiet" | "-q" => quiet = true,
            other if other.starts_with('-') => {
                return Err(format!(
                    "{other} is not an option of `autoindex <path> --facts <namespace>`; options: --url --api-key \
                     --key --subject --table --batch-actions --state-dir --dry-run --json --quiet"
                ))
            }
            other => {
                if path.is_some() {
                    return Err(format!("one path only (got a second: {other})"));
                }
                path = Some(PathBuf::from(other));
            }
        }
    }
    let namespace = ns.ok_or("--facts needs a namespace")?;
    if namespace.is_empty()
        || !namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("--facts <namespace>: lowercase letters, digits, '-' and '_' only".into());
    }
    let path = path.ok_or(
        "--facts needs a path: a SQLite file, SQL dump, CSV/TSV, XLSX, or a folder of them",
    )?;
    Ok(FactsCfg {
        path,
        namespace,
        url,
        api_key,
        key_column,
        subject_column,
        tables,
        batch_actions,
        state_dir,
        dry_run,
        json: json_out,
        quiet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> RowFactMapper {
        RowFactMapper {
            source: "hr".into(),
            table: "people".into(),
            key_column: Some("id".into()),
            subject_column: Some("name".into()),
        }
    }

    fn row(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_row_becomes_one_document_and_one_fact_per_column_with_exact_spans() {
        let m = mapper().map_change(&RowChange::Upsert {
            key: "7".into(),
            columns: row(
                json!({"id": 7, "name": "Ana Bel", "office": "Berlin", "height_cm": 170,
                                "start_date": "2019-08-13", "active": true}),
            ),
            prev_version: None,
        });
        assert_eq!(m.documents.len(), 1);
        let text = m.documents[0]["text"].as_str().unwrap();
        assert_eq!(m.facts.len(), 5, "every column but the key");
        for f in &m.facts {
            let (s, e) = (
                f["start"].as_u64().unwrap() as usize,
                f["end"].as_u64().unwrap() as usize,
            );
            let span: String = text.chars().skip(s).take(e - s).collect();
            assert_eq!(
                span,
                f["quote"].as_str().unwrap(),
                "quote equals text[start:end]"
            );
            assert_eq!(f["subject_id"], "people:7");
            assert_eq!(f["subject"], "Ana Bel");
            assert_eq!(f["doc_id"], "hr/people/7");
        }
        let by_rel = |r: &str| m.facts.iter().find(|f| f["relation"] == r).unwrap().clone();
        assert_eq!(by_rel("height cm")["kind"], "number");
        assert_eq!(by_rel("height cm")["value"], "170");
        assert_eq!(by_rel("start date")["kind"], "date");
        assert_eq!(by_rel("active")["kind"], "boolean");
        assert_eq!(by_rel("active")["value"], true);
        assert_eq!(by_rel("office")["kind"], "text");
        assert!(m.edits.is_empty());
    }

    #[test]
    fn spans_count_code_points_not_bytes() {
        let m = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: row(json!({"name": "Zoë Ångström", "city": "Zürich"})),
            prev_version: None,
        });
        let text = m.documents[0]["text"].as_str().unwrap();
        for f in &m.facts {
            let (s, e) = (
                f["start"].as_u64().unwrap() as usize,
                f["end"].as_u64().unwrap() as usize,
            );
            let span: String = text.chars().skip(s).take(e - s).collect();
            assert_eq!(span, f["quote"].as_str().unwrap());
        }
    }

    #[test]
    fn an_unchanged_row_is_a_no_op_and_a_changed_row_withdraws_the_old_version() {
        let cols = row(json!({"id": 1, "name": "Bo", "office": "Berlin"}));
        let first = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: cols.clone(),
            prev_version: None,
        });
        let v1 = first.new_version.clone().unwrap();
        let again = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: cols,
            prev_version: Some(v1.clone()),
        });
        assert!(again.unchanged && again.actions() == 0);
        let changed = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: row(json!({"id": 1, "name": "Bo", "office": "Oslo"})),
            prev_version: Some(v1.clone()),
        });
        assert_eq!(
            changed.edits,
            vec![json!({"op":"withdraw_document","doc_id":"hr/people/1","version": v1})]
        );
        assert_ne!(changed.new_version.unwrap(), v1);
        assert_eq!(changed.documents.len(), 1);
    }

    #[test]
    fn a_delete_withdraws_the_last_committed_version() {
        let m = mapper().map_change(&RowChange::Delete {
            key: "9".into(),
            prev_version: "abc".into(),
        });
        assert_eq!(
            m.edits,
            vec![json!({"op":"withdraw_document","doc_id":"hr/people/9","version":"abc"})]
        );
        assert!(m.new_version.is_none() && m.facts.is_empty());
    }

    #[test]
    fn column_order_does_not_change_the_version() {
        let a = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: row(json!({"a": "x", "b": "y"})),
            prev_version: None,
        });
        let mut rev = Map::new();
        rev.insert("b".into(), json!("y"));
        rev.insert("a".into(), json!("x"));
        let b = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: rev,
            prev_version: None,
        });
        assert_eq!(a.new_version, b.new_version);
    }

    #[test]
    fn cells_that_are_not_facts_are_counted() {
        let long = "x".repeat(MAX_TEXT_VALUE_CHARS + 1);
        let m = mapper().map_change(&RowChange::Upsert {
            key: "1".into(),
            columns: row(
                json!({"name": "A", "notes": long, "tags": ["a"], "blank": "  ", "gone": null}),
            ),
            prev_version: None,
        });
        assert_eq!(
            m.skipped,
            SkipCounts {
                empty: 2,
                unsupported: 1,
                long_text: 1
            }
        );
        assert_eq!(m.facts.len(), 1);
    }

    #[test]
    fn typing_keeps_identifiers_as_text_and_only_real_dates_as_dates() {
        let k = |s: &str| classify_str(s).ok().map(|t| t.kind);
        assert_eq!(
            k("00123"),
            Some("text"),
            "a leading zero is a code, not a number"
        );
        assert_eq!(k("0.5"), Some("number"));
        assert_eq!(k("-12"), Some("number"));
        assert_eq!(k("1e5"), Some("text"));
        assert_eq!(k("2019-02-30"), Some("text"), "not a calendar date");
        assert_eq!(k("2019-08-13"), Some("date"));
        assert_eq!(k("2019-08-13T10:00:00"), Some("text"));
        assert_eq!(k("TRUE"), Some("boolean"));
        assert_eq!(k("yes"), Some("text"));
    }

    #[test]
    fn relations_are_written_the_way_a_question_names_them() {
        assert_eq!(relation_words("start_date"), "start date");
        assert_eq!(relation_words("startDate"), "start date");
        assert_eq!(relation_words("Office"), "office");
        assert_eq!(relation_words("devices_count"), "devices count");
    }

    #[test]
    fn key_and_subject_columns_are_inferred_by_name() {
        let cols: Vec<String> = ["person_id", "name", "office"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            infer_key_column("persons", &cols).as_deref(),
            Some("person_id")
        );
        assert_eq!(
            infer_key_column("people", &["id".to_string()]).as_deref(),
            Some("id")
        );
        assert_eq!(infer_key_column("t", &["a".to_string()]), None);
        assert_eq!(infer_subject_column(&cols).as_deref(), Some("name"));
    }

    #[test]
    fn the_journal_replays_and_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ns.journal");
        let mut s = VersionState::load(p.clone()).unwrap();
        s.record("a", Some("1")).unwrap();
        s.record("b", Some("2")).unwrap();
        s.record("a", None).unwrap();
        s.flush().unwrap();
        let s2 = VersionState::load(p.clone()).unwrap();
        assert_eq!(s2.versions.len(), 1);
        assert_eq!(s2.versions.get("b").map(String::as_str), Some("2"));
        let mut s3 = VersionState::load(p.clone()).unwrap();
        s3.compact().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "+\tb\t2\n");
    }

    #[test]
    fn args_parse_and_refuse_clearly() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let c = a(&[
            "db.sqlite",
            "--facts",
            "hr",
            "--key",
            "id",
            "--batch-actions",
            "100",
        ])
        .unwrap();
        assert_eq!(c.namespace, "hr");
        assert_eq!(c.batch_actions, 100);
        assert!(a(&["db.sqlite", "--facts", "HR"]).is_err());
        assert!(a(&["--facts", "hr"]).unwrap_err().contains("path"));
        assert!(a(&["x", "--facts", "hr", "--batch-actions", "60000"]).is_err());
        assert!(a(&["x", "--facts", "hr", "--watch"])
            .unwrap_err()
            .contains("--watch"));
    }

    /// Live: `XERJ_FACTS_TEST_URL=http://127.0.0.1:<port>` pointing at a node
    /// started with `--facts-model-dir`. Writes, re-runs (no-op), changes one
    /// row, deletes one row, and checks `_lookup` after each step.
    #[test]
    fn live_rows_to_facts_round_trip() {
        let Ok(url) = std::env::var("XERJ_FACTS_TEST_URL") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("people.csv");
        std::fs::write(&csv, "id,name,office\n1,Ana Bel,Berlin\n2,Bo Chen,Paris\n").unwrap();
        let ns = format!("rtf-{}", std::process::id());
        let cfg = FactsCfg {
            path: csv.clone(),
            namespace: ns.clone(),
            url: url.clone(),
            api_key: None,
            key_column: None,
            subject_column: None,
            tables: vec![],
            batch_actions: 100,
            state_dir: Some(dir.path().join("state")),
            dry_run: false,
            json: true,
            quiet: true,
        };
        let client = FactsClient::new(&url, None).unwrap();
        let lookup = |s: &str, r: &str| -> Value {
            let resp = reqwest::blocking::Client::new()
                .post(format!("{url}/_facts/{ns}/_lookup"))
                .json(&json!({"subject": s, "relation": r}))
                .send()
                .unwrap();
            resp.json().unwrap()
        };
        run(cfg.clone()).unwrap();
        assert_eq!(lookup("Ana Bel", "office")["value"]["value"], "Berlin");
        let rev1 = client.namespace(&ns).unwrap().unwrap().revision;
        run(cfg.clone()).unwrap();
        assert_eq!(
            client.namespace(&ns).unwrap().unwrap().revision,
            rev1,
            "unchanged re-run writes nothing"
        );
        std::fs::write(&csv, "id,name,office\n1,Ana Bel,Oslo\n").unwrap();
        run(cfg).unwrap();
        assert_eq!(lookup("Ana Bel", "office")["value"]["value"], "Oslo");
        assert_eq!(
            lookup("Bo Chen", "office")["status"],
            "UNKNOWN",
            "a removed row is withdrawn"
        );
    }
}
