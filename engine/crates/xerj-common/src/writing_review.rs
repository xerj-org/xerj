//! Deterministic, plain-text writing checks, independent of the pack runtime.
//!
//! Policy belongs to the caller's TOML/JSON configuration. Citation checks are
//! syntactic heuristics, not fact checking. See `docs/writing-review.md`.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub check: Check,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    BannedPhrases {
        phrases: Vec<String>,
    },
    SentenceLength {
        max_words: usize,
    },
    Repetition {
        min_words: usize,
        max_occurrences: usize,
    },
    Hedging {
        phrases: Vec<String>,
        max_per_100_words: u32,
    },
    Citations {
        claim_phrases: Vec<String>,
        numeric_claims: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Note {
    /// One-based physical line on which the checked sentence starts.
    pub line: usize,
    pub rule_id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Review {
    pub passed: bool,
    pub notes: Vec<Note>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid writing review configuration: {0}")]
pub struct ConfigError(String);

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .map(str::to_lowercase)
        .collect()
}

fn phrase_count(tokens: &[String], phrase: &[String]) -> usize {
    tokens
        .windows(phrase.len())
        .filter(|w| *w == phrase)
        .count()
}

impl ReviewConfig {
    /// Validate programmatic configurations as well as deserialized recipes.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if rule.id.trim().is_empty() || !ids.insert(&rule.id) {
                return Err(ConfigError("rule ids must be nonempty and unique".into()));
            }
            let phrases = match &rule.check {
                Check::BannedPhrases { phrases } | Check::Hedging { phrases, .. } => Some(phrases),
                Check::Citations { claim_phrases, .. } => Some(claim_phrases),
                Check::SentenceLength { max_words: 0 }
                | Check::Repetition { min_words: 0, .. }
                | Check::Repetition {
                    max_occurrences: 0, ..
                } => {
                    return Err(ConfigError(format!(
                        "{}: word/occurrence limits must be positive",
                        rule.id
                    )));
                }
                _ => None,
            };
            if let Some(phrases) = phrases {
                let mut seen = BTreeSet::new();
                for phrase in phrases {
                    let tokens = words(phrase);
                    if tokens.is_empty() || !seen.insert(tokens) {
                        return Err(ConfigError(format!(
                            "{}: phrases must contain words and be unique after normalization",
                            rule.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Review plain text using only caller-supplied policy and trusted source IDs.
///
/// Citations have the exact form `[source:<id>]`. A known ID establishes only
/// syntactic attribution, never that the source supports the statement.
/// Sentences end at `.`, `!`, `?`, or a blank line; soft wraps preserve sentences.
/// Abbreviations and decimals are not linguistically parsed. Markdown is not
/// parsed: callers should supply prose, not code or a complete Markdown document.
pub fn review(
    draft: &str,
    config: &ReviewConfig,
    source_ids: &BTreeSet<String>,
) -> Result<Review, ConfigError> {
    config.validate()?;
    let mut notes = Vec::new();
    let mut repetitions: BTreeMap<(&str, Vec<String>), usize> = BTreeMap::new();
    for (line, sentence) in sentences(draft) {
        let (prose, citations) = citations(&sentence);
        let tokens = words(&prose);
        if tokens.is_empty() && citations.is_empty() {
            continue;
        }
        for rule in &config.rules {
            let mut add = |message| {
                notes.push(Note {
                    line,
                    rule_id: rule.id.clone(),
                    message,
                })
            };
            match &rule.check {
                Check::BannedPhrases { phrases } => {
                    for phrase in phrases {
                        if phrase_count(&tokens, &words(phrase)) > 0 {
                            add(format!("Avoid configured phrase: {phrase}"));
                        }
                    }
                }
                Check::SentenceLength { max_words } if tokens.len() > *max_words => {
                    add(format!(
                        "Sentence has {} words; maximum is {max_words}.",
                        tokens.len()
                    ));
                }
                Check::Repetition {
                    min_words,
                    max_occurrences,
                } if tokens.len() >= *min_words => {
                    let count = repetitions.entry((&rule.id, tokens.clone())).or_default();
                    *count += 1;
                    if *count > *max_occurrences {
                        add(format!(
                            "Repeated sentence occurrence {count}; maximum is {max_occurrences}."
                        ));
                    }
                }
                Check::Hedging {
                    phrases,
                    max_per_100_words,
                } => {
                    // Overlapping configured phrases count separately, by policy.
                    let count: usize = phrases
                        .iter()
                        .map(|p| phrase_count(&tokens, &words(p)))
                        .sum();
                    if (count as u128) * 100 > (*max_per_100_words as u128) * (tokens.len() as u128)
                    {
                        add(format!("{count} hedge matches in {} words; maximum is {max_per_100_words} per 100 words.", tokens.len()));
                    }
                }
                Check::Citations {
                    claim_phrases,
                    numeric_claims,
                } => {
                    for id in &citations {
                        if !source_ids.contains(*id) {
                            add(format!("Unknown source id: {id}"));
                        }
                    }
                    let claim = (*numeric_claims
                        && tokens.iter().any(|t| t.chars().any(char::is_numeric)))
                        || claim_phrases
                            .iter()
                            .any(|p| phrase_count(&tokens, &words(p)) > 0);
                    if claim && !citations.iter().any(|id| source_ids.contains(*id)) {
                        add("Potential claim has no recognized source citation.".into());
                    }
                }
                _ => {}
            }
        }
    }
    notes.sort_by(|a, b| (&a.line, &a.rule_id, &a.message).cmp(&(&b.line, &b.rule_id, &b.message)));
    notes.dedup();
    Ok(Review {
        passed: notes.is_empty(),
        notes,
    })
}

// Skip valid citation-shaped spans while splitting so punctuation within an ID
// cannot terminate a sentence. Malformed markers remain ordinary draft text.
fn sentences(draft: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut start = 1;
    for (index, line) in draft.lines().enumerate() {
        if line.trim().is_empty() {
            if !current.trim().is_empty() {
                result.push((start, std::mem::take(&mut current)));
            }
            continue;
        }
        let mut rest = line;
        while !rest.is_empty() {
            if current.trim().is_empty() {
                start = index + 1;
            }
            if rest.starts_with("[source:") {
                if let Some(end) = rest.find(']') {
                    current.push_str(&rest[..=end]);
                    rest = &rest[end + 1..];
                    continue;
                }
            }
            let ch = rest.chars().next().expect("nonempty remainder");
            current.push(ch);
            rest = &rest[ch.len_utf8()..];
            if matches!(ch, '.' | '!' | '?') {
                result.push((start, std::mem::take(&mut current)));
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
    }
    if !current.trim().is_empty() {
        result.push((start, current));
    }
    result
}

fn citations(sentence: &str) -> (String, Vec<&str>) {
    let mut prose = String::new();
    let mut ids = Vec::new();
    let mut rest = sentence;
    while let Some(start) = rest.find("[source:") {
        prose.push_str(&rest[..start]);
        let after = &rest[start + "[source:".len()..];
        let Some(end) = after.find(']') else {
            prose.push_str(&rest[start..]);
            return (prose, ids);
        };
        ids.push(&after[..end]);
        prose.push(' ');
        rest = &after[end + 1..];
    }
    prose.push_str(rest);
    (prose, ids)
}
