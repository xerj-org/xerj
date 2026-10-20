//! Corpus Hub v2 — the question router (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md`,
//! step 5, "Router").
//!
//! Given a question, the working directory and the signed [`Catalogue`], decide
//! which corpus answers it — **before** anything is downloaded. Three kinds of
//! evidence are gathered, each on its own scale, then merged per slug:
//!
//! 1. **Project dependencies** (score `1.0`). The strongest signal: if the
//!    project the agent is standing in declares a dependency a corpus is built
//!    for, that corpus almost certainly holds the answer. Dependency names are
//!    read from the lockfiles found walking from `cwd` up to the git root
//!    ([`parse_project_deps`]).
//! 2. **Hosted federated search** (score `0.9 × hit/top`). When the hosted node
//!    is consulted, its ranked hits are normalised to the best one so a strong
//!    hosted match sits just under a dependency match. The node lives behind the
//!    [`HostedSearch`] trait so this module stays node-free and unit-testable.
//! 3. **Term sketch** (score `≤ 0.8`). Offline, from the catalogue alone: how
//!    many of the question's tokens the corpus's Bloom [`sketch`](crate::sketch)
//!    reports present, as a fraction of all tokens. A token present in more than
//!    a quarter of all corpora is common vocabulary and counts half, so the
//!    distinctive words drive the score.
//!
//! The merged scores decide: a dominant, high-scoring corpus is
//! [`Routing::Confident`]; a cluster of plausible ones is
//! [`Routing::Ambiguous`] (best-first, for the caller to disambiguate with
//! `--corpus`); nothing credible is [`Routing::NoCorpus`]. A corpus already
//! cached locally gets a small nudge so a tie breaks toward the pack the agent
//! already has.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;
use xerj_common::hub::Catalogue;

use crate::sketch;

/// Where a piece of [`Evidence`] came from. Kept on the evidence so a caller (or
/// a test) can see *why* a corpus was proposed, even though [`Routing`] itself
/// only carries slugs.
#[derive(Debug, Clone, PartialEq)]
pub enum EvidenceSource {
    /// The project declares this dependency name (which a corpus answers for).
    ProjectDependency(String),
    /// The hosted node's federated search returned this corpus.
    Hosted,
    /// The corpus's term sketch matched some of the question's tokens.
    Sketch,
}

/// One corpus proposed for a question, with the score it earned on its source's
/// scale. Several pieces may name the same slug; [`route`] keeps the max.
#[derive(Debug, Clone)]
pub struct Evidence {
    pub slug: String,
    pub score: f32,
    pub source: EvidenceSource,
}

/// The routing verdict. `Ambiguous` holds the candidate slugs best-first.
#[derive(Debug, Clone, PartialEq)]
pub enum Routing {
    /// One corpus clearly wins; answer from it.
    Confident(String),
    /// Several corpora are plausible (best-first); the caller must pick one.
    Ambiguous(Vec<String>),
    /// Nothing credible matched.
    NoCorpus,
}

/// The hosted node's federated search, behind a trait so the router is
/// node-free. Returns `(slug, best_hit_score)` pairs, best first.
pub trait HostedSearch {
    fn search(&self, question: &str) -> anyhow::Result<Vec<(String, f32)>>;
}

/// Score a dependency match earns (the strongest evidence).
const DEP_SCORE: f32 = 1.0;
/// Hosted hits are scaled by this, relative to the top hosted hit.
const HOSTED_WEIGHT: f32 = 0.9;
/// A perfect sketch match (every token present, none common) earns this.
const SKETCH_WEIGHT: f32 = 0.8;
/// A token present in more than this fraction of all corpora is common
/// vocabulary and contributes half.
const COMMON_FRACTION: f32 = 0.25;
/// What a common token contributes instead of a full `1.0`.
const COMMON_CREDIT: f32 = 0.5;
/// A cached corpus gets this added to its merged score.
const CACHED_BONUS: f32 = 0.05;
/// The top score must reach this to be `Confident`.
const CONFIDENT_MIN: f32 = 0.6;
/// `Confident` also needs the top to beat the runner-up by this factor.
const DOMINANCE: f32 = 1.5;
/// A slug must reach this to be an `Ambiguous` candidate.
const CANDIDATE_MIN: f32 = 0.3;

