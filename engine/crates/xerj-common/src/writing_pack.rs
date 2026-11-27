//! Structure-first writing packets assembled from verified Corpus Hub evidence
//! (`write.outline` #2763 and `write.section` #2764, Epic #2769).
//!
//! `write.outline(topic, audience, length)` deterministically groups supplied
//! hub evidence by theme and partitions the groups into schema-valid chapters
//! (`thesis`, `evidence`, `source_ids`),
//! scales chapter count with [`OutlineLength`], emits a content-addressed
//! `outline_id`, and refuses insufficient evidence rather than padding.
//!
//! `write.section(outline_id, chapter)` resolves the chapter's sources (`facts`,
//! `quotes`, `counterpoints`, `style_rules`) with citable `source_id` and
//! `file:line`/byte-span provenance, includes at least one counterpoint when
//! the evidence pool contains one (never repeated as a fact or quote), fails
//! loudly on unknown `outline_id` or `chapter`, and bounds the packet in tokens
//! measured on its own RFC 8785 (JCS) serialization, sealed with a SHA-256
//! receipt.

use crate::hub_evidence::{sha256, PinnedCitation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

pub const OUTLINE_VERSION: &str = "xerj-write-outline/v1";
pub const SECTION_VERSION: &str = "xerj-write-section/v1";
pub const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum WritingPackError {
    #[error(
        "insufficient verified evidence: need at least {required} cited sources for this length, got {available}"
    )]
    InsufficientEvidence { required: usize, available: usize },
    #[error("unknown outline id: {0}")]
    UnknownOutlineId(String),
    #[error("unknown chapter: {0}")]
    UnknownChapter(String),
    #[error("token budget {budget} is too small; minimum required is {minimum}")]
    TokenBudgetTooSmall { budget: usize, minimum: usize },
    #[error("invalid writing packet: {0}")]
    Invalid(&'static str),
    #[error("writing packet JSON: {0}")]
    Json(String),
}

pub type WritePackError = WritingPackError;

impl From<serde_json::Error> for WritingPackError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, WritingPackError>;

fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(WritingPackError::Invalid(reason))
    }
}

fn hex_40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

