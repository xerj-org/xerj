//! Second-brain edge detectors: deterministic, versioned relationship
//! extraction over the corpus autoindex is already walking (contract:
//! SECOND_BRAIN_SPEC §6).
//!
//! Why deterministic instead of an LLM pass: the same corpus must produce a
//! byte-identical edge set on every run (given unchanged mtimes), because edge
//! identity — and therefore idempotent re-runs, kill -9 safety, and the
//! bi-temporal history — hangs off a content hash of (src, type, dst,
//! valid_at). A model in that loop would turn every re-run into a new belief
//! set. Every emitted edge instead carries the exact quote that taught it and
//! a `detector` tag like `wikilink@1`; bump the `@N` on ANY behavior change so
//! old edges remain attributable to the rules that produced them.
//!
//! Edges are ordinary documents in one reserved `.xerj-memory-{brain}-edges`
//! index per brain — no storage-format change, they ride the normal
//! WAL → memtable → segment path and are soft-invalidated (never deleted) so
//! "what did it believe last Tuesday" stays answerable.

pub mod cratecite;
pub mod href;
pub mod mdlink;
pub mod pathcite;
pub mod samedir;
pub mod sequence;
pub mod sharedterm;
pub mod wikilink;

#[cfg(test)]
mod e2e;

use crate::esclient::Es;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Edge-schema version stamped into every emitted edge (`schema_version`).
pub const EDGE_SCHEMA_VERSION: u32 = 1;

/// Reserved `_id` of the per-brain meta document. It carries no `src`/`dst`,
/// so the hop path and every edge count (all filter on `exists src`) never
/// see it.
pub const BRAIN_META_ID: &str = "__xerj-brain-meta";

/// edge_id = xxh3_128("xg1\0" src "\0" type "\0" dst "\0" decimal(valid_at_ms)),
/// rendered as 32 lowercase hex chars ({:032x}).
pub fn edge_id(src: &str, edge_type: &str, dst: &str, valid_at_ms: i64) -> String {
    use xxhash_rust::xxh3::xxh3_128;
    let mut input = Vec::with_capacity(16 + src.len() + edge_type.len() + dst.len());
    input.extend_from_slice(b"xg1\x00");
    input.extend_from_slice(src.as_bytes());
    input.push(0);
    input.extend_from_slice(edge_type.as_bytes());
    input.push(0);
    input.extend_from_slice(dst.as_bytes());
    input.push(0);
    input.extend_from_slice(valid_at_ms.to_string().as_bytes());
    format!("{:032x}", xxh3_128(&input))
}

/// Edges index name for a brain (SECOND_BRAIN_SPEC §1). The leading dot keeps
/// it out of user index APIs and `_cat` listings.
pub fn edges_index_name(brain: &str) -> String {
    format!(".xerj-memory-{brain}-edges")
}

/// Brain-name validation: identical rules to the agent-memory namespace
/// (lowercase/digit start, `[a-z0-9._-]`, ≤200 chars, no `..`) plus the
/// `-edges` suffix rejection — without it a brain named `kb-edges` would
/// collide with brain `kb`'s edge index.
pub fn validate_brain(brain: &str) -> std::result::Result<(), String> {
    if brain.is_empty() {
        return Err("brain name must not be empty".into());
    }
    if brain.len() > 200 {
        return Err("brain name too long (max 200 chars)".into());
    }
    let first = brain.chars().next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("brain name must start with a lowercase letter or digit".into());
    }
    for c in brain.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.');
        if !ok {
            return Err(format!(
                "brain name contains illegal character '{c}' (allowed: a-z 0-9 _ - .)"
            ));
        }
    }
    if brain.contains("..") {
        return Err("brain name must not contain '..'".into());
    }
    if brain.ends_with("-edges") {
        return Err("namespace suffix '-edges' is reserved for graph edge indices".into());
    }
    Ok(())
}

