//! Corpus Hub v2 — the question router (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md`,
//! step 5, "Router").
//!
//! Given a question, the working directory and the signed [`Catalogue`], decide
//! which corpus answers it — **before** anything is downloaded.
//!
//! A question that names a language or a technology is decided from the
//! catalogue entry itself: the slug (its name), topics (its tags), a language
//! word in the description, and the rest of the description. An explicit name
//! such as "Swift" outweighs generic words ("use", "type", "map"). When one
//! corpus or a tight group of at most [`GROUP_MAX`] share that name, they are
//! the answer ([`Routing::Confident`] or [`Routing::Group`]) and are searched
//! together. The Bloom sketch is not consulted for that decision: on a large
//! catalogue its false positives make unrelated corpora tie with the one the
//! question named.
//!
//! Otherwise three other kinds of evidence are gathered, each on its own scale,
//! then merged per slug:
//!
//! 1. **Project dependencies** (score `1.0`). If the project the agent is
//!    standing in declares a dependency a corpus is built for, that corpus
//!    almost certainly holds the answer. Dependency names are read from the
//!    lockfiles found walking from `cwd` up to the git root
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
//! [`Routing::Confident`]; a cluster of good matches is
//! [`Routing::Ambiguous`] (best-first; the ask path shows at most
//! [`AMBIGUOUS_SHOWN`] after symbol evidence, and the caller disambiguates
//! with `--corpus`); a cluster that only weakly overlaps the question, or a
//! tie on some other word while the question names a project no entry
//! contains, is [`Routing::NoCoverage`]; nothing credible is
//! [`Routing::NoCorpus`]. A corpus already cached locally gets a small nudge so
//! a tie breaks toward the pack the agent already has. A corpus the question
//! names by slug still decides outright, ahead of the identity score.

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
    /// The question names this corpus by its slug (`… in lz4?`, `snappy's …`).
    Named,
}

/// One corpus proposed for a question, with the score it earned on its source's
/// scale. Several pieces may name the same slug; [`route`] keeps the max.
#[derive(Debug, Clone)]
pub struct Evidence {
    pub slug: String,
    pub score: f32,
    pub source: EvidenceSource,
}

/// The routing verdict. `Ambiguous` and `Group` hold slugs best-first.
#[derive(Debug, Clone, PartialEq)]
pub enum Routing {
    /// One corpus clearly wins; answer from it.
    Confident(String),
    /// A tight group of two or three corpora that share the question's
    /// distinctive name (best-first). Search them together.
    Group(Vec<String>),
    /// Several corpora are plausible (best-first). The ask path shows at most
    /// [`AMBIGUOUS_SHOWN`] after symbol evidence has had a chance to narrow
    /// the list; the caller must pick one.
    Ambiguous(Vec<String>),
    /// Several corpora overlapped the question, but none covers it: every
    /// candidate is a weak score, or the question names a project no catalogue
    /// entry contains and the tie is on some other word. Not a choice among
    /// packs — the topic is absent.
    NoCoverage,
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
/// A multi-candidate tie whose best score is below this is a weak overlap, not
/// a choice. It sits under a full overlap of common tokens
/// ([`COMMON_CREDIT`] × [`SKETCH_WEIGHT`] = 0.4), which stays
/// [`Routing::Ambiguous`], and above [`CANDIDATE_MIN`]. A minority overlap
/// (two tokens out of five scores 0.32) is [`Routing::NoCoverage`].
const AMBIGUOUS_GOOD_MIN: f32 = 0.35;
/// Identity-channel scores (name weight × idf) sit above this. Sketch, hosted
/// and dependency scores do not (those top out at 1.0, plus the cached bonus).
/// An unknown lowercase word may break an identity tie without breaking a
/// sketch tie.
const IDENTITY_SCORE_FLOOR: f32 = 2.0;
/// Score a corpus named in the question earns (it outranks everything: the
/// asker said which corpus they mean).
const NAMED_SCORE: f32 = 1.0;
/// Slugs shorter than this are never matched as a name (too likely to be a
/// stray word or abbreviation).
const NAMED_MIN_LEN: usize = 2;
/// How many corpora an ambiguous verdict names. The rest stay in the ranked
/// list a caller can still inspect; the asker is not shown a flat tie of dozens.
pub const AMBIGUOUS_SHOWN: usize = 5;
/// A name match (slug or language) is weighted this many times the token's idf.
const IDENTITY_NAME_WEIGHT: f32 = 4.0;
/// A topic / tag match.
const IDENTITY_TAG_WEIGHT: f32 = 3.0;
/// Any other description word. Generic words stay on this scale, so a name
/// match dominates them.
const IDENTITY_DESC_WEIGHT: f32 = 1.0;
/// At most this many corpora are searched together when they share the winning
/// name. More than this is a genuine ambiguity: the asker picks.
const GROUP_MAX: usize = 3;
/// Identity tokens shorter than this are dropped (`in`, `of`). A two-letter
/// slug is still matched by [`named_slugs`].
const IDENTITY_MIN_LEN: usize = 3;
/// A token seen in more than `max(DISTINCTIVE_FLOOR, ceil(this × N))` corpora
/// is common vocabulary and cannot by itself pick a corpus.
const DISTINCTIVE_FRACTION: f32 = 0.02;
const DISTINCTIVE_FLOOR: usize = 3;

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

