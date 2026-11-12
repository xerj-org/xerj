//! Structure-first writing packets assembled from verified hub evidence.
//!
//! The pack never invents uncited claims: it groups supplied evidence and hands
//! bounded, citable source material to the caller's writer.

use crate::hub_evidence::PinnedCitation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};

const OUTLINE_VERSION: &str = "xerj-write-outline/v1";
const SECTION_VERSION: &str = "xerj-write-section/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum WritePackError {
    #[error("insufficient verified evidence: need at least {0} cited sources for this length")]
    InsufficientEvidence(usize),
    #[error("unknown outline id: {0}")]
    UnknownOutlineId(String),
    #[error("unknown chapter id: {0}")]
    UnknownChapterId(String),
    #[error("token budget {budget} is too small; minimum is {minimum}")]
    TokenBudgetTooSmall { budget: usize, minimum: usize },
    #[error("invalid writing packet: {0}")]
    Invalid(&'static str),
    #[error("writing packet JSON: {0}")]
    Json(String),
}

impl From<serde_json::Error> for WritePackError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

type Result<T> = std::result::Result<T, WritePackError>;

fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(WritePackError::Invalid(reason))
    }
}

fn hex_40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}

/// Deterministic estimate: one token per four UTF-8 bytes, rounded up.
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
pub enum TargetLength {
    Brief,
    Standard,
    Longform,
}

