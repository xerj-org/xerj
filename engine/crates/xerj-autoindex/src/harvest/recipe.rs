//! The corpus recipe — the declarative half of `xerj corpus build`.
//!
//! A recipe says WHERE records come from, which of their fields carry
//! IDENTITY (so the same logical record arriving from three sources becomes
//! one), how fields merge across sources, and which derived fields to
//! compute. It is TOML on purpose: it is human-authored, commented — licence
//! reasoning belongs next to the source it justifies — and it ships verbatim
//! inside the built pack as provenance.
//!
//! The tool is deliberately schema-agnostic: the recipe maps whatever the
//! sources actually contain onto a small query envelope (`title`, `body`,
//! `defs`, …). Nothing here knows what a vulnerability is. The parse is
//! STRICT — unknown keys and bad values are loud errors, not silently
//! ignored defaults — because a typo'd `aliases` silently turns one unique
//! record into three, which is the exact failure this subsystem exists to
//! prevent. (Hub manifests are tolerant because they are untrusted input
//! validated elsewhere; a recipe is trusted author input and must round-trip
//! faithfully.)

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;

/// Reserved envelope key names. Every emitted record carries these; derived
/// fields and passthrough data may not clobber them (validation refuses a
/// recipe that tries — a silent clobber here would make `xerj code` and the
/// ES-compat query surface disagree about what a record is).
pub const RESERVED_KEYS: &[&str] = &[
    "id",
    "title",
    "body",
    "defs",
    "licence",
    "source",
    "sources",
    "source_url",
    "modified",
    "origin",
];

/// The envelope provenance keys used INSIDE the content store. Underscored
/// so they can never collide with a data field at emit time, where they are
/// mapped onto `source`/`sources`/`source_url`/`origin` and dropped.
pub(crate) const SRC: &str = "_src";
pub(crate) const SRC_URL: &str = "_src_url";
pub(crate) const SRC_PATH: &str = "_src_path";
pub(crate) const LICENCE: &str = "_licence";

/// Names every emitted record carries (the uniform-key invariant): the
/// RESERVED_KEYS above plus every passthrough and derived key the recipe
/// declares. Absent values are `null`/`[]`, never omitted — autoindex
/// clusters datasets by field-name set, so an omitted optional field would
/// move a record between datasets (and change its `_id`) across rebuilds.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub name: String,
    /// Free-text, from `[recipe] description`; lands in the pack manifest so
    /// a consumer browsing packs sees what the corpus IS without reading the
    /// recipe.
    pub description: String,
    pub envelope: Envelope,
    pub sources: Vec<SourceSpec>,
    pub identity: Identity,
    pub merge: Merge,
    pub derived: Vec<Derived>,
    pub emit: Emit,
    pub suggest: Suggest,
}