    // 4. The corpus the question names. Asking "what is X in lz4?" names the
    //    corpus outright; the sketch alone could not see that, because the
    //    question's other words (and the name itself — zstd's sources mention
    //    lz4) also hit the neighbouring corpora. One named corpus decides; several
    //    ("lz4 or zstd?") are the candidates, in the order the evidence ranks them.
    let named = named_slugs(question, cat);
    for slug in &named {
        evidence.push(Evidence {
            slug: slug.clone(),
            score: NAMED_SCORE,
            source: EvidenceSource::Named,
        });
    }

    let (routing, ranked) = decide(evidence, cached);
    // A slug the question spells out decides before identity scoring. "lz4 or
    // zstd?" is the asker choosing, not a group to search together. That
    // choice is not rewritten into a coverage gap: the asker named the packs.
    match named.len() {
        1 => return (Routing::Confident(named[0].clone()), ranked),
        n if n > 1 => {
            let in_order: Vec<String> = ranked
                .iter()
                .filter(|(slug, _)| named.contains(slug))
                .map(|(slug, _)| slug.clone())
                .collect();
            return (Routing::Ambiguous(in_order), ranked);
        }
        _ => {}
    }
    if let Some((identity, identity_ranked)) = identity_route(question, cat) {
        return (
            apply_coverage(question, cat, identity, &identity_ranked),
            identity_ranked,
        );
    }
    (apply_coverage(question, cat, routing, &ranked), ranked)
}

/// Turn a tie or a miss into [`Routing::NoCoverage`] when the question names a
/// project the catalogue does not contain.
///
/// A capitalized, underscored, or digit-bearing token (`Phaser`,
/// `setInteractive`, `HTML5`) that no slug, topic, or description contains is
/// that project. A lowercase word does the same to an identity tie only (those
/// scores sit above [`IDENTITY_SCORE_FLOOR`]), so "how does phaser draw HTML?"
/// is not a choice among the HTML packs, while a sketch tie on words the
/// corpora actually contain stays [`Routing::Ambiguous`]. A slug the question
/// spells out is handled before this runs.
fn apply_coverage(
    question: &str,
    cat: &Catalogue,
    routing: Routing,
    ranked: &[(String, f32)],
) -> Routing {
    let proper = !uncovered_names(question, cat).is_empty();
    let identity_tie = ranked
        .first()
        .is_some_and(|(_, score)| *score > IDENTITY_SCORE_FLOOR);
    match routing {
        Routing::NoCorpus if proper => Routing::NoCoverage,
        Routing::Ambiguous(_) if proper || (identity_tie && has_unknown_word(question, cat)) => {
            Routing::NoCoverage
        }
        other => other,
    }
}