/// The exact edges-index mapping body (SECOND_BRAIN_SPEC §2.1). The API
/// stream's `graph_api::link` sends the byte-identical body — the two writers
/// must agree or the first one to touch a brain decides the column types for
/// everyone.
pub fn edge_index_mapping() -> Value {
    json!({
        "mappings": {
            "properties": {
                "edge_id":        { "type": "keyword" },
                "src":            { "type": "keyword" },
                "dst":            { "type": "keyword" },
                "type":           { "type": "keyword" },
                "weight":         { "type": "float" },
                "valid_at":       { "type": "date", "format": "epoch_millis" },
                "invalid_at":     { "type": "date", "format": "epoch_millis" },
                "created_at":     { "type": "date", "format": "epoch_millis" },
                "expired_at":     { "type": "date", "format": "epoch_millis" },
                "detector":       { "type": "keyword" },
                "confidence":     { "type": "float" },
                "schema_version": { "type": "integer" },
                "src_file":       { "type": "keyword" },
                "src_format":     { "type": "keyword" },
                "dst_format":     { "type": "keyword" },
                "evidence": {
                    "properties": {
                        "quote":  { "type": "text" },
                        "source": { "type": "keyword" },
                        "offset": { "type": "long" }
                    }
                }
            }
        }
    })
}

// ─── corpus resolution table ─────────────────────────────────────────────

/// Corpus-wide resolution table, built once after Phase A (plan assignment):
/// every indexed file's rel path → identity + the doc id of its first section.
pub struct CorpusIndex {
    /// key: root-relative path with forward slashes (`FileEntry.rel`).
    pub files: BTreeMap<String, CorpusFile>,
    /// lowercase file-stem → rel paths bearing it (wikilink resolution;
    /// BTreeMap + sorted values so ambiguity resolution is deterministic).
    pub by_stem: BTreeMap<String, Vec<String>>,
    /// exact file NAME (final path segment, case-sensitive) → rel paths
    /// bearing it. pathcite's suffix resolution: a token's last segment keys
    /// this table, then candidates are filtered by full-suffix match — O(log n)
    /// instead of scanning every rel per token.
    pub by_name: BTreeMap<String, Vec<String>>,
    /// crate-directory basename → rels of the `Cargo.toml` files directly
    /// inside a directory of that name (cratecite's Phase-A crate table). Keyed
    /// by DIRECTORY name, not `[package] name` — CorpusIndex never reads file
    /// contents, and saying otherwise would overclaim.
    pub crate_dirs: BTreeMap<String, Vec<String>>,
}

pub struct CorpusFile {
    pub rel: String,
    /// `ids::file_key` output.
    pub file_key: String,
    pub dataset_slug: String,
    /// `ids::doc_id(dataset_slug, file_key, "file")` — the file's CARD node.
    /// Every file-level edge terminates here. Historically this was the `s0`
    /// section doc id, which does not exist for row/line/page-locator families
    /// (CSV, JSONL, PDF…) — incoming links then pointed at a ghost. The card
    /// is a real emitted node for EVERY indexed file (lib.rs stages it before
    /// the file's records), so anchors are always hydratable and focusable.
    pub anchor_doc_id: String,
    pub mtime_ms: i64,
    /// Parent dir rel path, "" for root.
    pub dir: String,
    /// Sniffed format family (`Family::as_str`), from the frozen plan — NOT
    /// guessed from the extension. href@2 gates on this: the contract scopes
    /// anchor detection to html-extracted files, and an `.html` extension is
    /// not proof of that (sniffing decides the family).
    pub family: String,
    /// File-type label stamped into edges as `src_format`/`dst_format`:
    /// lowercase extension when the name has one ("md", "rs", "pdf"), else the
    /// sniffed family. Extension-level truth is what makes the dashboard's
    /// cross-type counts (md→rs, md→pdf) sayable at all — two txt-prose
    /// families would hide exactly the crossings the map exists to show.
    pub format: String,
}

/// Build one [`CorpusFile`], deriving the anchor node id and parent dir from
/// the identity fields so every caller (pipeline and tests) derives them the
/// same way.
pub fn corpus_file(
    rel: &str,
    file_key: &str,
    dataset_slug: &str,
    family: &str,
    mtime_ms: i64,
) -> CorpusFile {
    CorpusFile {
        rel: rel.to_string(),
        file_key: file_key.to_string(),
        dataset_slug: dataset_slug.to_string(),
        anchor_doc_id: crate::ids::doc_id(dataset_slug, file_key, FILE_CARD_LOCATOR),
        mtime_ms,
        dir: rel
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default(),
        family: family.to_string(),
        format: format_of(rel, family),
    }
}

/// Locator of the per-file card node. Cannot collide with content locators —
/// every extractor's locators are letter+digit shaped (`s3`, `p2-s0`, `b1024`,
/// `row7`, table/row pairs), never the bare word `file`.
pub const FILE_CARD_LOCATOR: &str = "file";