/// Which normalized keys feed the query envelope. All are "first non-empty
/// wins" lists evaluated per merged record, in merge-precedence order.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub id_from: Vec<String>,
    pub title_from: Vec<String>,
    pub body_join: Vec<String>,
    pub defs_from: Vec<String>,
    pub passthrough: bool,
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub slug: String,
    pub kind: SourceKind,
    /// `dir` sources: a path relative to the recipe file.
    pub path: Option<String>,
    /// `http-zip`/`http-json`/`git` sources: where the bytes come from. A git
    /// url may also be a local path (git itself accepts both — tests rely on it).
    pub url: Option<String>,
    /// `git` sources: a full commit sha (pinned), a branch or tag name, or
    /// absent to track the remote's default branch (HEAD).
    pub rev: Option<String>,
    pub glob: String,
    /// Optional top-level JSON object key holding the records array when a
    /// `.json` file is a wrapper catalog (e.g. CISA KEV's `"vulnerabilities"`).
    pub records_field: Option<String>,
    pub format: Format,
    /// Carried onto every record from this source as record DATA — the tool
    /// never enforces it; the pack author owns what goes in the pack.
    pub licence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Dir,
    HttpZip,
    HttpJson,
    Git,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// OSV v1.x (the `ossf/osv-schema` shape shared by osv.dev dumps and
    /// github-reviewed GHSA files). Curated extraction of the standard
    /// fields plus generic flattening of the rest.
    Osv,
    /// RustSec's native advisory: a `crates/<crate>/RUSTSEC-*.md` file —
    /// ```toml frontmatter tables plus markdown prose. The only source of
    /// the curated `affected.functions` paths (the osv-branch conversion
    /// drops them).
    RustsecMd,
    /// Already-flat JSON records: generic two-level flattening only. This is
    /// the "we have not written a format adapter for this yet" path.
    Flat,
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub edges: Vec<IdentityEdge>,
    /// Source precedence for choosing a cluster's canonical id; empty means
    /// source declaration order.
    pub canonical_source_order: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IdentityEdge {
    pub field: String,
    /// Array fields: union on EVERY element (`aliases`), not on the array
    /// as a whole.
    pub each: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Merge {
    pub precedence: Vec<String>,
    pub field_precedence: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct Derived {
    pub name: String,
    pub op: DerivedOp,
}

#[derive(Debug, Clone)]
pub enum DerivedOp {
    /// Collect every regex match over the listed fields (strings, and each
    /// element of string arrays) into a string array.
    RegexExtract {
        from: Vec<String>,
        re: Regex,
        unique: bool,
    },
    /// First matching rule's value for the field; `default` when nothing
    /// matches (or the field is absent).
    Map {
        from: String,
        rules: Vec<(Regex, String)>,
        default: String,
    },
    /// True when the field is present and non-empty.
    Present { from: String },
}

#[derive(Debug, Clone)]
pub struct Emit {
    pub shards: usize,
}

/// The suggester's knob. A recipe (not a CLI flag) on purpose: the sample
/// size changes the suggestion files' bytes, and the recipe ships inside
/// the pack — a pack stays reproducible from its own contents.
#[derive(Debug, Clone)]
pub struct Suggest {
    /// Records the suggester samples (stride over id-sorted records).
    pub sample: usize,
}

// ── raw (serde) layer ───────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    recipe: RawHeader,
    #[serde(default)]
    envelope: RawEnvelope,
    sources: Vec<RawSource>,
    identity: RawIdentity,
    #[serde(default)]
    merge: RawMerge,
    #[serde(default)]
    derived: Vec<RawDerived>,
    #[serde(default)]
    emit: RawEmit,
    #[serde(default)]
    suggest: RawSuggest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHeader {
    format: u32,
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvelope {
    #[serde(default = "d_id_from")]
    id_from: Vec<String>,
    #[serde(default = "d_title_from")]
    title_from: Vec<String>,
    #[serde(default)]
    body_join: Vec<String>,
    #[serde(default)]
    defs_from: Vec<String>,
    #[serde(default = "d_true")]
    passthrough: bool,
}

// `#[serde(default)]` on the whole-table fields in RawRecipe falls back to
// `Default::default()` when the table is absent — which would silently zero
// these. The manual impls keep absent-table defaults equal to field defaults.
impl Default for RawEnvelope {
    fn default() -> Self {
        RawEnvelope {
            id_from: d_id_from(),
            title_from: d_title_from(),
            body_join: Vec::new(),
            defs_from: Vec::new(),
            passthrough: d_true(),
        }
    }
}

fn d_id_from() -> Vec<String> {
    vec!["id".to_string()]
}
fn d_title_from() -> Vec<String> {
    vec!["id".to_string()]
}
fn d_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    slug: String,
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    rev: Option<String>,
    #[serde(default = "d_glob")]
    glob: String,
    #[serde(default)]
    records_field: Option<String>,
    format: String,
    #[serde(default)]
    licence: String,
}

fn d_glob() -> String {
    "**/*.json".to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIdentity {
    edges: Vec<RawEdge>,
    #[serde(default)]
    canonical_source_order: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEdge {
    field: String,
    #[serde(default)]
    each: bool,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMerge {
    #[serde(default)]
    precedence: Vec<String>,
    #[serde(default)]
    field_precedence: HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDerived {
    name: String,
    op: String,
    #[serde(default)]
    from: Vec<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    rules: Vec<RawMapRule>,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    unique: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMapRule {
    r#match: String,
    value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmit {
    #[serde(default = "d_shards")]
    shards: usize,
}

impl Default for RawEmit {
    fn default() -> Self {
        RawEmit { shards: d_shards() }
    }
}

fn d_shards() -> usize {
    16
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuggest {
    #[serde(default = "d_sample")]
    sample: usize,
}

// absent `[suggest]` table = field defaults, not Default::default() zero —
// the same trap RawEnvelope's manual impl documents.
impl Default for RawSuggest {
    fn default() -> Self {
        RawSuggest { sample: d_sample() }
    }
}

fn d_sample() -> usize {
    512
}

// ── load + validate + compile ───────────────────────────────────────────────

/// A slug becomes a filename component in the content store
/// (`<slug>.<key>.json`), so it is gated to a safe charset the way corpus
/// and repo names are — but locally, because slugs are a builder concept.
fn valid_slug(slug: &str) -> Result<()> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !slug.starts_with('.');
    if !ok {
        bail!("source slug '{slug}' must be 1-64 chars of [A-Za-z0-9._-], not starting with '.'");
    }
    Ok(())
}

/// Read, validate and compile a recipe. Every error names the file and the
/// offending key — a recipe that fails here must never half-run.
pub fn load(path: &Path) -> Result<Recipe> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read recipe {}", path.display()))?;
    let file: RawRecipe = toml::from_str(&raw).map_err(|e| {
        // the toml error names the unknown key/line — keep it in the message
        anyhow::anyhow!("{}: recipe parse error: {e}", path.display())
    })?;
    compile(path, file)
}

fn compile(path: &Path, file: RawRecipe) -> Result<Recipe> {
    let at = |what: &str| format!("{}: {}", path.display(), what);

    if file.recipe.format != 1 {
        bail!(at(&format!(
            "recipe format {} is not supported by this build (supported: 1)",
            file.recipe.format
        )));
    }
    let name = file.recipe.name.trim().to_string();
    if let Err(e) = xerj_common::xccode::pathgate::valid_corpus_name(&name) {
        bail!(at(&format!("recipe name '{name}': {e}")));
    }

    if file.sources.is_empty() {
        bail!(at("recipe declares no [[sources]]"));
    }
    let mut slugs: Vec<&str> = Vec::new();
    let mut sources = Vec::new();
    for s in &file.sources {
        valid_slug(&s.slug).with_context(|| at(&format!("source '{}'", s.slug)))?;
        if slugs.contains(&s.slug.as_str()) {
            bail!(at(&format!("duplicate source slug '{}'", s.slug)));
        }
        let kind = match s.kind.as_str() {
            "dir" => SourceKind::Dir,
            "http-zip" => SourceKind::HttpZip,
            "http-json" => SourceKind::HttpJson,
            "git" => SourceKind::Git,
            other => bail!(at(&format!(
                "source '{}': unknown kind '{other}' (supported: dir, http-zip, http-json, git)",
                s.slug
            ))),
        };
        let format = match s.format.as_str() {
            "osv" => Format::Osv,
            "rustsec-md" => Format::RustsecMd,
            "flat" => Format::Flat,
            other => bail!(at(&format!(
                "source '{}': unknown format '{other}' (supported: osv, rustsec-md, flat)",
                s.slug
            ))),
        };
        if let Some(rf) = &s.records_field {
            if rf.trim().is_empty() {
                bail!(at(&format!(
                    "source '{}': 'records_field' cannot be empty",
                    s.slug
                )));
            }
        }
        let url = s.url.as_deref().unwrap_or_default().to_string();
        let rev = s.rev.as_deref().unwrap_or_default().to_string();
        match kind {
            SourceKind::Dir => {
                if s.path.as_deref().unwrap_or("").is_empty() {
                    bail!(at(&format!(
                        "dir source '{}' needs a 'path' (relative to the recipe file)",
                        s.slug
                    )));
                }
                if let Some(p) = &s.path {
                    if Path::new(p).is_absolute() {
                        bail!(at(&format!(
                            "dir source '{}' path must be recipe-relative, got absolute '{p}'",
                            s.slug
                        )));
                    }
                }
                if !url.is_empty() || !rev.is_empty() {
                    bail!(at(&format!(
                        "dir source '{}' takes 'path', not 'url'/'rev'",
                        s.slug
                    )));
                }
            }
            SourceKind::HttpZip | SourceKind::HttpJson => {
                let kind_str = if kind == SourceKind::HttpZip {
                    "http-zip"
                } else {
                    "http-json"
                };
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    bail!(at(&format!(
                        "{kind_str} source '{}' needs an http(s) 'url', got '{url}'",
                        s.slug
                    )));
                }
                if s.path.is_some() || !rev.is_empty() {
                    bail!(at(&format!(
                        "{kind_str} source '{}' takes 'url' only — no 'path'/'rev'",
                        s.slug
                    )));
                }
            }
            SourceKind::Git => {
                if url.is_empty() {
                    bail!(at(&format!(
                        "git source '{}' needs a 'url' (a git URL or local path)",
                        s.slug
                    )));
                }
                if s.path.is_some() {
                    bail!(at(&format!(
                        "git source '{}' takes 'url' (+optional 'rev'), not 'path'",
                        s.slug
                    )));
                }
            }
        }
        slugs.push(&s.slug);
        sources.push(SourceSpec {
            slug: s.slug.clone(),
            kind,
            path: s.path.clone(),
            url: s.url.clone(),
            rev: if rev.is_empty() { None } else { Some(rev) },
            glob: s.glob.clone(),
            records_field: s.records_field.clone(),
            format,
            licence: s.licence.clone(),
        });
    }

    if file.identity.edges.is_empty() {
        bail!(at(
            "[identity] declares no edges: without identity edges every record is its \
             own cluster and nothing deduplicates"
        ));
    }
    let edges = file
        .identity
        .edges
        .iter()
        .map(|e| IdentityEdge {
            field: e.field.clone(),
            each: e.each,
        })
        .collect();

    let known = |s: &str| slugs.contains(&s);
    for s in &file.identity.canonical_source_order {
        if !known(s) {
            bail!(at(&format!(
                "identity.canonical_source_order names source '{s}' which the recipe \
                 does not declare"
            )));
        }
    }
    for s in &file.merge.precedence {
        if !known(s) {
            bail!(at(&format!(
                "merge.precedence names source '{s}' which the recipe does not declare"
            )));
        }
    }
    for (field, order) in &file.merge.field_precedence {
        for s in order {
            if !known(s) {
                bail!(at(&format!(
                    "merge.field_precedence.{field} names source '{s}' which the recipe \
                     does not declare"
                )));
            }
        }
    }

    let mut derived = Vec::new();
    for d in &file.derived {
        if RESERVED_KEYS.contains(&d.name.as_str()) {
            bail!(at(&format!(
                "derived field '{}' collides with a reserved envelope key",
                d.name
            )));
        }
        let op = match d.op.as_str() {
            "regex_extract" => {
                let pattern = d.pattern.as_deref().unwrap_or("");
                if pattern.is_empty() {
                    bail!(at(&format!(
                        "derived '{}': regex_extract needs a 'pattern'",
                        d.name
                    )));
                }
                if d.from.is_empty() {
                    bail!(at(&format!(
                        "derived '{}': regex_extract needs a 'from' list",
                        d.name
                    )));
                }
                let re = Regex::new(pattern)
                    .with_context(|| at(&format!("derived '{}': bad regex /{pattern}/", d.name)))?;
                DerivedOp::RegexExtract {
                    from: d.from.clone(),
                    re,
                    unique: d.unique,
                }
            }
            "map" => {
                if d.from.len() != 1 {
                    bail!(at(&format!(
                        "derived '{}': map needs exactly one 'from' field",
                        d.name
                    )));
                }
                if d.rules.is_empty() {
                    bail!(at(&format!(
                        "derived '{}': map needs at least one rule",
                        d.name
                    )));
                }
                let mut rules = Vec::new();
                for r in &d.rules {
                    let re = Regex::new(&r.r#match).with_context(|| {
                        at(&format!(
                            "derived '{}': bad rule regex /{}/",
                            d.name, r.r#match
                        ))
                    })?;
                    rules.push((re, r.value.clone()));
                }
                DerivedOp::Map {
                    from: d.from[0].clone(),
                    rules,
                    default: d.default.clone().unwrap_or_default(),
                }
            }
            "present" => {
                if d.from.len() != 1 {
                    bail!(at(&format!(
                        "derived '{}': present needs exactly one 'from' field",
                        d.name
                    )));
                }
                DerivedOp::Present {
                    from: d.from[0].clone(),
                }
            }
            other => bail!(at(&format!(
                "derived '{}': unknown op '{other}' (supported: regex_extract, map, present)",
                d.name
            ))),
        };
        derived.push(Derived {
            name: d.name.clone(),
            op,
        });
    }

    if file.emit.shards == 0 || file.emit.shards > 4096 {
        bail!(at(&format!(
            "emit.shards must be 1..=4096, got {}",
            file.emit.shards
        )));
    }
    if file.suggest.sample < 16 || file.suggest.sample > 100_000 {
        bail!(at(&format!(
            "suggest.sample must be 16..=100000, got {}",
            file.suggest.sample
        )));
    }

    Ok(Recipe {
        name,
        description: file.recipe.description,
        envelope: Envelope {
            id_from: file.envelope.id_from,
            title_from: file.envelope.title_from,
            body_join: file.envelope.body_join,
            defs_from: file.envelope.defs_from,
            passthrough: file.envelope.passthrough,
        },
        sources,
        identity: Identity {
            edges,
            canonical_source_order: file.identity.canonical_source_order,
        },
        merge: file.merge.into_recipe(),
        derived,
        emit: Emit {
            shards: file.emit.shards,
        },
        suggest: Suggest {
            sample: file.suggest.sample,
        },
    })
}

impl RawMerge {
    fn into_recipe(self) -> Merge {
        Merge {
            precedence: self.precedence,
            field_precedence: self.field_precedence,
        }
    }
}

/// Rank of a source slug under an order list (absent = last). Members of a
/// cluster are sorted by this before any first-wins merge decision, so the
/// order list — not filesystem accident — decides whose value wins.
pub fn slug_rank(order: &[String], slug: &str) -> usize {
    order
        .iter()
        .position(|s| s == slug)
        .unwrap_or(order.len() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(tmp: &std::path::Path, body: &str) -> std::path::PathBuf {
        let p = tmp.join("recipe.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    const MINIMAL: &str = r#"
[recipe]
format = 1
name = "demo"

[[sources]]
slug = "a"
kind = "dir"
path = "data/a"
format = "flat"

[identity]
edges = [{ field = "id" }]
"#;

    #[test]
    fn minimal_recipe_loads_with_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let r = load(&write(tmp.path(), MINIMAL)).unwrap();
        assert_eq!(r.name, "demo");
        assert_eq!(r.envelope.id_from, vec!["id".to_string()]);
        assert!(r.envelope.passthrough);
        assert_eq!(r.emit.shards, 16);
        assert_eq!(
            r.suggest.sample, 512,
            "absent [suggest] table = field default"
        );
        assert_eq!(r.sources[0].glob, "**/*.json");
    }

    #[test]
    fn suggest_sample_bounds_are_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in ["8", "200000"] {
            let r = MINIMAL.replace(
                "[identity]",
                &format!("[suggest]\nsample = {bad}\n\n[identity]"),
            );
            let err = load(&write(tmp.path(), &r)).unwrap_err().to_string();
            assert!(err.contains("suggest.sample"), "{err}");
        }
        let ok = MINIMAL.replace("[identity]", "[suggest]\nsample = 64\n\n[identity]");
        assert_eq!(load(&write(tmp.path(), &ok)).unwrap().suggest.sample, 64);
    }

    #[test]
    fn unknown_keys_are_loud() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = MINIMAL.replace("format = \"flat\"", "format = \"flat\"\nbogus_key = 1");
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("bogus_key"), "{err}");
    }

    #[test]
    fn unknown_source_kind_is_loud() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = MINIMAL.replace("kind = \"dir\"", "kind = \"svn\"");
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("unknown kind 'svn'"), "{err}");
    }

    #[test]
    fn network_kinds_validate_their_targets() {
        let tmp = tempfile::tempdir().unwrap();
        // http-zip without an http(s) url
        let bad = MINIMAL.replace(
            "kind = \"dir\"\npath = \"data/a\"",
            "kind = \"http-zip\"\nurl = \"ftp://x/y.zip\"",
        );
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("http(s) 'url'"), "{err}");
        // git with a stray path
        let bad = MINIMAL.replace("kind = \"dir\"", "kind = \"git\"\nurl = \"https://x/y\"");
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("not 'path'"), "{err}");
        // dir with a url
        let bad = MINIMAL.replace(
            "path = \"data/a\"",
            "path = \"data/a\"\nurl = \"https://x/y\"",
        );
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("not 'url'/'rev'"), "{err}");
    }

    #[test]
    fn identity_edges_are_mandatory() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = MINIMAL.replace("edges = [{ field = \"id\" }]", "edges = []");
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("declares no edges"), "{err}");
    }

    #[test]
    fn precedence_must_name_declared_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = MINIMAL.replace(
            "[identity]",
            "[merge]\nprecedence = [\"nope\"]\n\n[identity]",
        );
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("'nope'"), "{err}");
    }

    #[test]
    fn derived_name_may_not_clobber_the_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let bad =
            format!("{MINIMAL}\n[[derived]]\nname = \"title\"\nop = \"present\"\nfrom = [\"x\"]\n");
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("reserved envelope key"), "{err}");
    }

    #[test]
    fn bad_regex_is_rejected_at_load_not_at_build() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = format!(
            "{MINIMAL}\n[[derived]]\nname = \"cves\"\nop = \"regex_extract\"\nfrom = \
             [\"id\"]\npattern = \"([unclosed\"\n"
        );
        let err = load(&write(tmp.path(), &bad)).unwrap_err().to_string();
        assert!(err.contains("bad regex"), "{err}");
    }

    #[test]
    fn slugs_are_path_safe() {
        assert!(valid_slug("rustsec").is_ok());
        assert!(valid_slug("osv-crates-io").is_ok());
        assert!(valid_slug("../escape").is_err());
        assert!(valid_slug(".hidden").is_err());
        assert!(valid_slug("").is_err());
    }
}