/// Tokens the question writes as a name — a capital, a digit, or an underscore
/// — that no catalogue entry's slug, topics, or description contains.
/// Lowercase how-to words are not names: "handle" must not turn a real HTML
/// tie into a coverage gap, while "Phaser" must.
fn uncovered_names(question: &str, cat: &Catalogue) -> Vec<String> {
    let known = catalogue_words(cat);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in question_raw_tokens(question) {
        let lower = raw.to_ascii_lowercase();
        if lower.len() < IDENTITY_MIN_LEN || is_identity_stopword(&lower) {
            continue;
        }
        let name_shaped = raw.contains('_')
            || raw
                .chars()
                .any(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if name_shaped && !known.contains(&lower) && seen.insert(lower.clone()) {
            out.push(lower);
        }
    }
    out
}

/// A lowercase word of the question that no catalogue entry contains and that
/// is not ordinary how-to English (`draw`, `parser`). "phaser" qualifies;
/// "handle" does not.
fn has_unknown_word(question: &str, cat: &Catalogue) -> bool {
    let known = catalogue_words(cat);
    question_raw_tokens(question).any(|raw| {
        let lower = raw.to_ascii_lowercase();
        lower.len() >= IDENTITY_MIN_LEN
            && !is_identity_stopword(&lower)
            && !is_common_question_word(&lower)
            && raw.chars().all(|c| c.is_ascii_lowercase())
            && !known.contains(&lower)
    })
}

fn catalogue_words(cat: &Catalogue) -> HashSet<String> {
    let mut known = HashSet::new();
    for entry in &cat.corpora {
        let profile = IdentityProfile::from_entry(entry);
        known.extend(profile.slug);
        known.extend(profile.tags);
        known.extend(profile.desc);
    }
    known
}

fn question_raw_tokens(question: &str) -> impl Iterator<Item = &str> {
    question
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map(|word| word.trim_matches(|c| c == '.' || c == '-'))
        .filter(|word| !word.is_empty())
}

/// Ordinary words of a how-to question. Not stopwords (those are dropped
/// earlier): these are content words that are not a project name, so an
/// unknown one does not mean the hub lacks the topic. Sorted for binary search.
fn is_common_question_word(word: &str) -> bool {
    const WORDS: &[&str] = &[
        "area",
        "block",
        "body",
        "call",
        "calls",
        "canvas",
        "code",
        "compress",
        "content",
        "data",
        "decoder",
        "default",
        "document",
        "draw",
        "element",
        "engine",
        "error",
        "file",
        "files",
        "format",
        "function",
        "game",
        "games",
        "handle",
        "handles",
        "input",
        "internals",
        "level",
        "line",
        "lines",
        "loop",
        "method",
        "mode",
        "name",
        "names",
        "object",
        "objects",
        "parser",
        "path",
        "point",
        "queries",
        "query",
        "read",
        "reference",
        "render",
        "return",
        "size",
        "state",
        "text",
        "time",
        "type",
        "types",
        "value",
        "values",
        "version",
        "window",
        "word",
        "words",
        "work",
        "write",
        "writing",
    ];
    WORDS.binary_search(&word).is_ok()
}

/// Shorten an [`Routing::Ambiguous`] list to [`AMBIGUOUS_SHOWN`], best first.
/// Other verdicts pass through. Applied after symbol evidence, so a definition
/// that lives in one of the candidates can still narrow the full list.
pub fn cap_ambiguous(routing: Routing) -> Routing {
    match routing {
        Routing::Ambiguous(slugs) if slugs.len() > AMBIGUOUS_SHOWN => {
            Routing::Ambiguous(slugs.into_iter().take(AMBIGUOUS_SHOWN).collect())
        }
        other => other,
    }
}

/// One corpus scored against the question's identity terms.
struct IdentityScore {
    slug: String,
    score: f32,
    /// Distinctive slug, tag, or language tokens, `(token, contribution)`.
    identity: Vec<(String, f32)>,
}

/// Route from the catalogue entry itself: slug, topics, language, description.
///
/// `None` means no distinctive name matched, and the sketch / dependency /
/// hosted evidence should decide. A match returns the verdict and every
/// corpus that scored, best first.
fn identity_route(question: &str, cat: &Catalogue) -> Option<(Routing, Vec<(String, f32)>)> {
    let tokens = identity_query_tokens(question);
    if tokens.is_empty() || cat.corpora.is_empty() {
        return None;
    }
    let n = cat.corpora.len();
    let profiles: Vec<IdentityProfile> = cat
        .corpora
        .iter()
        .map(IdentityProfile::from_entry)
        .collect();

    let mut df: HashMap<String, usize> = HashMap::new();
    for profile in &profiles {
        let mut seen: HashSet<&str> = HashSet::new();
        for token in profile
            .slug
            .iter()
            .chain(profile.tags.iter())
            .chain(profile.desc.iter())
        {
            if seen.insert(token.as_str()) {
                *df.entry(token.clone()).or_default() += 1;
            }
        }
    }

    let mut scored: Vec<IdentityScore> = Vec::new();
    for (entry, profile) in cat.corpora.iter().zip(&profiles) {
        let mut score = 0.0;
        let mut identity: Vec<(String, f32)> = Vec::new();
        for token in &tokens {
            let Some(&docs) = df.get(token) else {
                continue;
            };
            let idf = ((n as f32 + 1.0) / (docs as f32 + 1.0)).ln();
            let (weight, is_name) = if profile.slug.contains(token) {
                (IDENTITY_NAME_WEIGHT, true)
            } else if profile.tags.contains(token) {
                (IDENTITY_TAG_WEIGHT, true)
            } else if profile.desc.contains(token) && is_language(token) {
                (IDENTITY_NAME_WEIGHT, true)
            } else if profile.desc.contains(token) {
                (IDENTITY_DESC_WEIGHT, false)
            } else {
                continue;
            };
            let contribution = weight * idf;
            score += contribution;
            if is_name && distinctive(docs, n) {
                identity.push((token.clone(), contribution));
            }
        }
        if score > 0.0 {
            scored.push(IdentityScore {
                slug: entry.slug.clone(),
                score,
                identity,
            });
        }
    }
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.slug.cmp(&b.slug))
    });
    let ranked: Vec<(String, f32)> = scored.iter().map(|s| (s.slug.clone(), s.score)).collect();

    // The winning term is the strongest distinctive name anywhere, not the
    // highest total: a rare description word must not outvote "Swift".
    let mut best_term: Option<(&str, f32)> = None;
    for row in &scored {
        for (token, contribution) in &row.identity {
            let replace = match best_term {
                None => true,
                Some((prev, prev_score)) => match contribution.partial_cmp(&prev_score) {
                    Some(std::cmp::Ordering::Greater) => true,
                    Some(std::cmp::Ordering::Equal) => token.as_str() < prev,
                    _ => false,
                },
            };
            if replace {
                best_term = Some((token.as_str(), *contribution));
            }
        }
    }
    let (term, _) = best_term?;
    let term = term.to_string();
    let peers: Vec<&IdentityScore> = scored
        .iter()
        .filter(|row| row.identity.iter().any(|(token, _)| token == &term))
        .collect();
    if peers.is_empty() {
        return None;
    }
    if peers.len() > GROUP_MAX {
        let slugs = peers
            .into_iter()
            .take(AMBIGUOUS_SHOWN)
            .map(|row| row.slug.clone())
            .collect();
        return Some((Routing::Ambiguous(slugs), ranked));
    }
    if peers.len() == 1 {
        return Some((Routing::Confident(peers[0].slug.clone()), ranked));
    }
    let slugs = peers.into_iter().map(|row| row.slug.clone()).collect();
    Some((Routing::Group(slugs), ranked))
}