/// Lowercase extension of the final path segment, falling back to the sniffed
/// family for extension-less names and dotfiles. Extensions longer than 12
/// chars or with non-alphanumerics are treated as not-an-extension (version
/// suffixes, minified-name noise).
pub(crate) fn format_of(rel: &str, family: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 12
                && ext.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            ext.to_ascii_lowercase()
        }
        _ => family.to_string(),
    }
}

impl CorpusIndex {
    pub fn build(files: Vec<CorpusFile>) -> Self {
        let mut map: BTreeMap<String, CorpusFile> = BTreeMap::new();
        let mut by_stem: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut crate_dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in files {
            by_stem
                .entry(stem_of(&f.rel).to_ascii_lowercase())
                .or_default()
                .push(f.rel.clone());
            let name = f.rel.rsplit('/').next().unwrap_or(&f.rel);
            by_name
                .entry(name.to_string())
                .or_default()
                .push(f.rel.clone());
            if let Some(dir) = f.rel.strip_suffix("/Cargo.toml") {
                let dir_name = dir.rsplit('/').next().unwrap_or(dir);
                if !dir_name.is_empty() {
                    crate_dirs
                        .entry(dir_name.to_string())
                        .or_default()
                        .push(f.rel.clone());
                }
            }
            map.insert(f.rel.clone(), f);
        }
        for rels in by_stem.values_mut() {
            rels.sort();
            rels.dedup();
        }
        for rels in by_name.values_mut() {
            rels.sort();
            rels.dedup();
        }
        for rels in crate_dirs.values_mut() {
            rels.sort();
            rels.dedup();
        }
        CorpusIndex {
            files: map,
            by_stem,
            by_name,
            crate_dirs,
        }
    }
}

/// File stem of a rel path: final path segment minus its last extension.
/// Dotfiles (".env") keep their full name — an empty stem would collide every
/// dotfile into one wikilink bucket.
pub(crate) fn stem_of(rel: &str) -> &str {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// Rel path minus its last extension (whole-path form used by wikilink's
/// "with or without extension" rule). "" stem is preserved as-is for dotfiles.
pub(crate) fn rel_without_ext(rel: &str) -> String {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let stem = match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => name,
    };
    if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    }
}

// ─── detector trait ──────────────────────────────────────────────────────

/// One detected edge before identity/envelope assembly.
pub struct EdgeDraft {
    /// Node doc id (usually the section containing the evidence).
    pub src: String,
    /// Node doc id (target file's anchor_doc_id).
    pub dst: String,
    pub edge_type: &'static str,
    pub weight: f32,
    pub confidence: f32,
    /// §6.4 determinism rule: mtime of `src_file`, never the wall clock —
    /// re-runs over an unchanged corpus reproduce identical edge_ids.
    pub valid_at_ms: i64,
    /// Rel path that taught this edge (top-level keyword — exists so
    /// replacement invalidation is a doc-values term query, not an
    /// `evidence.source` object scan).
    pub src_file: String,
    /// evidence.quote (≤240 chars, trimmed).
    pub quote: String,
    /// evidence.offset (byte offset in section text; 0 for structural).
    pub offset: u64,
    /// File-type labels of the two endpoints (`CorpusFile.format`), stored as
    /// `src_format`/`dst_format`. Empty string = unknown → key omitted from
    /// the stored doc (same omission discipline as `invalid_at`).
    pub src_format: String,
    pub dst_format: String,
}

/// Per-section textual context. `text` is the exact section string that became
/// the node doc's `body` (post `split_sections`).
pub struct SectionCtx<'a> {
    pub corpus: &'a CorpusIndex,
    pub file: &'a CorpusFile,
    /// Human label of this section within its file, derived from the locator
    /// by `lib.rs::section_label`: "section 3" for `s3`, "page 2 section 0"
    /// for `p2-s0`. Used verbatim in sequence evidence rationales.
    pub section_label: &'a str,
    /// (doc id, label) of the previously staged section of the SAME file;
    /// None for the file's first section. Tracked by the pipeline in stream
    /// order — the only way to know a page boundary's predecessor (`p2-s0`'s
    /// predecessor is the LAST section of page 1, which the locator alone
    /// cannot name).
    pub prev_section: Option<(&'a str, &'a str)>,
    pub section_doc_id: &'a str,
    pub text: &'a str,
}