/// Route a question to a corpus.
///
/// `hosted` is consulted only when `Some`; `cached` lists slugs whose packs are
/// already on disk (they get a small bonus). The working directory `cwd` is
/// searched for lockfiles up to the git root.
pub fn route(
    question: &str,
    cwd: &Path,
    cat: &Catalogue,
    hosted: Option<&dyn HostedSearch>,
    cached: &[String],
) -> Routing {
    route_ranked(question, cwd, cat, hosted, cached).0
}

/// [`route`], plus every corpus that earned any evidence at all as
/// `(slug, merged score)`, best first. The verdict is exactly [`route`]'s; the
/// ranking is what a caller prints when the verdict is [`Routing::NoCorpus`] — the
/// closest corpora, so the asker can name one with `--corpus` instead of being told
/// only that nothing matched.
pub fn route_ranked(
    question: &str,
    cwd: &Path,
    cat: &Catalogue,
    hosted: Option<&dyn HostedSearch>,
    cached: &[String],
) -> (Routing, Vec<(String, f32)>) {
    let mut evidence: Vec<Evidence> = Vec::new();

    // 1. Project dependencies — any corpus the project already depends on.
    let deps = parse_project_deps(cwd);
    if !deps.is_empty() {
        let dep_set: HashSet<&str> = deps.iter().map(String::as_str).collect();
        for entry in &cat.corpora {
            if let Some(hit) = entry
                .dependency_names
                .iter()
                .find(|name| dep_set.contains(name.as_str()))
            {
                evidence.push(Evidence {
                    slug: entry.slug.clone(),
                    score: DEP_SCORE,
                    source: EvidenceSource::ProjectDependency(hit.clone()),
                });
            }
        }
    }

    // 2. Hosted federated search, normalised to the top hit.
    if let Some(node) = hosted {
        if let Ok(results) = node.search(question) {
            let top = results
                .iter()
                .map(|(_, s)| *s)
                .fold(f32::NEG_INFINITY, f32::max);
            if top > 0.0 {
                for (slug, score) in results {
                    evidence.push(Evidence {
                        slug,
                        score: HOSTED_WEIGHT * (score / top),
                        source: EvidenceSource::Hosted,
                    });
                }
            }
        }
    }

    // 3. Term sketch — offline token overlap against each corpus's Bloom filter.
    let tokens = tokenize(question);
    if !tokens.is_empty() && !cat.corpora.is_empty() {
        let total = tokens.len() as f32;
        let n = cat.corpora.len() as f32;

        // present[corpus][token]: does this corpus's sketch report the token?
        let present: Vec<Vec<bool>> = cat
            .corpora
            .iter()
            .map(|entry| {
                tokens
                    .iter()
                    .map(|t| sketch::contains(&entry.term_sketch, t))
                    .collect()
            })
            .collect();

        // How many corpora hold each token → down-weight common vocabulary.
        let mut col_counts = vec![0usize; tokens.len()];
        for row in &present {
            for (ti, &p) in row.iter().enumerate() {
                if p {
                    col_counts[ti] += 1;
                }
            }
        }
        let weights: Vec<f32> = col_counts
            .iter()
            .map(|&c| {
                if c as f32 / n > COMMON_FRACTION {
                    COMMON_CREDIT
                } else {
                    1.0
                }
            })
            .collect();

        for (entry, row) in cat.corpora.iter().zip(&present) {
            let num: f32 = row
                .iter()
                .zip(&weights)
                .filter_map(|(&p, &w)| p.then_some(w))
                .sum();
            if num > 0.0 {
                evidence.push(Evidence {
                    slug: entry.slug.clone(),
                    score: num / total * SKETCH_WEIGHT,
                    source: EvidenceSource::Sketch,
                });
            }
        }
    }

    decide(evidence, cached)
}