struct IdentityProfile {
    slug: HashSet<String>,
    tags: HashSet<String>,
    desc: HashSet<String>,
}

impl IdentityProfile {
    fn from_entry(entry: &xerj_common::hub::CatalogueEntry) -> Self {
        let mut slug = HashSet::new();
        let whole = entry.slug.to_ascii_lowercase();
        if whole.len() >= IDENTITY_MIN_LEN {
            slug.insert(whole);
        }
        for part in entry.slug.split(|c: char| !c.is_ascii_alphanumeric()) {
            let part = part.to_ascii_lowercase();
            if part.len() >= IDENTITY_MIN_LEN {
                slug.insert(part);
            }
        }
        let mut tags = HashSet::new();
        for topic in &entry.topics {
            tags.extend(identity_text_words(topic));
        }
        IdentityProfile {
            slug,
            tags,
            desc: identity_text_words(&entry.ask_for),
        }
    }
}

fn identity_text_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() >= IDENTITY_MIN_LEN)
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn identity_query_tokens(question: &str) -> Vec<String> {
    let lower = question.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for word in lower.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
    {
        let word = word.trim_matches(|c| c == '.' || c == '-');
        if word.len() < IDENTITY_MIN_LEN || is_identity_stopword(word) {
            continue;
        }
        if seen.insert(word.to_string()) {
            out.push(word.to_string());
        }
    }
    out
}