/// What a resolving detector could not turn into edges. Surfaced in the run
/// summary so a dangling `[[link]]` is recorded, never silently dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DetectorCounters {
    /// Link targets that resolved to no corpus file (dangling).
    pub unresolved: u64,
    /// Link targets with >1 candidate; the lexicographically smallest rel won.
    pub ambiguous: u64,
    /// Candidate edges a density budget refused to draw (sharedterm's
    /// per-document cap). The map's "what it did not show" is a fact about the
    /// corpus too, and swallowing it would make a sparse map look complete.
    pub capped: u64,
}

/// Deterministic, versioned edge detector. NO network, NO LLM, NO clock reads
/// (all time comes from ctx mtimes) — same inputs must yield the same drafts.
pub trait EdgeDetector: Sync {
    /// Versioned tag stored in `detector`, e.g. "wikilink@1". Bump the @N on
    /// ANY behavior change.
    fn tag(&self) -> &'static str;
    /// Per-section textual detection. Default: no-op.
    fn detect_text(&self, _ctx: &SectionCtx<'_>, _out: &mut Vec<EdgeDraft>) {}
    /// Corpus-structural detection, called once after Phase A — before any
    /// file is read, so it sees identities and paths only. Default: no-op.
    fn detect_structure(&self, _corpus: &CorpusIndex, _out: &mut Vec<EdgeDraft>) {}
    /// Corpus-wide detection over the text of the whole run, called ONCE after
    /// Phase B. This is where a detector emits edges that cannot be judged one
    /// section at a time — sharedterm cannot know a term is distinctive until
    /// every document's terms are in. Such a detector accumulates in
    /// `detect_text` behind interior mutability and emits here; determinism is
    /// its own responsibility, since Phase B's worker interleaving is not.
    /// Default: no-op.
    fn detect_corpus(&self, _corpus: &CorpusIndex, _out: &mut Vec<EdgeDraft>) {}
    /// Run-summary honesty counters (default: nothing to report). Detectors
    /// that resolve link targets keep these in atomics so the counts survive
    /// the shared `&self` the parallel Phase B workers hold.
    fn counters(&self) -> DetectorCounters {
        DetectorCounters::default()
    }
}

/// The registry. Order is normative (fixture edge ordering depends on it only
/// via edge sort, so this is cosmetic — but keep it).
pub fn default_detectors() -> Vec<Box<dyn EdgeDetector>> {
    vec![
        Box::new(wikilink::Wikilink::default()),
        Box::new(mdlink::Mdlink::default()),
        Box::new(href::Href::default()),
        Box::new(pathcite::Pathcite::default()),
        Box::new(cratecite::Cratecite::default()),
        Box::new(sequence::Sequence),
        Box::new(samedir::SameDir),
        Box::new(sharedterm::SharedTerm::default()),
    ]
}

/// Detector tag for an edge type. The draft deliberately carries only the
/// type (§6.3 shape); the 1:1 type→tag map lives here, next to the registry,
/// so a version bump touches exactly one detector file plus nothing else.
pub fn detector_tag_for(edge_type: &str) -> &'static str {
    match edge_type {
        wikilink::EDGE_TYPE => wikilink::TAG,
        mdlink::EDGE_TYPE => mdlink::TAG,
        href::EDGE_TYPE => href::TAG,
        pathcite::EDGE_TYPE => pathcite::TAG,
        cratecite::EDGE_TYPE => cratecite::TAG,
        sequence::EDGE_TYPE => sequence::TAG,
        samedir::EDGE_TYPE => samedir::TAG,
        sharedterm::EDGE_TYPE => sharedterm::TAG,
        _ => "unknown@0",
    }
}

// ─── shared text/link helpers ────────────────────────────────────────────

/// Evidence quote discipline: the full trimmed line containing `offset`,
/// clipped to 240 chars — enough to show a human (or agent) exactly what
/// taught the edge without dragging whole sections into every edge doc.
pub(crate) fn line_at(text: &str, offset: usize) -> String {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(text.len());
    clip_quote(&text[start..end])
}

pub(crate) fn clip_quote(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= 240 {
        t.to_string()
    } else {
        t.chars().take(240).collect()
    }
}

/// Outcome of scheme-less link resolution (mdlink/href rule, §6.5).
pub(crate) enum LinkTarget<'a> {
    /// Has a scheme or `//` prefix — external by design, not dangling.
    External,
    /// Nothing left after stripping fragment/query (same-page anchor).
    Empty,
    /// Scheme-less but no corpus file matches — dangling, counted.
    Miss,
    Hit(&'a CorpusFile),
}