impl TargetLength {
    pub fn chapter_count(self) -> usize {
        match self {
            Self::Brief => 1,
            Self::Standard => 3,
            Self::Longform => 5,
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
    pub quote: String,
    pub stance: Stance,
    pub pinned_citation: PinnedCitation,
}

impl WriteSourceItem {
    fn validate(&self) -> Result<()> {
        require(
            !self.source_id.trim().is_empty()
                && !self.corpus.trim().is_empty()
                && hex_40(&self.pin_sha)
                && !self.file.trim().is_empty()
                && self.line_start > 0
                && self.line_start <= self.line_end
                && !self.quote.trim().is_empty(),
            "source identity, location, pin, or quote",
        )?;
        require(
            self.pinned_citation.corpus == self.corpus
                && self.pinned_citation.commit == self.pin_sha
                && self.pinned_citation.file == self.file
                && self.pinned_citation.line_start == self.line_start
                && self.pinned_citation.line_end == self.line_end,
            "pinned citation does not match source metadata",
        )
    }

    fn theme_key(&self) -> String {
        self.quote
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.len() >= 4)
            .map(str::to_ascii_lowercase)
            .take(4)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineChapter {
    pub chapter_id: String,
    pub title: String,
    pub thesis: String,
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteOutline {
    pub report_version: String,
    pub topic: String,
    pub audience: String,
    pub target_length: TargetLength,
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
    target_length: TargetLength,
    chapters: &'a [OutlineChapter],
    sources: &'a [WriteSourceItem],
}

impl WriteOutline {
    pub fn new(
        topic: String,
        audience: String,
        target_length: TargetLength,
        mut sources: Vec<WriteSourceItem>,
    ) -> Result<Self> {
        for source in &sources {
            source.validate()?;
        }
        let chapter_count = target_length.chapter_count();
        if sources.len() < chapter_count {
            return Err(WritePackError::InsufficientEvidence(chapter_count));
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

        let base = sources.len() / chapter_count;
        let remainder = sources.len() % chapter_count;
        let mut offset = 0;
        let mut chapters = Vec::with_capacity(chapter_count);
        for index in 0..chapter_count {
            let width = base + usize::from(index < remainder);
            let cluster = &sources[offset..offset + width];
            offset += width;
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
            chapters.push(OutlineChapter {
                chapter_id: format!("chapter-{:02}", index + 1),
                title: if title_words.is_empty() {
                    format!("{topic} — chapter {}", index + 1)
                } else {
                    format!("{} — {title_words}", index + 1)
                },
                thesis: format!(
                    "Use the pinned evidence to examine {topic}: {}",
                    primary.quote
                ),
                source_ids: cluster.iter().map(|s| s.source_id.clone()).collect(),
            });
        }

        let mut outline = Self {
            report_version: OUTLINE_VERSION.into(),
            topic,
            audience,
            target_length,
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
            target_length: self.target_length,
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
            self.target_length,
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
                && self.chapters.len() == self.target_length.chapter_count()
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
pub struct CitedQuote {
    pub source_id: String,
    pub corpus: String,
    pub file: String,
    pub line_start: u64,
    pub line_end: u64,
    pub quote: String,
    pub stance: Stance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_citation: Option<PinnedCitation>,
}

impl CitedQuote {
    fn from(source: &WriteSourceItem) -> Self {
        Self {
            source_id: source.source_id.clone(),
            corpus: source.corpus.clone(),
            file: source.file.clone(),
            line_start: source.line_start,
            line_end: source.line_end,
            quote: source.quote.clone(),
            stance: source.stance,
            pinned_citation: Some(source.pinned_citation.clone()),
        }
    }

    fn validate(&self) -> Result<()> {
        require(
            !self.source_id.trim().is_empty()
                && !self.corpus.trim().is_empty()
                && !self.file.trim().is_empty()
                && self.line_start > 0
                && self.line_start <= self.line_end
                && !self.quote.trim().is_empty(),
            "cited quote is not resolvable",
        )?;
        if let Some(citation) = &self.pinned_citation {
            require(
                citation.corpus == self.corpus
                    && citation.file == self.file
                    && citation.line_start == self.line_start
                    && citation.line_end == self.line_end,
                "cited quote does not match pinned citation",
            )?;
        }
        Ok(())
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
    pub facts: Vec<CitedQuote>,
    pub counterpoints: Vec<CitedQuote>,
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
    facts: &'a [CitedQuote],
    counterpoints: &'a [CitedQuote],
    style_rules: &'a [String],
    token_budget: usize,
    estimated_tokens: usize,
}

fn packet_token_count(packet: &WriteSectionPacket) -> usize {
    let quote_tokens = |quotes: &[CitedQuote]| {
        quotes
            .iter()
            .map(|q| {
                estimated_token_count(&q.source_id)
                    + estimated_token_count(&q.corpus)
                    + estimated_token_count(&q.file)
                    + estimated_token_count(&q.quote)
                    + 8
            })
            .sum::<usize>()
    };
    12 + estimated_token_count(&packet.outline_id)
        + estimated_token_count(&packet.chapter_id)
        + estimated_token_count(&packet.title)
        + estimated_token_count(&packet.thesis)
        + packet
            .style_rules
            .iter()
            .map(|s| estimated_token_count(s))
            .sum::<usize>()
        + quote_tokens(&packet.facts)
        + quote_tokens(&packet.counterpoints)
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
            counterpoints: &self.counterpoints,
            style_rules: &self.style_rules,
            token_budget: self.token_budget,
            estimated_tokens: self.estimated_tokens,
        })?))
    }

    fn validate(&self) -> Result<()> {
        require(
            self.report_version == SECTION_VERSION
                && !self.outline_id.trim().is_empty()
                && !self.chapter_id.trim().is_empty()
                && !self.title.trim().is_empty()
                && !self.thesis.trim().is_empty()
                && !self.facts.is_empty()
                && !self.style_rules.is_empty()
                && self.estimated_tokens == packet_token_count(self)
                && self.estimated_tokens <= self.token_budget,
            "section metadata, evidence, token count, or budget",
        )?;
        for quote in self.facts.iter().chain(&self.counterpoints) {
            quote.validate()?;
        }
        require(
            self.counterpoints
                .iter()
                .all(|quote| quote.stance == Stance::Counterpoint),
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
    chapter_id: &str,
    token_budget: usize,
) -> Result<WriteSectionPacket> {
    outline.to_jcs()?;
    if outline_id != outline.outline_id {
        return Err(WritePackError::UnknownOutlineId(outline_id.into()));
    }
    let chapter = outline
        .chapters
        .iter()
        .find(|chapter| chapter.chapter_id == chapter_id)
        .ok_or_else(|| WritePackError::UnknownChapterId(chapter_id.into()))?;
    let chapter_sources: Vec<_> = chapter
        .source_ids
        .iter()
        .filter_map(|id| {
            outline
                .sources
                .iter()
                .find(|source| source.source_id == *id)
        })
        .collect();
    let mut facts: Vec<_> = chapter_sources
        .iter()
        .copied()
        .filter(|source| source.stance != Stance::Counterpoint)
        .collect();
    facts.sort_by_key(fact_rank);
    if facts.is_empty() {
        facts.push(chapter_sources[0]);
    }
    let required_fact = facts[0];
    let counterpoint = chapter_sources
        .iter()
        .copied()
        .find(|source| source.stance == Stance::Counterpoint)
        .or_else(|| {
            outline
                .sources
                .iter()
                .find(|source| source.stance == Stance::Counterpoint)
        });
    let style_rules = vec![
        "Use the chapter thesis; do not add claims without a cited source.".into(),
        "Quote or paraphrase facts with the supplied source_id and file:line.".into(),
        "Represent counterpoints as limitations, not as ignored evidence.".into(),
        format!("Write for audience: {}.", outline.audience),
    ];

    let mut packet = WriteSectionPacket {
        report_version: SECTION_VERSION.into(),
        outline_id: outline.outline_id.clone(),
        chapter_id: chapter.chapter_id.clone(),
        title: chapter.title.clone(),
        thesis: chapter.thesis.clone(),
        facts: vec![CitedQuote::from(required_fact)],
        counterpoints: counterpoint
            .map(|source| vec![CitedQuote::from(source)])
            .unwrap_or_default(),
        style_rules,
        token_budget,
        estimated_tokens: 0,
        report_sha256: String::new(),
    };
    packet.estimated_tokens = packet_token_count(&packet);
    if packet.estimated_tokens > token_budget {
        return Err(WritePackError::TokenBudgetTooSmall {
            budget: token_budget,
            minimum: packet.estimated_tokens,
        });
    }
    for source in facts.into_iter().skip(1) {
        let candidate = CitedQuote::from(source);
        packet.facts.push(candidate.clone());
        let candidate_count = packet_token_count(&packet);
        if candidate_count > token_budget {
            packet.facts.pop();
            break;
        }
        packet.estimated_tokens = candidate_count;
    }
    packet.report_sha256 = packet.digest()?;
    packet.validate()?;
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(index: usize, stance: Stance) -> WriteSourceItem {
        let quote = format!(
            "Evidence theme {} supports a qualified statement about topic safety.",
            index % 3
        );
        let file = format!("topic-{index}.md");
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
            file.clone(),
            bytes.as_bytes(),
            (line_start as usize - 1)..(bytes.len() - 1),
        )
        .unwrap();
        WriteSourceItem {
            source_id: format!("source-{index:02}"),
            corpus: if index.is_multiple_of(2) {
                "docs"
            } else {
                "research"
            }
            .into(),
            pin_sha: "a".repeat(40),
            file,
            line_start,
            line_end: line_start,
            quote,
            stance,
            pinned_citation: citation,
        }
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
    fn outline_length_is_deterministic_and_every_source_resolves() {
        let brief = WriteOutline::new(
            "Pack determinism".into(),
            "engineers".into(),
            TargetLength::Brief,
            sources(),
        )
        .unwrap();
        let brief_again = WriteOutline::new(
            "Pack determinism".into(),
            "engineers".into(),
            TargetLength::Brief,
            sources().into_iter().rev().collect(),
        )
        .unwrap();
        assert_eq!(brief.outline_id, brief_again.outline_id);
        let standard = WriteOutline::new(
            "Pack determinism".into(),
            "engineers".into(),
            TargetLength::Standard,
            sources(),
        )
        .unwrap();
        let longform = WriteOutline::new(
            "Pack determinism".into(),
            "engineers".into(),
            TargetLength::Longform,
            sources(),
        )
        .unwrap();
        assert!(brief.chapters.len() < standard.chapters.len());
        assert!(standard.chapters.len() < longform.chapters.len());
        assert!(brief.chapters.iter().all(|c| !c.source_ids.is_empty()));
        assert_eq!(brief.to_jcs().unwrap(), brief_again.to_jcs().unwrap());
    }

    #[test]
    fn outline_refuses_padded_insufficient_evidence() {
        let error = WriteOutline::new(
            "Topic".into(),
            "audience".into(),
            TargetLength::Standard,
            vec![source(1, Stance::Supporting)],
        )
        .unwrap_err();
        assert!(matches!(error, WritePackError::InsufficientEvidence(3)));
    }

    #[test]
    fn section_rejects_unknown_ids_preserves_counterpoint_and_bounds_tokens() {
        let outline = WriteOutline::new(
            "Topic".into(),
            "audience".into(),
            TargetLength::Standard,
            sources(),
        )
        .unwrap();
        assert!(matches!(
            write_section(&outline, "outline-wrong", "chapter-01", 1_000),
            Err(WritePackError::UnknownOutlineId(_))
        ));
        assert!(matches!(
            write_section(&outline, &outline.outline_id, "chapter-99", 1_000),
            Err(WritePackError::UnknownChapterId(_))
        ));
        let packet = write_section(&outline, &outline.outline_id, "chapter-01", 1_000).unwrap();
        assert_eq!(packet.counterpoints.len(), 1);
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
            Err(WritePackError::TokenBudgetTooSmall { .. })
        ));
    }

    #[test]
    fn outline_and_section_digests_detect_tampering() {
        let outline = WriteOutline::new(
            "Topic".into(),
            "audience".into(),
            TargetLength::Brief,
            sources(),
        )
        .unwrap();
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
}