/// Token estimate for one string: one token per four UTF-8 bytes, rounded up.
/// A section's token budget is not a sum of these: it is measured on the whole
/// serialized packet (see `packet_token_count`).
pub fn estimated_token_count(text: &str) -> usize {
    text.len().div_ceil(4).max(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stance {
    Supporting,
    Counterpoint,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutlineLength {
    Brief,
    Standard,
    #[serde(alias = "longform")]
    Comprehensive,
}

pub type TargetLength = OutlineLength;

impl OutlineLength {
    pub const LONGFORM: Self = Self::Comprehensive;

    pub fn chapter_count(self) -> usize {
        match self {
            Self::Brief => 1,
            Self::Standard => 3,
            Self::Comprehensive => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteSourceItem {
    pub source_id: String,
    pub corpus: String,
    pub pin_sha: String,
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub fact: String,
    pub quote: String,
    pub stance: Stance,
    pub pinned_citation: PinnedCitation,
}

impl WriteSourceItem {
    pub fn from_citation(
        source_id: String,
        fact: String,
        quote: String,
        stance: Stance,
        pinned_citation: PinnedCitation,
    ) -> Result<Self> {
        let item = Self {
            source_id,
            corpus: pinned_citation.corpus.clone(),
            pin_sha: pinned_citation.commit.clone(),
            file: pinned_citation.file.clone(),
            line_start: pinned_citation.line_start,
            line_end: pinned_citation.line_end,
            byte_start: pinned_citation.byte_start,
            byte_end: pinned_citation.byte_end,
            fact,
            quote,
            stance,
            pinned_citation,
        };
        item.validate()?;
        Ok(item)
    }

    fn validate(&self) -> Result<()> {
        require(
            !self.source_id.trim().is_empty()
                && !self.corpus.trim().is_empty()
                && hex_40(&self.pin_sha)
                && !self.file.trim().is_empty()
                && self.line_start > 0
                && self.line_start <= self.line_end
                && self.byte_start < self.byte_end
                && !self.fact.trim().is_empty()
                && !self.quote.trim().is_empty(),
            "source identity, location, byte span, pin, fact, or quote",
        )?;
        require(
            self.pinned_citation.corpus == self.corpus
                && self.pinned_citation.commit == self.pin_sha
                && self.pinned_citation.file == self.file
                && self.pinned_citation.line_start == self.line_start
                && self.pinned_citation.line_end == self.line_end
                && self.pinned_citation.byte_start == self.byte_start
                && self.pinned_citation.byte_end == self.byte_end,
            "pinned citation does not match source metadata",
        )
    }

    /// The theme a source is grouped under: its first four alphanumeric
    /// tokens (fact, then quote), lower-cased. Short tokens and digits are
    /// kept, so "theme 1" and "theme 2" are different themes.
    fn theme_key(&self) -> String {
        self.fact
            .split(|c: char| !c.is_alphanumeric())
            .chain(self.quote.split(|c: char| !c.is_alphanumeric()))
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .take(4)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Chapter boundaries over `keys`, the theme key of each source in sorted
/// order. Returns `chapter_count + 1` indices (`0` first, `keys.len()` last).
///
/// Sources sharing a theme key are consecutive and stay in one chapter: a
/// boundary falls between themes, as close as it can get to an even split. Only
/// when there are fewer themes than chapters is a theme split, the largest
/// first, so that no chapter is empty and none is padded. The caller has checked
/// that `keys.len() >= chapter_count`.
fn chapter_bounds(keys: &[String], chapter_count: usize) -> Vec<usize> {
    let total = keys.len();
    let mut cuts: Vec<usize> = (1..total).filter(|&i| keys[i] != keys[i - 1]).collect();
    while cuts.len() + 1 < chapter_count {
        let mut bounds = vec![0];
        bounds.extend(&cuts);
        bounds.push(total);
        let (start, len) =
            bounds
                .windows(2)
                .map(|w| (w[0], w[1] - w[0]))
                .fold(
                    (0, 0),
                    |best, next| if next.1 > best.1 { next } else { best },
                );
        let cut = start + len / 2;
        let at = cuts.partition_point(|&c| c < cut);
        cuts.insert(at, cut);
    }
    let mut bounds = vec![0];
    let mut next_candidate = 0;
    for k in 1..chapter_count {
        let target = (total * k).div_ceil(chapter_count);
        let last_allowed = cuts.len() - (chapter_count - k);
        let best = (next_candidate..=last_allowed)
            .min_by_key(|&j| (cuts[j].abs_diff(target), j))
            .unwrap_or(next_candidate);
        bounds.push(cuts[best]);
        next_candidate = best + 1;
    }
    bounds.push(total);
    bounds
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineChapter {
    pub chapter_id: String,
    pub title: String,
    pub thesis: String,
    pub evidence: Vec<String>,
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteOutline {
    pub report_version: String,
    pub topic: String,
    pub audience: String,
    pub length: OutlineLength,
    pub chapters: Vec<OutlineChapter>,
    pub sources: Vec<WriteSourceItem>,
    pub outline_id: String,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct OutlinePayload<'a> {
    report_version: &'a str,
    topic: &'a str,
    audience: &'a str,
    length: OutlineLength,
    chapters: &'a [OutlineChapter],
    sources: &'a [WriteSourceItem],
}

pub fn write_outline(
    topic: impl Into<String>,
    audience: impl Into<String>,
    length: OutlineLength,
    sources: Vec<WriteSourceItem>,
) -> Result<WriteOutline> {
    WriteOutline::new(topic.into(), audience.into(), length, sources)
}

impl WriteOutline {
    pub fn new(
        topic: String,
        audience: String,
        length: OutlineLength,
        mut sources: Vec<WriteSourceItem>,
    ) -> Result<Self> {
        for source in &sources {
            source.validate()?;
        }
        let chapter_count = length.chapter_count();
        if sources.len() < chapter_count {
            return Err(WritingPackError::InsufficientEvidence {
                required: chapter_count,
                available: sources.len(),
            });
        }
        require(
            !topic.trim().is_empty() && !audience.trim().is_empty(),
            "outline topic and audience",
        )?;
        sources.sort_by(|a, b| {
            a.theme_key()
                .cmp(&b.theme_key())
                .then_with(|| a.corpus.cmp(&b.corpus))
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line_start.cmp(&b.line_start))
                .then_with(|| a.source_id.cmp(&b.source_id))
        });
        let unique_sources: HashSet<_> = sources.iter().map(|s| &s.source_id).collect();
        require(unique_sources.len() == sources.len(), "duplicate source id")?;

        let keys: Vec<String> = sources.iter().map(WriteSourceItem::theme_key).collect();
        let bounds = chapter_bounds(&keys, chapter_count);
        let mut chapters = Vec::with_capacity(chapter_count);
        for index in 0..chapter_count {
            let cluster = &sources[bounds[index]..bounds[index + 1]];
            let primary = cluster
                .iter()
                .find(|s| s.stance == Stance::Supporting)
                .or_else(|| cluster.iter().find(|s| s.stance == Stance::Neutral))
                .unwrap_or(&cluster[0]);
            let title_words = primary
                .theme_key()
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            let evidence = cluster
                .iter()
                .map(|s| {
                    format!(
                        "{} ({}:{}-{}) [source:{}]",
                        s.fact, s.file, s.line_start, s.line_end, s.source_id
                    )
                })
                .collect();
            chapters.push(OutlineChapter {
                chapter_id: format!("chapter-{:02}", index + 1),
                title: if title_words.is_empty() {
                    format!("{topic} — chapter {}", index + 1)
                } else {
                    format!("{} — {title_words}", index + 1)
                },
                thesis: format!(
                    "Use the pinned evidence to examine {topic}: {} [source:{}]",
                    primary.fact, primary.source_id
                ),
                evidence,
                source_ids: cluster.iter().map(|s| s.source_id.clone()).collect(),
            });
        }

        let mut outline = Self {
            report_version: OUTLINE_VERSION.into(),
            topic,
            audience,
            length,
            chapters,
            sources,
            outline_id: String::new(),
            report_sha256: String::new(),
        };
        outline.outline_id = outline.compute_outline_id()?;
        outline.report_sha256 = outline.digest()?;
        outline.validate()?;
        Ok(outline)
    }

    fn payload(&self) -> OutlinePayload<'_> {
        OutlinePayload {
            report_version: &self.report_version,
            topic: &self.topic,
            audience: &self.audience,
            length: self.length,
            chapters: &self.chapters,
            sources: &self.sources,
        }
    }

    fn compute_outline_id(&self) -> Result<String> {
        let digest = sha256(&canonical(&self.payload())?);
        Ok(format!("outline-{}", &digest[..16]))
    }

    fn digest(&self) -> Result<String> {
        Ok(sha256(&canonical(&(
            &self.report_version,
            &self.topic,
            &self.audience,
            self.length,
            &self.chapters,
            &self.sources,
            &self.outline_id,
        ))?))
    }

    fn validate(&self) -> Result<()> {
        require(
            self.report_version == OUTLINE_VERSION
                && !self.topic.trim().is_empty()
                && !self.audience.trim().is_empty()
                && self.chapters.len() == self.length.chapter_count()
                && !self.sources.is_empty(),
            "outline metadata",
        )?;
        let mut source_ids = HashSet::new();
        for source in &self.sources {
            source.validate()?;
            require(
                source_ids.insert(source.source_id.as_str()),
                "duplicate source id",
            )?;
        }
        let mut chapter_ids = HashSet::new();
        let mut referenced = BTreeSet::new();
        for chapter in &self.chapters {
            require(
                chapter_ids.insert(chapter.chapter_id.as_str())
                    && !chapter.title.trim().is_empty()
                    && !chapter.thesis.trim().is_empty()
                    && !chapter.evidence.is_empty()
                    && chapter.evidence.len() == chapter.source_ids.len()
                    && !chapter.source_ids.is_empty(),
                "chapter metadata or evidence",
            )?;
            for id in &chapter.source_ids {
                require(
                    source_ids.contains(id.as_str()),
                    "chapter source does not resolve",
                )?;
                require(referenced.insert(id.as_str()), "source assigned twice")?;
            }
        }
        require(referenced.len() == self.sources.len(), "unclustered source")?;
        require(
            self.outline_id == self.compute_outline_id()?,
            "outline id mismatch",
        )
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.validate()?;
        require(
            self.report_sha256 == self.digest()?,
            "outline report digest mismatch",
        )?;
        let bytes = canonical(self)?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    pub fn verify_json(json: &[u8]) -> Result<Self> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let outline: Self = serde_json::from_slice(json)?;
        outline.to_jcs()?;
        Ok(outline)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitedEvidence {
    pub source_id: String,
    pub corpus: String,
    pub pin_sha: String,
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub text: String,
    pub stance: Stance,
    pub pinned_citation: PinnedCitation,
}

pub type CitedQuote = CitedEvidence;

impl CitedEvidence {
    fn fact_from(source: &WriteSourceItem) -> Self {
        Self {
            source_id: source.source_id.clone(),
            corpus: source.corpus.clone(),
            pin_sha: source.pin_sha.clone(),
            file: source.file.clone(),
            line_start: source.line_start,
            line_end: source.line_end,
            byte_start: source.byte_start,
            byte_end: source.byte_end,
            text: source.fact.clone(),
            stance: source.stance,
            pinned_citation: source.pinned_citation.clone(),
        }
    }

    fn quote_from(source: &WriteSourceItem) -> Self {
        Self {
            source_id: source.source_id.clone(),
            corpus: source.corpus.clone(),
            pin_sha: source.pin_sha.clone(),
            file: source.file.clone(),
            line_start: source.line_start,
            line_end: source.line_end,
            byte_start: source.byte_start,
            byte_end: source.byte_end,
            text: source.quote.clone(),
            stance: source.stance,
            pinned_citation: source.pinned_citation.clone(),
        }
    }

    pub fn file_line_span(&self) -> String {
        format!(
            "{}:{}-{}@{}:{}",
            self.file, self.line_start, self.line_end, self.byte_start, self.byte_end
        )
    }

    fn validate(&self) -> Result<()> {
        require(
            !self.source_id.trim().is_empty()
                && !self.corpus.trim().is_empty()
                && hex_40(&self.pin_sha)
                && !self.file.trim().is_empty()
                && self.line_start > 0
                && self.line_start <= self.line_end
                && self.byte_start < self.byte_end
                && !self.text.trim().is_empty(),
            "cited evidence is not resolvable",
        )?;
        require(
            self.pinned_citation.corpus == self.corpus
                && self.pinned_citation.commit == self.pin_sha
                && self.pinned_citation.file == self.file
                && self.pinned_citation.line_start == self.line_start
                && self.pinned_citation.line_end == self.line_end
                && self.pinned_citation.byte_start == self.byte_start
                && self.pinned_citation.byte_end == self.byte_end,
            "cited evidence does not match pinned citation",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteSectionPacket {
    pub report_version: String,
    pub outline_id: String,
    pub chapter_id: String,
    pub title: String,
    pub thesis: String,
    pub facts: Vec<CitedEvidence>,
    pub quotes: Vec<CitedEvidence>,
    pub counterpoints: Vec<CitedEvidence>,
    pub style_rules: Vec<String>,
    pub token_budget: usize,
    pub estimated_tokens: usize,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct SectionPayload<'a> {
    report_version: &'a str,
    outline_id: &'a str,
    chapter_id: &'a str,
    title: &'a str,
    thesis: &'a str,
    facts: &'a [CitedEvidence],
    quotes: &'a [CitedEvidence],
    counterpoints: &'a [CitedEvidence],
    style_rules: &'a [String],
    token_budget: usize,
    estimated_tokens: usize,
}

/// Length of the `report_sha256` digest, hex.
const DIGEST_HEX_LEN: usize = 64;

/// The packet's size in tokens, measured on what `to_jcs` emits: one token per
/// four bytes of its RFC 8785 JSON, rounded up. Every key, every
/// `pinned_citation` and the receipt count, so the figure is what a consumer of
/// the packet pays, not an estimate of its prose.
///
/// `estimated_tokens` is a field of the packet it measures, so its own digits
/// are part of the bytes: the count is the fixed point of that, found by
/// iterating up from zero (it settles in one or two rounds). The digest is a
/// fixed-length hex string, so it is measured as a placeholder.
fn packet_token_count(packet: &WriteSectionPacket) -> Result<usize> {
    let mut sized = packet.clone();
    sized.report_sha256 = "0".repeat(DIGEST_HEX_LEN);
    let mut estimate = 0usize;
    for _ in 0..8 {
        sized.estimated_tokens = estimate;
        let next = canonical(&sized)?.len().div_ceil(4);
        if next == estimate {
            return Ok(estimate);
        }
        estimate = next;
    }
    Err(WritingPackError::Invalid("token count did not settle"))
}

impl WriteSectionPacket {
    fn digest(&self) -> Result<String> {
        Ok(sha256(&canonical(&SectionPayload {
            report_version: &self.report_version,
            outline_id: &self.outline_id,
            chapter_id: &self.chapter_id,
            title: &self.title,
            thesis: &self.thesis,
            facts: &self.facts,
            quotes: &self.quotes,
            counterpoints: &self.counterpoints,
            style_rules: &self.style_rules,
            token_budget: self.token_budget,
            estimated_tokens: self.estimated_tokens,
        })?))
    }

    fn validate(&self) -> Result<()> {
        let supported = !self.facts.is_empty() && !self.quotes.is_empty();
        let counterpoint_only =
            self.facts.is_empty() && self.quotes.is_empty() && !self.counterpoints.is_empty();
        require(
            self.report_version == SECTION_VERSION
                && !self.outline_id.trim().is_empty()
                && !self.chapter_id.trim().is_empty()
                && !self.title.trim().is_empty()
                && !self.thesis.trim().is_empty()
                && (supported || counterpoint_only)
                && !self.style_rules.is_empty()
                && self.estimated_tokens == packet_token_count(self)?
                && self.estimated_tokens <= self.token_budget,
            "section metadata, facts, quotes, token count, or budget",
        )?;
        for item in self
            .facts
            .iter()
            .chain(&self.quotes)
            .chain(&self.counterpoints)
        {
            item.validate()?;
        }
        require(
            self.counterpoints
                .iter()
                .all(|item| item.stance == Stance::Counterpoint)
                && self
                    .facts
                    .iter()
                    .chain(&self.quotes)
                    .all(|item| item.stance != Stance::Counterpoint),
            "counterpoint classification",
        )
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.validate()?;
        require(
            self.report_sha256 == self.digest()?,
            "section report digest mismatch",
        )?;
        let bytes = canonical(self)?;
        require(
            bytes.len().div_ceil(4) <= self.token_budget,
            "serialized section exceeds its token budget",
        )?;
        require(bytes.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        Ok(bytes)
    }

    pub fn verify_json(json: &[u8]) -> Result<Self> {
        require(json.len() <= MAX_REPORT_BYTES, "report byte limit")?;
        let packet: Self = serde_json::from_slice(json)?;
        packet.to_jcs()?;
        Ok(packet)
    }
}

fn fact_rank(source: &&WriteSourceItem) -> u8 {
    match source.stance {
        Stance::Supporting => 0,
        Stance::Neutral => 1,
        Stance::Counterpoint => 2,
    }
}

pub fn write_section(
    outline: &WriteOutline,
    outline_id: &str,
    chapter: &str,
    token_budget: usize,
) -> Result<WriteSectionPacket> {
    outline.to_jcs()?;
    if outline_id != outline.outline_id {
        return Err(WritingPackError::UnknownOutlineId(outline_id.into()));
    }
    let resolved_chapter = outline
        .chapters
        .iter()
        .find(|item| item.chapter_id == chapter || item.title == chapter)
        .ok_or_else(|| WritingPackError::UnknownChapter(chapter.into()))?;
    let chapter_sources: Vec<_> = resolved_chapter
        .source_ids
        .iter()
        .filter_map(|id| {
            outline
                .sources
                .iter()
                .find(|source| source.source_id == *id)
        })
        .collect();
    let mut supporting_sources: Vec<_> = chapter_sources
        .iter()
        .copied()
        .filter(|source| source.stance != Stance::Counterpoint)
        .collect();
    supporting_sources.sort_by_key(fact_rank);
    // A chapter whose sources are all counterpoints has nothing to support its
    // thesis. Its counterpoints are listed once, as counterpoints: they are not
    // also presented as the fact and the quote.
    let counterpoint_only = supporting_sources.is_empty();
    let counterpoints: Vec<&WriteSourceItem> = if counterpoint_only {
        chapter_sources.clone()
    } else {
        chapter_sources
            .iter()
            .copied()
            .find(|source| source.stance == Stance::Counterpoint)
            .or_else(|| {
                outline
                    .sources
                    .iter()
                    .find(|source| source.stance == Stance::Counterpoint)
            })
            .into_iter()
            .collect()
    };
    let mut style_rules = vec![
        "Ground every factual sentence in the chapter thesis and cited sources.".into(),
        "Cite every factual claim or quote with [source:<id>] and preserve file:line provenance."
            .into(),
        "Represent counterpoints explicitly as scope boundaries or limitations, never as omitted evidence."
            .into(),
        format!("Calibrate depth and terminology for audience: {}.", outline.audience),
    ];
    if counterpoint_only {
        style_rules.push(
            "This chapter has only counterpoint evidence: present it as a limitation and make no supported claim."
                .into(),
        );
    }

    let mut packet = WriteSectionPacket {
        report_version: SECTION_VERSION.into(),
        outline_id: outline.outline_id.clone(),
        chapter_id: resolved_chapter.chapter_id.clone(),
        title: resolved_chapter.title.clone(),
        thesis: resolved_chapter.thesis.clone(),
        facts: supporting_sources
            .first()
            .map(|source| vec![CitedEvidence::fact_from(source)])
            .unwrap_or_default(),
        quotes: supporting_sources
            .first()
            .map(|source| vec![CitedEvidence::quote_from(source)])
            .unwrap_or_default(),
        counterpoints: counterpoints
            .first()
            .map(|source| vec![CitedEvidence::quote_from(source)])
            .unwrap_or_default(),
        style_rules,
        token_budget,
        estimated_tokens: 0,
        report_sha256: String::new(),
    };
    packet.estimated_tokens = packet_token_count(&packet)?;
    if packet.estimated_tokens > token_budget {
        return Err(WritingPackError::TokenBudgetTooSmall {
            budget: token_budget,
            minimum: packet.estimated_tokens,
        });
    }
    // Lower-ranked items are added while the packet, as `to_jcs` serializes it,
    // still fits: the first one that does not fit ends the list.
    for source in supporting_sources.into_iter().skip(1) {
        packet.facts.push(CitedEvidence::fact_from(source));
        packet.quotes.push(CitedEvidence::quote_from(source));
        let candidate_count = packet_token_count(&packet)?;
        if candidate_count > token_budget {
            packet.facts.pop();
            packet.quotes.pop();
            break;
        }
        packet.estimated_tokens = candidate_count;
    }
    if counterpoint_only {
        for source in counterpoints.into_iter().skip(1) {
            packet.counterpoints.push(CitedEvidence::quote_from(source));
            let candidate_count = packet_token_count(&packet)?;
            if candidate_count > token_budget {
                packet.counterpoints.pop();
                break;
            }
            packet.estimated_tokens = candidate_count;
        }
    }
    packet.report_sha256 = packet.digest()?;
    packet.validate()?;
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(index: usize, stance: Stance) -> WriteSourceItem {
        let fact = format!(
            "Evidence theme {} establishes bounded determinism for section synthesis.",
            index % 3
        );
        source_with_fact(index, fact, stance)
    }

    fn source_with_fact(index: usize, fact: String, stance: Stance) -> WriteSourceItem {
        let quote = format!(
            "\"Evidence theme {} supports a qualified statement about topic safety.\"",
            index % 3
        );
        let file = format!("docs/topic-{index}.md");
        let line_start = 10 + index as u64;
        let bytes = format!(
            "{}{quote}\n",
            "\n".repeat(usize::try_from(line_start - 1).unwrap())
        );
        let citation = PinnedCitation::from_bytes(
            if index.is_multiple_of(2) {
                "docs"
            } else {
                "research"
            }
            .into(),
            "example/evidence".into(),
            "a".repeat(40),
            file,
            bytes.as_bytes(),
            (line_start as usize - 1)..(bytes.len() - 1),
        )
        .unwrap();
        WriteSourceItem::from_citation(format!("source-{index:02}"), fact, quote, stance, citation)
            .unwrap()
    }

    fn sources() -> Vec<WriteSourceItem> {
        vec![
            source(1, Stance::Supporting),
            source(2, Stance::Neutral),
            source(3, Stance::Counterpoint),
            source(4, Stance::Supporting),
            source(5, Stance::Supporting),
            source(6, Stance::Neutral),
        ]
    }

    #[test]
    fn outline_length_scales_deterministically_and_every_source_resolves() {
        let brief = write_outline(
            "Pack determinism",
            "engineers",
            OutlineLength::Brief,
            sources(),
        )
        .unwrap();
        let brief_again = write_outline(
            "Pack determinism",
            "engineers",
            OutlineLength::Brief,
            sources().into_iter().rev().collect(),
        )
        .unwrap();
        assert_eq!(brief.outline_id, brief_again.outline_id);
        let standard = write_outline(
            "Pack determinism",
            "engineers",
            OutlineLength::Standard,
            sources(),
        )
        .unwrap();
        let comprehensive = write_outline(
            "Pack determinism",
            "engineers",
            OutlineLength::Comprehensive,
            sources(),
        )
        .unwrap();
        assert_eq!(brief.chapters.len(), 1);
        assert_eq!(standard.chapters.len(), 3);
        assert_eq!(comprehensive.chapters.len(), 5);
        assert!(comprehensive
            .chapters
            .iter()
            .all(|c| !c.source_ids.is_empty() && c.evidence.len() == c.source_ids.len()));
        assert_eq!(brief.to_jcs().unwrap(), brief_again.to_jcs().unwrap());
    }

    #[test]
    fn outline_refuses_padded_insufficient_evidence() {
        let error = write_outline(
            "Topic",
            "audience",
            OutlineLength::Standard,
            vec![source(1, Stance::Supporting)],
        )
        .unwrap_err();
        assert_eq!(
            error,
            WritingPackError::InsufficientEvidence {
                required: 3,
                available: 1
            }
        );
    }

    #[test]
    fn section_rejects_unknown_ids_preserves_counterpoint_and_bounds_tokens() {
        let outline =
            write_outline("Topic", "audience", OutlineLength::Standard, sources()).unwrap();
        assert_eq!(
            write_section(&outline, "outline-wrong", "chapter-01", 1_000).unwrap_err(),
            WritingPackError::UnknownOutlineId("outline-wrong".into())
        );
        assert_eq!(
            write_section(&outline, &outline.outline_id, "chapter-99", 1_000).unwrap_err(),
            WritingPackError::UnknownChapter("chapter-99".into())
        );
        let packet = write_section(&outline, &outline.outline_id, "chapter-01", 1_000).unwrap();
        assert!(!packet.facts.is_empty());
        assert!(!packet.quotes.is_empty());
        assert_eq!(packet.counterpoints.len(), 1);
        assert!(packet.facts[0].file_line_span().starts_with("docs/topic-"));
        assert!(packet.estimated_tokens <= packet.token_budget);
        let json = packet.to_jcs().unwrap();
        assert_eq!(
            WriteSectionPacket::verify_json(&json)
                .unwrap()
                .report_sha256,
            packet.report_sha256
        );
        assert!(matches!(
            write_section(&outline, &outline.outline_id, "chapter-01", 1),
            Err(WritingPackError::TokenBudgetTooSmall { .. })
        ));
    }

    #[test]
    fn outline_and_section_digests_detect_tampering() {
        let outline = write_outline("Topic", "audience", OutlineLength::Brief, sources()).unwrap();
        let json = outline.to_jcs().unwrap();
        assert_eq!(
            WriteOutline::verify_json(&json).unwrap().report_sha256,
            outline.report_sha256
        );
        let changed = String::from_utf8(json)
            .unwrap()
            .replacen("\"audience\":\"audience\"", "\"audience\":\"changed\"", 1)
            .into_bytes();
        assert!(WriteOutline::verify_json(&changed).is_err());
    }

    fn themed(index: usize, theme: &str, stance: Stance) -> WriteSourceItem {
        source_with_fact(
            index,
            format!("{theme} establishes bounded determinism for section synthesis."),
            stance,
        )
    }

    fn chapter_of<'a>(outline: &'a WriteOutline, id: &str) -> &'a OutlineChapter {
        outline
            .chapters
            .iter()
            .find(|c| c.source_ids.iter().any(|s| s == id))
            .unwrap()
    }

    #[test]
    fn the_theme_key_keeps_short_tokens_and_digits() {
        let one = themed(1, "Rule 1 of the set", Stance::Supporting);
        let two = themed(2, "Rule 2 of the set", Stance::Supporting);
        let short = themed(3, "An id is not a key", Stance::Supporting);
        assert_eq!(one.theme_key(), "rule 1 of the");
        assert_ne!(one.theme_key(), two.theme_key());
        assert_eq!(short.theme_key(), "an id is not");
    }

    #[test]
    fn a_theme_stays_in_one_chapter_and_boundaries_fall_between_themes() {
        // Three themes of two sources each, three chapters: one theme per chapter.
        let mut grouped = Vec::new();
        for (n, theme) in ["alpha one", "beta two", "gamma three"].iter().enumerate() {
            grouped.push(themed(n * 2 + 1, theme, Stance::Supporting));
            grouped.push(themed(n * 2 + 2, theme, Stance::Neutral));
        }
        let outline = write_outline("Topic", "audience", OutlineLength::Standard, grouped).unwrap();
        for chapter in &outline.chapters {
            assert_eq!(chapter.source_ids.len(), 2, "{chapter:?}");
            let keys: BTreeSet<_> = chapter
                .source_ids
                .iter()
                .map(|id| {
                    outline
                        .sources
                        .iter()
                        .find(|s| &s.source_id == id)
                        .unwrap()
                        .theme_key()
                })
                .collect();
            assert_eq!(keys.len(), 1, "a theme was split across chapters: {keys:?}");
        }

        // Sources that differ only by a digit are different themes.
        let numbered: Vec<_> = (1..=3)
            .map(|n| themed(n, &format!("Section {n} covers"), Stance::Supporting))
            .collect();
        let outline =
            write_outline("Topic", "audience", OutlineLength::Standard, numbered).unwrap();
        assert!(outline.chapters.iter().all(|c| c.source_ids.len() == 1));
    }

    #[test]
    fn fewer_themes_than_chapters_split_the_largest_theme_and_never_pad() {
        let mut few = Vec::new();
        for n in 1..=4 {
            few.push(themed(n, "alpha one", Stance::Supporting));
        }
        few.push(themed(5, "beta two", Stance::Supporting));
        let outline =
            write_outline("Topic", "audience", OutlineLength::Comprehensive, few).unwrap();
        assert_eq!(outline.chapters.len(), 5);
        assert!(outline.chapters.iter().all(|c| c.source_ids.len() == 1));
        let all: BTreeSet<_> = outline
            .chapters
            .iter()
            .flat_map(|c| c.source_ids.clone())
            .collect();
        assert_eq!(all.len(), 5, "every source in exactly one chapter");
    }

    #[test]
    fn the_token_budget_bounds_the_serialized_packet_not_a_sum_of_prose() {
        let twelve: Vec<_> = (1..=12)
            .map(|n| {
                source(
                    n,
                    if n % 4 == 3 {
                        Stance::Counterpoint
                    } else {
                        Stance::Supporting
                    },
                )
            })
            .collect();
        let outline = write_outline("Topic", "audience", OutlineLength::Brief, twelve).unwrap();
        let id = outline.outline_id.clone();

        // The review probe: 12 sources and a 600-token budget. The prose-only
        // estimate said 569; the serialized packet is several times that.
        let floor = match write_section(&outline, &id, "chapter-01", 600).unwrap_err() {
            WritingPackError::TokenBudgetTooSmall { budget, minimum } => {
                assert_eq!(budget, 600);
                minimum
            }
            other => panic!("{other}"),
        };
        assert!(floor > 600, "{floor}");

        let full = write_section(&outline, &id, "chapter-01", 1_000_000).unwrap();
        assert_eq!(full.facts.len(), 9);
        let full_bytes = full.to_jcs().unwrap();
        assert_eq!(full.estimated_tokens, full_bytes.len().div_ceil(4));

        let core = write_section(&outline, &id, "chapter-01", floor).unwrap();
        assert_eq!(core.facts.len(), 1, "only the mandatory core fits");
        assert_eq!(core.counterpoints.len(), 1);
        assert_eq!(core.estimated_tokens, floor);
        assert_eq!(core.to_jcs().unwrap().len().div_ceil(4), floor);

        let middle = (floor + full.estimated_tokens) / 2;
        let partial = write_section(&outline, &id, "chapter-01", middle).unwrap();
        assert!(partial.facts.len() > 1 && partial.facts.len() < full.facts.len());
        let partial_bytes = partial.to_jcs().unwrap();
        assert!(partial_bytes.len().div_ceil(4) <= middle);
        assert_eq!(partial.facts.len(), partial.quotes.len());

        // A packet whose budget was lowered after sealing no longer verifies.
        let lowered = String::from_utf8(full_bytes).unwrap().replacen(
            "\"token_budget\":1000000",
            "\"token_budget\":10",
            1,
        );
        assert!(WriteSectionPacket::verify_json(lowered.as_bytes()).is_err());
    }

    #[test]
    fn a_counterpoint_only_chapter_lists_its_source_once() {
        let sources = vec![
            themed(1, "alpha one", Stance::Supporting),
            themed(2, "alpha one", Stance::Neutral),
            themed(3, "beta two", Stance::Supporting),
            themed(4, "gamma three", Stance::Counterpoint),
        ];
        let outline = write_outline("Topic", "audience", OutlineLength::Standard, sources).unwrap();
        let only = chapter_of(&outline, "source-04");
        assert_eq!(only.source_ids, vec!["source-04".to_string()]);

        let packet =
            write_section(&outline, &outline.outline_id, &only.chapter_id, 100_000).unwrap();
        assert!(packet.facts.is_empty() && packet.quotes.is_empty());
        assert_eq!(packet.counterpoints.len(), 1);
        assert_eq!(packet.counterpoints[0].source_id, "source-04");
        assert!(packet
            .style_rules
            .iter()
            .any(|rule| rule.contains("only counterpoint evidence")));
        WriteSectionPacket::verify_json(&packet.to_jcs().unwrap()).unwrap();

        // A chapter with supporting evidence never repeats a counterpoint as
        // its fact or quote.
        let mixed = write_section(
            &outline,
            &outline.outline_id,
            &chapter_of(&outline, "source-01").chapter_id,
            100_000,
        )
        .unwrap();
        let in_facts: BTreeSet<_> = mixed
            .facts
            .iter()
            .chain(&mixed.quotes)
            .map(|e| e.source_id.clone())
            .collect();
        assert!(mixed
            .counterpoints
            .iter()
            .all(|c| !in_facts.contains(&c.source_id)));
    }
}