/// Resolve a scheme-less url against the corpus: relative to the containing
/// file's dir first, then as root-relative. Fragment (`#…`) and query (`?…`)
/// are stripped; percent-encoding is NOT decoded (a deliberate v1 limit —
/// decoding would need an escaping table and links in local corpora rarely
/// use it; a miss is counted, not hidden).
pub(crate) fn resolve_local<'a>(
    corpus: &'a CorpusIndex,
    containing_dir: &str,
    url: &str,
) -> LinkTarget<'a> {
    let url = url.trim();
    if url.is_empty() {
        return LinkTarget::Empty;
    }
    if url.starts_with("//") || has_scheme(url) {
        return LinkTarget::External;
    }
    let path = url.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() {
        return LinkTarget::Empty;
    }
    let candidates: [Option<String>; 2] = if let Some(site_abs) = path.strip_prefix('/') {
        // Site-absolute → root-relative only; joining it under the containing
        // dir would invent a path the author never wrote.
        [normalize_rel(site_abs), None]
    } else {
        let joined = if containing_dir.is_empty() {
            path.to_string()
        } else {
            format!("{containing_dir}/{path}")
        };
        [normalize_rel(&joined), normalize_rel(path)]
    };
    for cand in candidates.into_iter().flatten() {
        if let Some(f) = corpus.files.get(&cand) {
            return LinkTarget::Hit(f);
        }
    }
    LinkTarget::Miss
}