fn distinctive(docs: usize, corpora: usize) -> bool {
    let cap = ((DISTINCTIVE_FRACTION * corpora as f32).ceil() as usize).max(DISTINCTIVE_FLOOR);
    docs > 0 && docs <= cap
}

fn is_language(word: &str) -> bool {
    // Sorted. A language named in the description is the corpus's identity,
    // the same way the slug is: "writing Swift" is not a generic word.
    const LANGUAGES: &[&str] = &[
        "bash",
        "clojure",
        "cpp",
        "csharp",
        "css",
        "dart",
        "elixir",
        "elm",
        "erlang",
        "fortran",
        "fsharp",
        "golang",
        "groovy",
        "haskell",
        "html",
        "java",
        "javascript",
        "julia",
        "kotlin",
        "lisp",
        "lua",
        "nim",
        "ocaml",
        "perl",
        "php",
        "prolog",
        "python",
        "ruby",
        "rust",
        "scala",
        "scheme",
        "solidity",
        "sql",
        "swift",
        "typescript",
        "wasm",
        "zig",
    ];
    LANGUAGES.binary_search(&word).is_ok()
}

fn is_identity_stopword(word: &str) -> bool {
    // Sorted. Function words of a how-to question. Technical words (`async`,
    // `let`, `map`, `result`, `type`) are not here: idf and the name-channel
    // weight are what keep them from outvoting "Swift".
    const STOP: &[&str] = &[
        "about", "all", "also", "and", "any", "are", "but", "can", "could", "did", "does", "each",
        "for", "from", "had", "has", "have", "how", "into", "its", "just", "may", "might", "more",
        "most", "must", "not", "off", "only", "other", "our", "out", "over", "per", "same",
        "shall", "should", "some", "such", "than", "that", "the", "their", "them", "then", "they",
        "this", "too", "under", "use", "used", "using", "very", "via", "was", "were", "what",
        "when", "where", "which", "who", "why", "will", "with", "would", "you", "your",
    ];
    STOP.binary_search(&word).is_ok()
}