/// Merge evidence by slug (max score), apply the cached bonus, and reach a
/// verdict; the ranking it was reached from comes back with it.
fn decide(evidence: Vec<Evidence>, cached: &[String]) -> (Routing, Vec<(String, f32)>) {
    let mut best: HashMap<String, f32> = HashMap::new();
    for e in evidence {
        let score = e.score;
        best.entry(e.slug)
            .and_modify(|s| *s = s.max(score))
            .or_insert(score);
    }
    if best.is_empty() {
        return (Routing::NoCorpus, Vec::new());
    }
    for (slug, score) in best.iter_mut() {
        if cached.iter().any(|c| c == slug) {
            *score += CACHED_BONUS;
        }
    }

    // Best first; ties broken by slug so the order is deterministic.
    let mut ranked: Vec<(String, f32)> = best.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let top_score = ranked[0].1;
    let dominant = match ranked.get(1) {
        None => true,
        Some((_, second)) => top_score >= DOMINANCE * *second,
    };
    if top_score >= CONFIDENT_MIN && dominant {
        let slug = ranked[0].0.clone();
        return (Routing::Confident(slug), ranked);
    }

    let candidates: Vec<String> = ranked
        .iter()
        .filter(|(_, s)| *s >= CANDIDATE_MIN)
        .map(|(slug, _)| slug.clone())
        .collect();
    if candidates.len() >= 2 {
        return (Routing::Ambiguous(candidates), ranked);
    }

    (Routing::NoCorpus, ranked)
}