/// Any ':' before the first '/' marks the target external ("https:", "mailto:",
/// "tel:", "host:8080"…). Corpus rel paths essentially never contain a colon,
/// and a rule this blunt is deterministic where scheme-grammar parsing would
/// accumulate special cases.
fn has_scheme(url: &str) -> bool {
    match (url.find(':'), url.find('/')) {
        (Some(c), Some(s)) => c < s,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Normalize `a/./b/../c` → `a/c`; `..` escaping the corpus root is a miss
/// (None), not a guess.
fn normalize_rel(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

// ─── envelope assembly + write path ──────────────────────────────────────

/// One edge ready for `_bulk`: the two ndjson lines plus the tag for
/// per-detector accounting.
pub struct AssembledEdge {
    pub detector: &'static str,
    pub edge_id: String,
    pub ndjson: Vec<u8>,
}

pub struct AssembleOutcome {
    pub edges: Vec<AssembledEdge>,
    /// Self-edges (src == dst) dropped by the assembler — counted, per §6.4.
    pub self_dropped: u64,
}

/// f32 → JSON number without widening artifacts: `0.3f32 as f64` is
/// 0.30000001192092896, which would poison every stored weight and the
/// doc-values compare downstream. The shortest f32 display ("0.3") parsed as
/// f64 is the number the contract fixtures assert.
fn clean_f64(v: f32) -> f64 {
    format!("{v}").parse().unwrap_or(f64::from(v))
}

/// Turn drafts into `_bulk` actions (§2.2 stored-type discipline: every
/// scalar a plain string/number, `invalid_at`/`expired_at` omitted — the
/// null_bitmap IS the "still valid" signal). `created_at_ms` is the one
/// non-deterministic field and never participates in `edge_id`.
pub fn assemble(drafts: &[EdgeDraft], edges_index: &str, created_at_ms: i64) -> AssembleOutcome {
    let mut edges = Vec::with_capacity(drafts.len());
    let mut self_dropped = 0u64;
    for d in drafts {
        if d.src == d.dst {
            self_dropped += 1;
            continue;
        }
        let id = edge_id(&d.src, d.edge_type, &d.dst, d.valid_at_ms);
        let tag = detector_tag_for(d.edge_type);
        let action = json!({"index": {"_index": edges_index, "_id": id}});
        let mut doc = json!({
            "edge_id": id,
            "src": d.src,
            "dst": d.dst,
            "type": d.edge_type,
            "weight": clean_f64(d.weight),
            "valid_at": d.valid_at_ms,
            "created_at": created_at_ms,
            "detector": tag,
            "confidence": clean_f64(d.confidence),
            "schema_version": EDGE_SCHEMA_VERSION,
            "src_file": d.src_file,
            "evidence": {
                "quote": d.quote,
                "source": d.src_file,
                "offset": d.offset,
            }
        });
        // Endpoint file-type labels: omitted (not null) when unknown, same
        // discipline as invalid_at — the API stream's hand-asserted edges
        // carry neither, and that absence must stay a null_bitmap fact.
        if let Some(obj) = doc.as_object_mut() {
            if !d.src_format.is_empty() {
                obj.insert("src_format".into(), json!(d.src_format));
            }
            if !d.dst_format.is_empty() {
                obj.insert("dst_format".into(), json!(d.dst_format));
            }
        }
        let mut ndjson = action.to_string().into_bytes();
        ndjson.push(b'\n');
        ndjson.extend_from_slice(doc.to_string().as_bytes());
        ndjson.push(b'\n');
        edges.push(AssembledEdge {
            detector: tag,
            edge_id: id,
            ndjson,
        });
    }
    AssembleOutcome {
        edges,
        self_dropped,
    }
}

/// Create-if-absent write of the §2.5 brain meta doc. `create` semantics via
/// a probing GET: a racing writer's meta doc must never be clobbered — it may
/// carry a different `nodes_index` some reader already resolved through.
pub fn ensure_brain_meta(
    es: &Es,
    edges_index: &str,
    brain: &str,
    nodes_index: &str,
    created_at_ms: i64,
) -> Result<()> {
    if es.get_doc(edges_index, BRAIN_META_ID)?.is_some() {
        return Ok(());
    }
    let action = json!({"create": {"_index": edges_index, "_id": BRAIN_META_ID}});
    let doc = json!({
        "meta_version": 1,
        "brain": brain,
        "nodes_index": nodes_index,
        "created_at": created_at_ms,
    });
    let mut body = action.to_string().into_bytes();
    body.push(b'\n');
    body.extend_from_slice(doc.to_string().as_bytes());
    body.push(b'\n');
    let outcome = es.bulk(body).context("write brain meta doc")?;
    if outcome.server_errors > 0 {
        return Err(anyhow!(
            "brain meta write failed: {}",
            outcome
                .first_server_error
                .as_deref()
                .unwrap_or("unknown server error")
        ));
    }
    // item_errors here can only be the create-conflict race — the doc exists,
    // which is exactly the state we want.
    Ok(())
}

/// Soft-invalidate every live edge whose keyword `field` equals `value`.
/// Each pass re-indexes its hits with `invalid_at = expired_at = now` and
/// refreshes, so repeat-until-empty converges; the pass bound turns a broken
/// endpoint into an error instead of an infinite loop. Callers pass `src_file`
/// (edges a file taught) or `dst` (edges pointing at a file's anchor node).
pub fn invalidate_edges_by_field(
    es: &Es,
    edges_index: &str,
    field: &str,
    value: &str,
    now_ms: i64,
) -> Result<u64> {
    const MAX_PASSES: usize = 1_000;
    let query = json!({
        "query": {"bool": {
            "filter": [{"term": {field: value}}],
            "must_not": [{"exists": {"field": "invalid_at"}}]
        }},
        "size": 1000,
        "_source": true
    });
    let mut total = 0u64;
    for _ in 0..MAX_PASSES {
        // `search_present`, not `search`: the exclusion sweep (#694) may call
        // this before the edges index has ever been created (it runs ahead of
        // the graph phase's `ensure_index`). A missing index is not a failure —
        // it simply holds no edges to invalidate. The replacement-invalidation
        // caller always ensures the index first, so its behaviour is unchanged
        // (the index is present → `Some`).
        let Some(resp) = es.search_present(edges_index, &query)? else {
            return Ok(total);
        };
        let hits = resp
            .pointer("/hits/hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if hits.is_empty() {
            return Ok(total);
        }
        let mut body = Vec::new();
        for hit in &hits {
            let id = hit
                .get("_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("edge hit without _id in {edges_index}"))?;
            let mut source = hit
                .get("_source")
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| anyhow!("edge {id} returned without _source"))?;
            source.insert("invalid_at".into(), json!(now_ms));
            source.insert("expired_at".into(), json!(now_ms));
            body.extend_from_slice(
                json!({"index": {"_index": edges_index, "_id": id}})
                    .to_string()
                    .as_bytes(),
            );
            body.push(b'\n');
            body.extend_from_slice(Value::Object(source).to_string().as_bytes());
            body.push(b'\n');
        }
        let outcome = es.bulk(body).context("invalidate prior edges")?;
        if outcome.server_errors > 0 || outcome.item_errors > 0 {
            return Err(anyhow!(
                "edge invalidation for {}={} was partial: {}",
                field,
                value,
                outcome
                    .first_server_error
                    .or(outcome.first_error)
                    .unwrap_or_else(|| "unknown bulk error".into())
            ));
        }
        total += hits.len() as u64;
        es.refresh(edges_index)?;
    }
    Err(anyhow!(
        "edge invalidation for {field}={value} still found live edges after {MAX_PASSES} passes"
    ))
}

/// Soft-invalidate every live edge a file taught (`src_file` == `rel`). The
/// replacement hook (§6.6.3): runs before this run rewrites the file's edges,
/// so it only matches prior generations. Also reused by the exclusion sweep.
pub fn invalidate_prior_edges(es: &Es, edges_index: &str, rel: &str, now_ms: i64) -> Result<u64> {
    invalidate_edges_by_field(es, edges_index, "src_file", rel, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §2.3 pin vector — xerj-api carries a byte-identical copy of `edge_id`;
    /// this shared vector is what keeps the two in lockstep.
    #[test]
    fn edge_id_pin_vector() {
        assert_eq!(
            edge_id("note-alpha", "wikilink", "note-beta", 1753600000000),
            "bef814a75bd3d914c3e561f610154304"
        );
    }

    #[test]
    fn edge_id_is_valid_at_sensitive() {
        let a = edge_id("a", "t", "b", 1);
        assert_eq!(a, edge_id("a", "t", "b", 1));
        assert_ne!(a, edge_id("a", "t", "b", 2));
        assert_ne!(a, edge_id("a", "u", "b", 1));
    }

    #[test]
    fn brain_names_reject_the_edges_suffix_and_bad_shapes() {
        assert!(validate_brain("notes").is_ok());
        assert!(validate_brain("a.b-c_9").is_ok());
        assert!(validate_brain("kb-edges").is_err());
        assert!(validate_brain("").is_err());
        assert!(validate_brain("-notes").is_err());
        assert!(validate_brain("Notes").is_err());
        assert!(validate_brain("a..b").is_err());
    }

    #[test]
    fn assembler_drops_self_edges_and_stamps_the_envelope() {
        let drafts = vec![
            EdgeDraft {
                src: "n1".into(),
                dst: "n1".into(),
                edge_type: wikilink::EDGE_TYPE,
                weight: 1.0,
                confidence: 0.95,
                valid_at_ms: 5,
                src_file: "a.md".into(),
                quote: "self".into(),
                offset: 0,
                src_format: "md".into(),
                dst_format: "md".into(),
            },
            EdgeDraft {
                src: "n1".into(),
                dst: "n2".into(),
                edge_type: samedir::EDGE_TYPE,
                weight: samedir::WEIGHT,
                confidence: samedir::CONFIDENCE,
                valid_at_ms: 5,
                src_file: "a.md".into(),
                quote: "a.md and b.md share directory .".into(),
                offset: 0,
                src_format: "md".into(),
                dst_format: "md".into(),
            },
        ];
        let out = assemble(&drafts, ".xerj-memory-t-edges", 99);
        assert_eq!(out.self_dropped, 1);
        assert_eq!(out.edges.len(), 1);
        let lines: Vec<&str> = std::str::from_utf8(&out.edges[0].ndjson)
            .unwrap()
            .lines()
            .collect();
        let doc: Value = serde_json::from_str(lines[1]).unwrap();
        // f32 weights must land as their short decimal, not the widened f64.
        assert_eq!(doc["weight"], json!(0.3));
        assert_eq!(doc["confidence"], json!(0.4));
        assert_eq!(doc["detector"], json!(samedir::TAG));
        assert_eq!(doc["schema_version"], json!(1));
        assert_eq!(doc["created_at"], json!(99));
        assert_eq!(doc["evidence"]["source"], json!("a.md"));
        assert!(
            doc.get("invalid_at").is_none(),
            "unset keys must be omitted"
        );
        assert_eq!(doc["src_format"], json!("md"));
        assert_eq!(doc["dst_format"], json!("md"));
        assert_eq!(doc["edge_id"], json!(out.edges[0].edge_id));
    }

    #[test]
    fn unknown_endpoint_formats_are_omitted_not_null() {
        let draft = EdgeDraft {
            src: "n1".into(),
            dst: "n2".into(),
            edge_type: wikilink::EDGE_TYPE,
            weight: 1.0,
            confidence: 0.95,
            valid_at_ms: 5,
            src_file: "a.md".into(),
            quote: "q".into(),
            offset: 0,
            src_format: String::new(),
            dst_format: String::new(),
        };
        let out = assemble(std::slice::from_ref(&draft), "i", 7);
        let lines: Vec<&str> = std::str::from_utf8(&out.edges[0].ndjson)
            .unwrap()
            .lines()
            .collect();
        let doc: Value = serde_json::from_str(lines[1]).unwrap();
        assert!(doc.get("src_format").is_none());
        assert!(doc.get("dst_format").is_none());
    }

    #[test]
    fn format_labels_prefer_extension_and_fall_back_to_family() {
        assert_eq!(format_of("docs/a.md", "txt-prose"), "md");
        assert_eq!(format_of("src/lib.rs", "txt-lines"), "rs");
        assert_eq!(format_of("gtm/brief.PDF", "pdf"), "pdf");
        assert_eq!(format_of("Makefile", "txt-lines"), "txt-lines");
        assert_eq!(format_of(".env", "txt-lines"), "txt-lines");
        assert_eq!(
            format_of("x.tar.reallylongextension", "binary"),
            "binary",
            "over-long extensions are not extensions"
        );
    }

    #[test]
    fn corpus_index_builds_name_and_crate_tables() {
        let corpus = CorpusIndex::build(vec![
            corpus_file(
                "engine/crates/xerj-fts/Cargo.toml",
                "k1",
                "d",
                "txt-lines",
                1,
            ),
            corpus_file(
                "engine/crates/xerj-fts/src/lib.rs",
                "k2",
                "d",
                "txt-lines",
                1,
            ),
            corpus_file(
                "engine/crates/xerj-api/src/lib.rs",
                "k3",
                "d",
                "txt-lines",
                1,
            ),
            corpus_file("Cargo.toml", "k4", "d", "txt-lines", 1),
        ]);
        assert_eq!(
            corpus.by_name["lib.rs"],
            vec![
                "engine/crates/xerj-api/src/lib.rs",
                "engine/crates/xerj-fts/src/lib.rs"
            ]
        );
        // Crate table keys on the containing directory's basename; the root
        // Cargo.toml has no containing directory name and must not register.
        assert_eq!(
            corpus.crate_dirs.get("xerj-fts"),
            Some(&vec!["engine/crates/xerj-fts/Cargo.toml".to_string()])
        );
        assert_eq!(corpus.crate_dirs.len(), 1);
    }

    #[test]
    fn assembly_is_deterministic_for_a_fixed_created_at() {
        let draft = EdgeDraft {
            src: "n1".into(),
            dst: "n2".into(),
            edge_type: mdlink::EDGE_TYPE,
            weight: mdlink::WEIGHT,
            confidence: mdlink::CONFIDENCE,
            valid_at_ms: 1753600000000,
            src_file: "a.md".into(),
            quote: "q".into(),
            offset: 3,
            src_format: "md".into(),
            dst_format: "md".into(),
        };
        let a = assemble(std::slice::from_ref(&draft), "i", 7);
        let b = assemble(std::slice::from_ref(&draft), "i", 7);
        assert_eq!(a.edges[0].ndjson, b.edges[0].ndjson);
    }

    #[test]
    fn local_resolution_handles_relative_root_and_external_targets() {
        let corpus = CorpusIndex::build(vec![
            corpus_file("docs/a.md", "k1", "d", "txt-prose", 1),
            corpus_file("b.md", "k2", "d", "txt-prose", 1),
        ]);
        assert!(matches!(
            resolve_local(&corpus, "docs", "../b.md"),
            LinkTarget::Hit(f) if f.rel == "b.md"
        ));
        assert!(matches!(
            resolve_local(&corpus, "", "docs/a.md#frag?q=1"),
            LinkTarget::Hit(f) if f.rel == "docs/a.md"
        ));
        assert!(matches!(
            resolve_local(&corpus, "docs", "/b.md"),
            LinkTarget::Hit(f) if f.rel == "b.md"
        ));
        assert!(matches!(
            resolve_local(&corpus, "docs", "missing.md"),
            LinkTarget::Miss
        ));
        assert!(matches!(
            resolve_local(&corpus, "", "https://example.com/b.md"),
            LinkTarget::External
        ));
        assert!(matches!(
            resolve_local(&corpus, "", "//cdn.example.com/x"),
            LinkTarget::External
        ));
        assert!(matches!(
            resolve_local(&corpus, "", "#anchor"),
            LinkTarget::Empty
        ));
        assert!(matches!(
            resolve_local(&corpus, "", "../../escape.md"),
            LinkTarget::Miss
        ));
    }
}