/// The catalogue slugs `question` names as a whole word, case-insensitively,
/// in catalogue order. A word is a run of slug characters (`a-z`, `0-9`, `.`,
/// `_`, `-`) with sentence punctuation (`.`, `-`) trimmed from its ends, so
/// `in lz4?`, `LZ4.`, `snappy's` and `c-ares` all name their corpus while
/// `lz4frame` and `liblz4` do not.
pub fn named_slugs(question: &str, cat: &Catalogue) -> Vec<String> {
    let lower = question.to_ascii_lowercase();
    let words: HashSet<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .map(|w| w.trim_matches(|c| c == '.' || c == '-'))
        .filter(|w| w.len() >= NAMED_MIN_LEN)
        .collect();
    let mut out: Vec<String> = Vec::new();
    for entry in &cat.corpora {
        if words.contains(entry.slug.as_str()) && !out.contains(&entry.slug) {
            out.push(entry.slug.clone());
        }
    }
    out
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
        // A full overlap of common tokens still scores 0.4 and is a real tie.
        // A minority overlap clears the candidate floor and is not a choice.
        if top_score >= AMBIGUOUS_GOOD_MIN {
            return (Routing::Ambiguous(candidates), ranked);
        }
        return (Routing::NoCoverage, ranked);
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
            upstream: String::new(),
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
    fn a_minority_overlap_is_no_coverage_not_ambiguous() {
        // Eight corpora so a token shared by two is not common vocabulary
        // (the quarter cut). Five query tokens, two hits each: 2/5 × 0.8 = 0.32,
        // over the 0.3 candidate floor and under the 0.35 good-tie bar.
        let mut entries = vec![
            entry("alpha", &[], &["aaa", "bbb"]),
            entry("beta", &[], &["aaa", "ccc"]),
        ];
        for i in 0..6 {
            entries.push(entry(
                &format!("other{i}"),
                &[],
                &[&format!("uniq{i}a"), &format!("uniq{i}b")],
            ));
        }
        let cat = catalogue(entries);
        let routing = route(
            "aaa bbb ccc noisea noiseb",
            empty_project().path(),
            &cat,
            None,
            &[],
        );
        assert_eq!(routing, Routing::NoCoverage);
    }

    #[test]
    fn a_project_the_catalogue_does_not_contain_is_no_coverage() {
        // "html" is a language word in four descriptions. On a catalogue large
        // enough for that to stay distinctive, the identity router used to
        // call the tie ambiguous. The question names Phaser, which no entry
        // contains, so the HTML packs do not cover it. A lowercase spelling
        // is the same gap. "HTML" alone is a real tie. An exact slug still wins.
        let mut entries = Vec::new();
        for slug in ["whatwg-html", "lol-html", "parse5", "jsdom"] {
            let mut e = entry(slug, &[], &[]);
            e.ask_for = "An agent writing HTML queries this for the document format.".into();
            entries.push(e);
        }
        for i in 0..(151 - entries.len()) {
            let mut e = entry(&format!("filler-{i}"), &[], &[]);
            e.ask_for = "an agent working on storage queries this for internals".into();
            entries.push(e);
        }
        let mut lz4 = entry("lz4", &[], &["lz4", "block", "decoder"]);
        lz4.ask_for = "an agent needing fast block compression queries this for the decoder".into();
        entries.push(lz4);
        let cat = catalogue(entries);
        let proj = empty_project();

        assert_eq!(
            route("How does Phaser draw HTML?", proj.path(), &cat, None, &[]),
            Routing::NoCoverage
        );
        assert_eq!(
            route("how does phaser draw HTML?", proj.path(), &cat, None, &[]),
            Routing::NoCoverage
        );
        match route("HTML document format", proj.path(), &cat, None, &[]) {
            Routing::Ambiguous(slugs) => {
                assert!(slugs.len() >= 2, "{slugs:?}");
                assert!(slugs.iter().any(|s| s == "whatwg-html"), "{slugs:?}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        assert_eq!(
            route(
                "how does the lz4 decoder work",
                proj.path(),
                &cat,
                None,
                &[]
            ),
            Routing::Confident("lz4".to_string())
        );
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
    fn named_slugs_match_whole_words_only() {
        let cat = catalogue(vec![
            entry("lz4", &[], &[]),
            entry("c-ares", &[], &[]),
            entry("snappy", &[], &[]),
            entry("h2", &[], &[]),
        ]);
        let names = |q: &str| named_slugs(q, &cat);
        assert_eq!(names("What is LZ4_VERSION_RELEASE in lz4?"), ["lz4"]);
        assert_eq!(names("Does LZ4. handle it"), ["lz4"]);
        assert_eq!(names("how does snappy's decoder work"), ["snappy"]);
        assert_eq!(names("c-ares resolver timeouts"), ["c-ares"]);
        assert_eq!(names("lz4 vs snappy"), ["lz4", "snappy"]);
        // Part of a longer word or identifier is not the name.
        assert!(names("liblz4 and lz4frame and LZ4_compress").is_empty());
        assert!(names("snappyish ares").is_empty());
    }

    #[test]
    fn a_named_corpus_decides_even_against_a_dependency() {
        let cat = catalogue(vec![
            entry("alpha", &["widgetlib"], &["frobnicate", "wizzle"]),
            entry("beta", &[], &["frobnicate", "wizzle"]),
            entry("gamma", &[], &["server"]),
        ]);
        let proj = empty_project();
        fs::write(
            proj.path().join("Cargo.lock"),
            "[[package]]\nname = \"widgetlib\"\nversion = \"1.0\"\n",
        )
        .unwrap();
        let (routing, ranked) =
            route_ranked("how does beta frobnicate", proj.path(), &cat, None, &[]);
        assert_eq!(routing, Routing::Confident("beta".to_string()));
        assert!(ranked.iter().any(|(s, _)| s == "beta"), "{ranked:?}");
        // Two named: ambiguous between exactly those two.
        let both = route("alpha or beta?", empty_project().path(), &cat, None, &[]);
        match both {
            Routing::Ambiguous(mut slugs) => {
                slugs.sort();
                assert_eq!(slugs, ["alpha", "beta"]);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

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

    /// Every corpus's sketch reports every token of both Swift questions, which
    /// is the failure mode of a saturated Bloom filter: the sketch ties
    /// unrelated corpora with swift-book. The slug and the description still
    /// name Swift, so those two are the group and the others are not.
    #[test]
    fn a_language_term_picks_its_corpora_despite_sketch_false_positives() {
        const POISON: &[&str] = &[
            "how", "use", "async", "let", "swift", "Swift", "result", "Result", "type", "map",
        ];
        let ask = [
            (
                "swift-book",
                "An agent writing Swift queries this for The Swift Programming Language book: syntax and semantics of concurrency (async/await, async let, actors), generics, optionals, error handling and the language reference grammar.",
            ),
            (
                "swift-evolution",
                "An agent writing Swift queries this for the accepted Swift Evolution proposals (SE-NNNN): the motivation, exact semantics and API of language and standard-library features such as Result, async let, typed throws and macros.",
            ),
            (
                "qwik",
                "An agent minimising client JS queries this for Qwik's resumability model and code-splitting/lazy-loading of event handlers.",
            ),
            (
                "docker-docs",
                "An agent writing Dockerfiles and compose files queries this for the canonical Docker CLI, build and Compose reference docs.",
            ),
            (
                "harper",
                "An agent building a fast local grammar/style checker in Rust queries this for its rule engine, lint rules and curated dictionary (no cloud, no LLM).",
            ),
            (
                "oss-review-toolkit",
                "An agent building a dependency license-compliance pipeline (SBOM, policy, attribution) queries this for a production OSS Review Toolkit implementation.",
            ),
        ];
        let cat = catalogue(
            ask.into_iter()
                .map(|(slug, ask_for)| {
                    let mut terms: Vec<String> = ask_for
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .filter(|w| w.len() >= 3)
                        .map(|w| w.to_ascii_lowercase())
                        .collect();
                    terms.extend(POISON.iter().map(|s| (*s).to_string()));
                    let mut e = entry(slug, &[], &[]);
                    e.ask_for = ask_for.to_string();
                    e.term_sketch = sketch::build(terms);
                    e
                })
                .collect(),
        );
        let proj = empty_project();
        let group = |q| match route(q, proj.path(), &cat, None, &[]) {
            Routing::Group(slugs) => slugs,
            other => panic!("{q} expected a group, got {other:?}"),
        };
        assert_eq!(
            group("how do I use async let in Swift"),
            vec!["swift-book".to_string(), "swift-evolution".to_string()]
        );
        assert_eq!(
            group("Swift Result type map"),
            vec!["swift-evolution".to_string(), "swift-book".to_string()]
        );

        // An unrelated dependency must not steal a question that names Swift.
        fs::write(
            proj.path().join("Cargo.lock"),
            "[[package]]\nname = \"zstd\"\nversion = \"1.0\"\n",
        )
        .unwrap();
        let mut with_dep = cat.clone();
        // The zstd corpus is not in this catalogue; the dependency only matters
        // when a corpus lists it. Add one whose sketch is equally poisoned.
        let mut zstd = entry("zstd", &["zstd"], &[]);
        zstd.ask_for = "an agent tuning compression queries this for frame formats".to_string();
        zstd.term_sketch = sketch::build(POISON.iter().map(|s| (*s).to_string()));
        with_dep.corpora.push(zstd);
        assert_eq!(
            group_of("how do I use async let in Swift", &with_dep, proj.path()),
            vec!["swift-book".to_string(), "swift-evolution".to_string()]
        );
    }

    fn group_of(q: &str, cat: &Catalogue, cwd: &std::path::Path) -> Vec<String> {
        match route(q, cwd, cat, None, &[]) {
            Routing::Group(slugs) => slugs,
            other => panic!("{q} expected a group, got {other:?}"),
        }
    }

    #[test]
    fn cap_ambiguous_keeps_the_best_few_in_order() {
        let slugs: Vec<String> = (0..8).map(|i| format!("c{i}")).collect();
        let capped = cap_ambiguous(Routing::Ambiguous(slugs.clone()));
        assert_eq!(
            capped,
            Routing::Ambiguous(slugs.into_iter().take(AMBIGUOUS_SHOWN).collect())
        );
        assert_eq!(
            cap_ambiguous(Routing::Confident("only".into())),
            Routing::Confident("only".into())
        );
    }
}