/// Tokenize a question into the terms matched against a sketch: each identifier
/// run (regex `[A-Za-z_][A-Za-z0-9_]{2,}`) in its original case **and**
/// lowercased, deduplicated, in order. The original case lets a symbol like
/// `LZ4_compress` match a sketch's symbol term; the lowercase form lets prose
/// match the sketch's lowercase vocabulary.
fn tokenize(question: &str) -> Vec<String> {
    let chars: Vec<char> = question.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_ident_char(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_ident_char(chars[i]) {
            i += 1;
        }
        let run = &chars[start..i];
        // The regex must start at a letter/underscore and span ≥3 chars.
        if let Some(p) = run.iter().position(|&c| is_ident_start(c)) {
            if run.len() - p >= 3 {
                let raw: String = run[p..].iter().collect();
                let lower = raw.to_ascii_lowercase();
                push_unique(&mut tokens, &mut seen, raw);
                push_unique(&mut tokens, &mut seen, lower);
            }
        }
    }
    tokens
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn push_unique(tokens: &mut Vec<String>, seen: &mut HashSet<String>, t: String) {
    if seen.insert(t.clone()) {
        tokens.push(t);
    }
}

/// Dependency names declared by the project, read from the lockfiles in `cwd`
/// and every parent up to and including the directory that holds `.git`.
/// Deduplicated, first-seen order. Missing or unparseable files are skipped.
///
/// Parsed: `Cargo.lock` (`[[package]]` names), `package.json`
/// (`dependencies` + `devDependencies` keys), `go.mod` (`require` paths — both
/// the full module path and its last segment), `requirements.txt` and
/// `pyproject.toml` `[project].dependencies` (the name before any version
/// specifier or extras).
pub fn parse_project_deps(cwd: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut dir = Some(cwd);
    while let Some(d) = dir {
        collect_dir(d, &mut names, &mut seen);
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    names
}

fn collect_dir(dir: &Path, names: &mut Vec<String>, seen: &mut HashSet<String>) {
    if let Ok(text) = fs::read_to_string(dir.join("Cargo.lock")) {
        add_all(parse_cargo_lock(&text), names, seen);
    }
    if let Ok(text) = fs::read_to_string(dir.join("package.json")) {
        add_all(parse_package_json(&text), names, seen);
    }
    if let Ok(text) = fs::read_to_string(dir.join("go.mod")) {
        add_all(parse_go_mod(&text), names, seen);
    }
    if let Ok(text) = fs::read_to_string(dir.join("requirements.txt")) {
        add_all(parse_requirements(&text), names, seen);
    }
    if let Ok(text) = fs::read_to_string(dir.join("pyproject.toml")) {
        add_all(parse_pyproject(&text), names, seen);
    }
}

fn add_all(found: Vec<String>, names: &mut Vec<String>, seen: &mut HashSet<String>) {
    for name in found {
        if !name.is_empty() && seen.insert(name.clone()) {
            names.push(name);
        }
    }
}

fn parse_cargo_lock(text: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Package>,
    }
    #[derive(Deserialize)]
    struct Package {
        #[serde(default)]
        name: String,
    }
    toml::from_str::<Lock>(text)
        .map(|lock| lock.package.into_iter().map(|p| p.name).collect())
        .unwrap_or_default()
}

fn parse_package_json(text: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Pkg {
        #[serde(default)]
        dependencies: serde_json::Map<String, serde_json::Value>,
        #[serde(default, rename = "devDependencies")]
        dev_dependencies: serde_json::Map<String, serde_json::Value>,
    }
    match serde_json::from_str::<Pkg>(text) {
        Ok(pkg) => pkg
            .dependencies
            .keys()
            .chain(pkg.dev_dependencies.keys())
            .cloned()
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn parse_go_mod(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = strip_line_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if in_block {
            if line == ")" {
                in_block = false;
            } else if let Some(path) = line.split_whitespace().next() {
                push_module(&mut out, path);
            }
        } else if line.starts_with("require (") {
            in_block = true;
        } else if let Some(rest) = line.strip_prefix("require ") {
            if let Some(path) = rest.split_whitespace().next() {
                push_module(&mut out, path);
            }
        }
    }
    out
}

fn push_module(out: &mut Vec<String>, path: &str) {
    let path = path.trim_matches('"');
    if path.is_empty() {
        return;
    }
    out.push(path.to_string());
    if let Some(segment) = path.rsplit('/').next() {
        if segment != path {
            out.push(segment.to_string());
        }
    }
}

fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

fn parse_requirements(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        // Skip blanks, comments and option lines (`-r`, `--index-url`, …).
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let name = leading_requirement_name(line);
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

// KNOWN SCOPE GAP: only PEP 621 `[project].dependencies` is parsed. Poetry's
// `[tool.poetry.dependencies]` table (a map keyed by name, not a PEP 508 list)
// is NOT read, so a pure-Poetry project contributes no dependency hints to
// routing. Left unimplemented deliberately — PEP 621 is the cross-tool standard;
// Poetry support is a separate change, not part of this fix.
fn parse_pyproject(text: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct PyProject {
        project: Option<Project>,
    }
    #[derive(Deserialize)]
    struct Project {
        #[serde(default)]
        dependencies: Vec<String>,
    }
    let parsed = match toml::from_str::<PyProject>(text) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    parsed
        .project
        .map(|p| {
            p.dependencies
                .iter()
                .map(|dep| leading_requirement_name(dep.trim()))
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The distribution name at the start of a requirement string: the leading run
/// of name characters, which stops at the first version specifier (`==`, `>=`,
/// `<`, `~=`, `!=`), extras (`[`), environment marker (`;`) or whitespace.
fn leading_requirement_name(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::{tempdir, TempDir};
    use xerj_common::hub::{Catalogue, CatalogueEntry, LicenceClass};

    // --- builders ---------------------------------------------------------

    fn entry(slug: &str, deps: &[&str], sketch_terms: &[&str]) -> CatalogueEntry {
        CatalogueEntry {
            slug: slug.to_string(),
            domain: "TEST".to_string(),
            topics: Vec::new(),
            ask_for: String::new(),
            pin_sha: "0".repeat(40),
            pin_date: "2026-01-01".to_string(),
            refresh_cadence_days: 7,
            licence_spdx: "Apache-2.0".to_string(),
            licence_class: LicenceClass::AdaptWithAttribution,
            pack_bytes: 0,
            pack_url: String::new(),
            pack_sha256: String::new(),
            sig_url: String::new(),
            dependency_names: deps.iter().map(|s| s.to_string()).collect(),
            quality_grade: None,
            term_sketch: sketch::build(sketch_terms.iter().map(|s| s.to_string())),
        }
    }

    fn catalogue(entries: Vec<CatalogueEntry>) -> Catalogue {
        Catalogue {
            format_version: 1,
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            corpora: entries,
        }
    }

    /// A temp dir that is a git root with no lockfiles: `parse_project_deps`
    /// stops there and returns nothing.
    fn empty_project() -> TempDir {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        dir
    }

    // --- parse_project_deps ----------------------------------------------

    #[test]
    fn parse_project_deps_reads_all_five_kinds_and_dedupes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();

        fs::write(
            root.join("Cargo.lock"),
            "version = 3\n\n[[package]]\nname = \"serde\"\nversion = \"1.0\"\n\n\
             [[package]]\nname = \"tokio\"\nversion = \"1.0\"\nsource = \"registry+x\"\n",
        )
        .unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"demo","dependencies":{"react":"^18.0.0"},"devDependencies":{"jest":"^29"}}"#,
        )
        .unwrap();
        fs::write(
            root.join("go.mod"),
            "module example.com/demo\n\ngo 1.21\n\n\
             require (\n\tgithub.com/pkg/errors v0.9.1\n\tgolang.org/x/sync v0.1.0 // indirect\n)\n\n\
             require github.com/spf13/cobra v1.8.0\n",
        )
        .unwrap();
        fs::write(
            root.join("requirements.txt"),
            "# a comment\nrequests==2.28.1\nnumpy>=1.20\nflask[async]\ndjango ; python_version < \"3.8\"\n-r other.txt\n",
        )
        .unwrap();
        fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"demo\"\ndependencies = [\"httpx>=0.24\", \"click\"]\n",
        )
        .unwrap();

        let deps = parse_project_deps(root);
        for expected in [
            "serde",
            "tokio",
            "react",
            "jest",
            "github.com/pkg/errors",
            "errors", // go.mod last segment
            "golang.org/x/sync",
            "sync", // go.mod last segment
            "github.com/spf13/cobra",
            "cobra", // go.mod last segment
            "requests",
            "numpy",
            "flask",
            "django",
            "httpx",
            "click",
        ] {
            assert!(
                deps.contains(&expected.to_string()),
                "expected {expected:?} in {deps:?}"
            );
        }

        // No duplicates.
        let mut sorted = deps.clone();
        sorted.sort();
        let mut unique = sorted.clone();
        unique.dedup();
        assert_eq!(sorted, unique, "duplicate dependency names in {deps:?}");
    }

    #[test]
    fn parse_project_deps_finds_parent_lockfile_from_subdir() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(
            root.join("Cargo.lock"),
            "[[package]]\nname = \"rootcrate\"\nversion = \"1.0\"\n",
        )
        .unwrap();

        let sub = root.join("crates").join("inner");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("requirements.txt"), "subpkg==1.0\n").unwrap();

        let deps = parse_project_deps(&sub);
        assert!(deps.contains(&"subpkg".to_string()), "{deps:?}");
        assert!(
            deps.contains(&"rootcrate".to_string()),
            "parent Cargo.lock not found walking up: {deps:?}"
        );
    }

    #[test]
    fn parse_project_deps_empty_when_no_lockfiles() {
        let proj = empty_project();
        assert!(parse_project_deps(proj.path()).is_empty());
    }

    // --- route ------------------------------------------------------------

    #[test]
    fn strong_sketch_hit_is_confident() {
        let cat = catalogue(vec![
            entry(
                "alpha",
                &[],
                &["frobnicate", "wizzle", "kelvin", "sprocket"],
            ),
            entry("beta", &[], &["banana", "orange", "grape", "melon"]),
            entry("gamma", &[], &["server", "socket", "packet", "router"]),
            entry("delta", &[], &["ledger", "balance", "invoice", "levy"]),
        ]);
        let proj = empty_project();
        let routing = route("frobnicate wizzle sprocket", proj.path(), &cat, None, &[]);
        assert_eq!(routing, Routing::Confident("alpha".to_string()));
    }

    #[test]
    fn project_dependency_forces_confident_even_with_weak_sketch() {
        let cat = catalogue(vec![
            entry("alpha", &["widgetlib"], &["frobnicate", "wizzle", "kelvin"]),
            entry("beta", &[], &["banana", "orange", "grape"]),
            entry("gamma", &[], &["server", "socket", "packet"]),
            entry("delta", &[], &["ledger", "balance", "invoice"]),
        ]);

        // Question touches alpha's sketch only weakly (one of two tokens) and no
        // other corpus — on its own that is NoCorpus.
        let question = "frobnicate glorptxngq";
        let without = route(question, empty_project().path(), &cat, None, &[]);
        assert_eq!(without, Routing::NoCorpus);

        // With the project depending on widgetlib, alpha is a certainty.
        let proj = empty_project();
        fs::write(
            proj.path().join("Cargo.lock"),
            "[[package]]\nname = \"widgetlib\"\nversion = \"1.0\"\n",
        )
        .unwrap();
        let with = route(question, proj.path(), &cat, None, &[]);
        assert_eq!(with, Routing::Confident("alpha".to_string()));
    }

    #[test]
    fn vague_question_matching_two_corpora_is_ambiguous() {
        let cat = catalogue(vec![
            entry("alpha", &[], &["aaa", "bbb", "ccc", "ddd"]),
            entry("beta", &[], &["aaa", "bbb", "eee", "fff"]),
            entry("gamma", &[], &["server", "socket", "packet", "router"]),
            entry("delta", &[], &["ledger", "balance", "invoice", "levy"]),
        ]);
        let proj = empty_project();
        let routing = route("aaa bbb", proj.path(), &cat, None, &[]);
        match routing {
            Routing::Ambiguous(slugs) => {
                assert!(
                    slugs.contains(&"alpha".to_string()) && slugs.contains(&"beta".to_string()),
                    "both candidates expected, got {slugs:?}"
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn nonsense_question_is_no_corpus() {
        let cat = catalogue(vec![
            entry("alpha", &[], &["aaa", "bbb"]),
            entry("beta", &[], &["ccc", "ddd"]),
            entry("gamma", &[], &["eee", "fff"]),
            entry("delta", &[], &["ggg", "hhh"]),
        ]);
        let proj = empty_project();
        let routing = route("zxqwptl vmkrbng unmatchable", proj.path(), &cat, None, &[]);
        assert_eq!(routing, Routing::NoCorpus);
    }

    #[test]
    fn hosted_search_boosts_into_confident() {
        struct Stub;
        impl HostedSearch for Stub {
            fn search(&self, _q: &str) -> anyhow::Result<Vec<(String, f32)>> {
                Ok(vec![("beta".to_string(), 12.0), ("gamma".to_string(), 3.0)])
            }
        }

        let cat = catalogue(vec![
            entry("alpha", &[], &["aaa", "bbb"]),
            entry("beta", &[], &["ccc", "ddd"]),
            entry("gamma", &[], &["eee", "fff"]),
            entry("delta", &[], &["ggg", "hhh"]),
        ]);
        let proj = empty_project();
        let question = "unrelated prose touching nothing";

        // Without the hosted node there is no evidence at all.
        assert_eq!(
            route(question, proj.path(), &cat, None, &[]),
            Routing::NoCorpus
        );

        // The hosted node's top hit (normalised to 0.9) wins outright.
        let stub = Stub;
        assert_eq!(
            route(question, proj.path(), &cat, Some(&stub), &[]),
            Routing::Confident("beta".to_string())
        );
    }

    #[test]
    fn cached_bonus_flips_a_borderline_match() {
        // alpha owns five distinct terms; the question has them plus two noise
        // tokens, so alpha scores 5/7 * 0.8 = 0.571 — under the 0.6 bar and
        // alone, hence NoCorpus. The +0.05 cached bonus lifts it over.
        let cat = catalogue(vec![
            entry("alpha", &[], &["aaa", "bbb", "ccc", "ddd", "eee"]),
            entry("beta", &[], &["ppp1", "ppp2", "ppp3", "ppp4", "ppp5"]),
            entry("gamma", &[], &["qqq1", "qqq2", "qqq3", "qqq4", "qqq5"]),
            entry("delta", &[], &["rrr1", "rrr2", "rrr3", "rrr4", "rrr5"]),
        ]);
        let proj = empty_project();
        let question = "aaa bbb ccc ddd eee noisetokenone noisetokentwo";

        assert_eq!(
            route(question, proj.path(), &cat, None, &[]),
            Routing::NoCorpus
        );
        assert_eq!(
            route(question, proj.path(), &cat, None, &["alpha".to_string()]),
            Routing::Confident("alpha".to_string())
        );
    }

    // --- tokenizer --------------------------------------------------------

    #[test]
    fn tokenizer_keeps_identifiers_and_lowercases() {
        let toks = tokenize("How does LZ4_compress handle a 64 byte block?");
        // "How" -> How/how, "does" -> does, "LZ4_compress" -> raw + lower,
        // "handle" -> handle, "byte" -> byte, "block" -> block.
        assert!(toks.contains(&"LZ4_compress".to_string()));
        assert!(toks.contains(&"lz4_compress".to_string()));
        assert!(toks.contains(&"how".to_string()));
        assert!(toks.contains(&"block".to_string()));
        // "a" and "64" are too short / not identifier-starting → dropped.
        assert!(!toks.iter().any(|t| t == "a"));
        assert!(!toks.iter().any(|t| t == "64"));
    }
}
