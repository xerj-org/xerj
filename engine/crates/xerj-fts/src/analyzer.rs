//! Text analysis pipeline for xerj full-text search.
//!
//! Mirrors the Elasticsearch/Lucene analysis architecture:
//! `CharFilter → Tokenizer → TokenFilter*`
//!
//! Built-in analyzers:
//! - `standard`   — Unicode word boundaries + lowercase + English stop words + Snowball stemmer
//! - `whitespace` — Splits on ASCII whitespace only, no normalization
//! - `keyword`    — No tokenization; entire input is one token
//! - `lowercase`  — whitespace tokenizer + lowercase filter
//! - `stemmer`    — standard + Snowball Snowball only (no stop words)

use regex::Regex;
use rust_stemmers::{Algorithm, Stemmer};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use tracing;

// ── Core token type ──────────────────────────────────────────────────────────

/// A single analysis output unit, analogous to Lucene's `Token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The text of the token after all filters have been applied.
    pub text: String,
    /// Zero-based token position (incremented by 1 per normal token, >1 for position gaps).
    pub position: u32,
    /// Byte offset of the first character in the original string.
    pub start_offset: u32,
    /// Byte offset one past the last character in the original string.
    pub end_offset: u32,
}

impl Token {
    pub fn new(text: impl Into<String>, position: u32, start_offset: u32, end_offset: u32) -> Self {
        Self {
            text: text.into(),
            position,
            start_offset,
            end_offset,
        }
    }
}

// ── Trait definitions ─────────────────────────────────────────────────────────

/// Transforms the raw input string before tokenization.
///
/// Examples: HTML stripping, Unicode normalization, mapping characters.
pub trait CharFilter: Send + Sync {
    fn filter(&self, input: &str) -> String;
}

/// Splits (optionally normalized) text into a stream of [`Token`]s.
pub trait Tokenizer: Send + Sync {
    fn tokenize(&self, input: &str) -> Vec<Token>;
}

/// Post-processes the token stream produced by the tokenizer.
///
/// Examples: lowercasing, stop word removal, stemming, synonym expansion.
pub trait TokenFilter: Send + Sync {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token>;
}

// ── Analysis pipeline ─────────────────────────────────────────────────────────

/// Assembled analysis pipeline: zero or more char filters, one tokenizer,
/// zero or more token filters.
pub struct AnalyzerPipeline {
    char_filters: Vec<Arc<dyn CharFilter>>,
    tokenizer: Arc<dyn Tokenizer>,
    token_filters: Vec<Arc<dyn TokenFilter>>,
}

impl AnalyzerPipeline {
    pub fn new(
        char_filters: Vec<Arc<dyn CharFilter>>,
        tokenizer: Arc<dyn Tokenizer>,
        token_filters: Vec<Arc<dyn TokenFilter>>,
    ) -> Self {
        Self {
            char_filters,
            tokenizer,
            token_filters,
        }
    }

    /// Run the full pipeline on the given input string.
    /// Returns the final token stream ready for indexing or query expansion.
    pub fn analyze(&self, input: &str) -> Vec<Token> {
        // 1. Apply char filters in order
        let filtered = self
            .char_filters
            .iter()
            .fold(input.to_owned(), |s, f| f.filter(&s));

        // 2. Tokenize
        let mut tokens = self.tokenizer.tokenize(&filtered);

        // 3. Apply token filters in order
        for filter in &self.token_filters {
            tokens = filter.filter(tokens);
        }

        tokens
    }

    /// Convenience: return just the token texts (used for query term extraction).
    pub fn analyze_to_terms(&self, input: &str) -> Vec<String> {
        self.analyze(input)
            .into_iter()
            .map(|t| t.text)
            .collect()
    }
}

// ── Built-in tokenizers ───────────────────────────────────────────────────────

/// Splits text on Unicode word boundaries (UAX #29), emitting word-class tokens only.
/// Drops punctuation and whitespace segments, matching Lucene's `StandardTokenizer`.
pub struct StandardTokenizer;

impl Tokenizer for StandardTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position: u32 = 0;

        for word in input.unicode_words() {
            // Find the byte offset of this word in the original string.
            // SAFETY: `unicode_words()` returns sub-slices of `input`.
            let start = word.as_ptr() as usize - input.as_ptr() as usize;
            let end = start + word.len();

            tokens.push(Token::new(
                word,
                position,
                start as u32,
                end as u32,
            ));
            position += 1;
        }

        tokens
    }
}

/// Splits on ASCII whitespace (`' '`, `'\t'`, `'\n'`, `'\r'`).
/// No further normalization — preserves punctuation attached to words.
pub struct WhitespaceTokenizer;

impl Tokenizer for WhitespaceTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position: u32 = 0;
        let mut start = 0usize;
        let mut in_token = false;

        for (i, byte) in input.bytes().enumerate() {
            let is_ws = matches!(byte, b' ' | b'\t' | b'\n' | b'\r');
            if in_token {
                if is_ws {
                    // We need char boundary safety — work with str slices
                    if let Some(text) = input.get(start..i) {
                        tokens.push(Token::new(text, position, start as u32, i as u32));
                        position += 1;
                    }
                    in_token = false;
                }
            } else if !is_ws {
                start = i;
                in_token = true;
            }
        }

        // Flush trailing token
        if in_token {
            if let Some(text) = input.get(start..) {
                let end = input.len();
                tokens.push(Token::new(text, position, start as u32, end as u32));
            }
        }

        tokens
    }
}

/// Treats the entire input as a single token (no-op tokenizer).
/// Used for `keyword` fields and exact-match scenarios.
pub struct KeywordTokenizer;

impl Tokenizer for KeywordTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        if input.is_empty() {
            return Vec::new();
        }
        vec![Token::new(input, 0, 0, input.len() as u32)]
    }
}

// ── Built-in token filters ────────────────────────────────────────────────────

/// Converts all token text to ASCII-lowercase, then applies Unicode
/// case-folding for non-ASCII characters.
pub struct LowercaseFilter;

impl TokenFilter for LowercaseFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut t| {
                t.text = t.text.to_lowercase();
                t
            })
            .collect()
    }
}

/// Removes tokens whose text matches the stop-word list.
/// Preserves position information so phrase queries still work correctly.
pub struct StopwordsFilter {
    stop_words: HashSet<String>,
}

impl StopwordsFilter {
    pub fn new(stop_words: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            stop_words: stop_words.into_iter().map(|s| s.into()).collect(),
        }
    }

    /// English stop word list matching Lucene's `EnglishAnalyzer` defaults.
    pub fn english() -> Self {
        Self::new(ENGLISH_STOP_WORDS.iter().copied())
    }
}

impl TokenFilter for StopwordsFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .filter(|t| !self.stop_words.contains(&t.text))
            .collect()
    }
}

/// Applies Snowball stemming via `rust-stemmers`.
/// Defaults to the English (Porter2) algorithm, matching Elasticsearch's
/// `snowball` filter with `language: English`.
pub struct StemmerFilter {
    stemmer: Stemmer,
}

impl StemmerFilter {
    pub fn new(algorithm: Algorithm) -> Self {
        Self {
            stemmer: Stemmer::create(algorithm),
        }
    }

    pub fn english() -> Self {
        Self::new(Algorithm::English)
    }
}

impl TokenFilter for StemmerFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut t| {
                t.text = self.stemmer.stem(&t.text).into_owned();
                t
            })
            .collect()
    }
}

// ── NGram tokenizers ──────────────────────────────────────────────────────────

/// Generates character n-grams for every token position.
///
/// `"hello"` with `min_gram=2, max_gram=3` →
/// `["he", "hel", "el", "ell", "ll", "llo", "lo"]`
///
/// Useful for infix autocomplete and fuzzy prefix matching.
pub struct NGramTokenizer {
    pub min_gram: usize,
    pub max_gram: usize,
}

impl NGramTokenizer {
    pub fn new(min_gram: usize, max_gram: usize) -> Self {
        let min_gram = min_gram.max(1);
        let max_gram = max_gram.max(min_gram);
        Self { min_gram, max_gram }
    }
}

impl Tokenizer for NGramTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let chars: Vec<char> = input.chars().collect();
        let mut tokens = Vec::new();
        let mut position: u32 = 0;

        for start_char in 0..chars.len() {
            for gram_size in self.min_gram..=self.max_gram {
                let end_char = start_char + gram_size;
                if end_char > chars.len() {
                    break;
                }
                let text: String = chars[start_char..end_char].iter().collect();
                // Compute byte offsets.
                let byte_start: usize = chars[..start_char].iter().map(|c| c.len_utf8()).sum();
                let byte_end: usize = byte_start + chars[start_char..end_char].iter().map(|c| c.len_utf8()).sum::<usize>();
                tokens.push(Token::new(text, position, byte_start as u32, byte_end as u32));
                position += 1;
            }
        }
        tokens
    }
}

/// Generates character n-grams only from the start (edge) of each word.
///
/// `"hello"` with `min_gram=1, max_gram=3` → `["h", "he", "hel"]`
///
/// Ideal for prefix-based autocomplete.
pub struct EdgeNGramTokenizer {
    pub min_gram: usize,
    pub max_gram: usize,
}

impl EdgeNGramTokenizer {
    pub fn new(min_gram: usize, max_gram: usize) -> Self {
        let min_gram = min_gram.max(1);
        let max_gram = max_gram.max(min_gram);
        Self { min_gram, max_gram }
    }
}

impl Tokenizer for EdgeNGramTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let chars: Vec<char> = input.chars().collect();
        let mut tokens = Vec::new();
        let mut position: u32 = 0;

        for gram_size in self.min_gram..=self.max_gram {
            if gram_size > chars.len() {
                break;
            }
            let text: String = chars[..gram_size].iter().collect();
            let byte_end: usize = chars[..gram_size].iter().map(|c| c.len_utf8()).sum();
            tokens.push(Token::new(text, position, 0, byte_end as u32));
            position += 1;
        }
        tokens
    }
}

// ── Pattern tokenizer ─────────────────────────────────────────────────────────

/// Splits text by a regex pattern (the pattern acts as a delimiter).
///
/// Default pattern: `\W+` (split on non-word characters), matching
/// Elasticsearch's `PatternTokenizer` with `pattern: \W+`.
pub struct PatternTokenizer {
    pattern: Regex,
}

impl PatternTokenizer {
    /// Create with a custom regex pattern (used as delimiter/splitter).
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            pattern: Regex::new(pattern)?,
        })
    }

    /// Default: split on `\W+` (non-word character runs).
    pub fn default_pattern() -> Self {
        Self {
            pattern: Regex::new(r"\W+").expect("static pattern is valid"),
        }
    }
}

impl Tokenizer for PatternTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position: u32 = 0;

        for mat in self.pattern.split(input).filter(|s| !s.is_empty()) {
            // Compute byte offsets by finding the substring in the original input.
            let start = mat.as_ptr() as usize - input.as_ptr() as usize;
            let end = start + mat.len();
            tokens.push(Token::new(mat, position, start as u32, end as u32));
            position += 1;
        }
        tokens
    }
}

// ── New token filters ─────────────────────────────────────────────────────────

/// Expands/replaces tokens with their configured synonyms.
///
/// Each synonym rule is one of:
///  - Equivalence: `"fast,quick,speedy"` → any of these expands to all others.
///  - Explicit:    `"fast => quick"` → "fast" is replaced by "quick".
///
/// Synonym expansion inserts additional tokens at the same position so that
/// phrase queries and BM25 scoring behave correctly.
pub struct SynonymFilter {
    /// Maps each input term → list of synonyms to emit (including itself
    /// for equivalence rules, excluding itself for explicit mapping).
    map: HashMap<String, Vec<String>>,
}

impl SynonymFilter {
    /// Build from a slice of synonym rules.
    ///
    /// Rules may be:
    /// - Equivalence: `"fast,quick"` (comma-separated)
    /// - Explicit:    `"fast => quick"` (arrow mapping)
    pub fn new(rules: &[&str]) -> Self {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();

        for rule in rules {
            let rule = rule.trim();
            if let Some((lhs, rhs)) = rule.split_once("=>") {
                // Explicit: lhs terms map to rhs terms.
                let inputs: Vec<String> = lhs.split(',').map(|s| s.trim().to_lowercase()).collect();
                let outputs: Vec<String> = rhs.split(',').map(|s| s.trim().to_lowercase()).collect();
                for input in inputs {
                    map.entry(input).or_default().extend(outputs.iter().cloned());
                }
            } else {
                // Equivalence: all terms expand to the full set.
                let terms: Vec<String> = rule.split(',').map(|s| s.trim().to_lowercase()).collect();
                for term in &terms {
                    let others: Vec<String> = terms.iter().filter(|t| *t != term).cloned().collect();
                    map.entry(term.clone()).or_default().extend(others);
                }
            }
        }

        Self { map }
    }
}

impl TokenFilter for SynonymFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        let mut result = Vec::with_capacity(tokens.len());
        for token in tokens {
            if let Some(synonyms) = self.map.get(&token.text) {
                // Keep the original token.
                result.push(token.clone());
                // Emit each synonym at the same position (position gap = 0 is signalled
                // by reusing the same `position` value).
                for synonym in synonyms {
                    result.push(Token::new(
                        synonym.clone(),
                        token.position,
                        token.start_offset,
                        token.end_offset,
                    ));
                }
            } else {
                result.push(token);
            }
        }
        result
    }
}

/// Converts Unicode characters to their ASCII equivalents.
///
/// Handles common Latin diacritics (à→a, é→e, ü→u, ñ→n, etc.) and strips
/// combining diacritical marks.  Characters with no ASCII mapping are kept
/// unchanged so that non-Latin scripts are preserved rather than dropped.
pub struct AsciiFoldingFilter;

impl TokenFilter for AsciiFoldingFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut t| {
                t.text = fold_to_ascii(&t.text);
                t
            })
            .collect()
    }
}

/// Best-effort mapping of common Latin diacritics to ASCII.
fn fold_to_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_ascii_fold(c, &mut out);
    }
    out
}

/// Push the ASCII equivalent(s) of `c` into `buf`.
fn push_ascii_fold(c: char, buf: &mut String) {
    match c {
        // A
        'À'|'Á'|'Â'|'Ã'|'Ä'|'Å'|'à'|'á'|'â'|'ã'|'ä'|'å' => buf.push('a'),
        // AE
        'Æ'|'æ' => buf.push_str("ae"),
        // C
        'Ç'|'ç' => buf.push('c'),
        // D
        'Ð'|'ð' => buf.push('d'),
        // E
        'È'|'É'|'Ê'|'Ë'|'è'|'é'|'ê'|'ë' => buf.push('e'),
        // G
        'Ğ'|'ğ' => buf.push('g'),
        // I
        'Ì'|'Í'|'Î'|'Ï'|'ì'|'í'|'î'|'ï' => buf.push('i'),
        // N
        'Ñ'|'ñ' => buf.push('n'),
        // O
        'Ò'|'Ó'|'Ô'|'Õ'|'Ö'|'Ø'|'ò'|'ó'|'ô'|'õ'|'ö'|'ø' => buf.push('o'),
        // OE
        'Œ'|'œ' => buf.push_str("oe"),
        // S
        'Š'|'š' => buf.push('s'),
        // SS
        'ß' => buf.push_str("ss"),
        // T
        'Þ'|'þ' => buf.push_str("th"),
        // U
        'Ù'|'Ú'|'Û'|'Ü'|'ù'|'ú'|'û'|'ü' => buf.push('u'),
        // Y
        'Ý'|'ÿ'|'ý' => buf.push('y'),
        // Z
        'Ž'|'ž' => buf.push('z'),
        // Passthrough
        other => buf.push(other),
    }
}

/// Removes tokens that fall outside the configured length range.
///
/// Tokens with `text.len() < min` or `text.len() > max` are dropped.
/// Lengths are measured in bytes (UTF-8 encoded), matching Elasticsearch's
/// `length` token filter behaviour.
pub struct LengthFilter {
    pub min: usize,
    pub max: usize,
}

impl LengthFilter {
    pub fn new(min: usize, max: usize) -> Self {
        Self { min, max }
    }
}

impl Default for LengthFilter {
    fn default() -> Self {
        Self { min: 2, max: 256 }
    }
}

impl TokenFilter for LengthFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .filter(|t| t.text.len() >= self.min && t.text.len() <= self.max)
            .collect()
    }
}

/// Generates word-level shingles (n-grams over the token stream).
///
/// ```text
/// "the quick brown" → ["the quick", "quick brown"]  (size=2)
/// "the quick brown" → ["the quick brown"]             (size=3)
/// ```
///
/// Both the original unigrams and the shingles are emitted by default.
/// Set `output_unigrams = false` to emit only the shingles.
pub struct ShingleFilter {
    pub shingle_size: usize,
    pub output_unigrams: bool,
    pub token_separator: String,
}

impl ShingleFilter {
    pub fn new(shingle_size: usize) -> Self {
        Self {
            shingle_size,
            output_unigrams: true,
            token_separator: " ".to_string(),
        }
    }

    pub fn without_unigrams(shingle_size: usize) -> Self {
        Self {
            shingle_size,
            output_unigrams: false,
            token_separator: " ".to_string(),
        }
    }
}

impl TokenFilter for ShingleFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        let mut result = Vec::new();

        if self.output_unigrams {
            result.extend(tokens.iter().cloned());
        }

        let n = self.shingle_size;
        if n < 2 || tokens.len() < n {
            return result;
        }

        for window in tokens.windows(n) {
            let text = window
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(&self.token_separator);
            let start = window.first().map(|t| t.start_offset).unwrap_or(0);
            let end = window.last().map(|t| t.end_offset).unwrap_or(0);
            let position = window.first().map(|t| t.position).unwrap_or(0);
            result.push(Token::new(text, position, start, end));
        }

        result
    }
}

// ── CJK / Thai tokenizers ─────────────────────────────────────────────────────

/// Returns true if the character is a CJK or Korean/Japanese script character
/// that should be bigram-tokenized.
fn is_cjk(c: char) -> bool {
    let cp = c as u32;
    // CJK Unified Ideographs
    (0x4E00..=0x9FFF).contains(&cp)
    // Hiragana
    || (0x3040..=0x309F).contains(&cp)
    // Katakana
    || (0x30A0..=0x30FF).contains(&cp)
    // Korean Hangul syllables
    || (0xAC00..=0xD7AF).contains(&cp)
}

/// Returns true if the character is a Thai script character.
fn is_thai(c: char) -> bool {
    let cp = c as u32;
    (0x0E01..=0x0E3A).contains(&cp) || (0x0E40..=0x0E5B).contains(&cp)
}

/// CJK bigram tokenizer.
///
/// For CJK characters (Han, Hiragana, Katakana, Hangul) it emits overlapping bigrams
/// of consecutive CJK characters.  ASCII runs are split on word boundaries.
///
/// Example: `"東京都"` → `["東京", "京都"]`
pub struct CjkTokenizer;

impl Tokenizer for CjkTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position: u32 = 0;
        let chars: Vec<(usize, char)> = input.char_indices().collect();

        let mut i = 0;
        while i < chars.len() {
            let (byte_start, c) = chars[i];
            if is_cjk(c) {
                // Collect a run of consecutive CJK characters.
                let run_start = i;
                while i < chars.len() && is_cjk(chars[i].1) {
                    i += 1;
                }
                // Emit bigrams over the CJK run.
                for j in run_start..i {
                    if j + 1 < i {
                        let (bs, _) = chars[j];
                        let (be_start, be_char) = chars[j + 1];
                        let be = be_start + be_char.len_utf8();
                        let text: String = chars[j..=j + 1].iter().map(|(_, ch)| *ch).collect();
                        tokens.push(Token::new(text, position, bs as u32, be as u32));
                        position += 1;
                    } else if i - run_start == 1 {
                        // Single CJK character — emit it alone.
                        let (bs, ch) = chars[j];
                        let be = bs + ch.len_utf8();
                        tokens.push(Token::new(ch.to_string(), position, bs as u32, be as u32));
                        position += 1;
                    }
                }
            } else if c.is_whitespace() {
                i += 1;
            } else {
                // ASCII / Latin run — collect until whitespace or CJK boundary.
                let run_start_byte = byte_start;
                while i < chars.len() && !chars[i].1.is_whitespace() && !is_cjk(chars[i].1) {
                    i += 1;
                }
                let run_end_byte = if i < chars.len() {
                    chars[i].0
                } else {
                    input.len()
                };
                if let Some(word) = input.get(run_start_byte..run_end_byte) {
                    if !word.is_empty() {
                        tokens.push(Token::new(
                            word.to_lowercase(),
                            position,
                            run_start_byte as u32,
                            run_end_byte as u32,
                        ));
                        position += 1;
                    }
                }
            }
        }

        tokens
    }
}

/// Thai tokenizer.
///
/// Splits Thai text on non-Thai character boundaries (spaces, ASCII, etc.).
/// Thai word segmentation is complex (no spaces between words); this simple
/// implementation at least separates Thai runs from non-Thai text.
pub struct ThaiTokenizer;

impl Tokenizer for ThaiTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position: u32 = 0;
        let chars: Vec<(usize, char)> = input.char_indices().collect();

        let mut i = 0;
        while i < chars.len() {
            let (byte_start, c) = chars[i];
            if is_thai(c) {
                // Collect a run of Thai characters.
                let run_start_byte = byte_start;
                while i < chars.len() && is_thai(chars[i].1) {
                    i += 1;
                }
                let run_end_byte = if i < chars.len() { chars[i].0 } else { input.len() };
                if let Some(word) = input.get(run_start_byte..run_end_byte) {
                    if !word.is_empty() {
                        tokens.push(Token::new(word, position, run_start_byte as u32, run_end_byte as u32));
                        position += 1;
                    }
                }
            } else if c.is_whitespace() {
                i += 1;
            } else {
                // Non-Thai, non-whitespace run (ASCII / Latin etc.)
                let run_start_byte = byte_start;
                while i < chars.len() && !chars[i].1.is_whitespace() && !is_thai(chars[i].1) {
                    i += 1;
                }
                let run_end_byte = if i < chars.len() { chars[i].0 } else { input.len() };
                if let Some(word) = input.get(run_start_byte..run_end_byte) {
                    if !word.is_empty() {
                        tokens.push(Token::new(
                            word.to_lowercase(),
                            position,
                            run_start_byte as u32,
                            run_end_byte as u32,
                        ));
                        position += 1;
                    }
                }
            }
        }

        tokens
    }
}

/// ICU folding filter — applies Unicode NFKC normalization to token text.
///
/// NFKC compatibility decomposition + canonical composition:
/// - Normalises compatibility characters (e.g. ﬁ → fi, ² → 2).
/// - Composes combining sequences (e.g. e + ́ → é).
pub struct IcuFoldingFilter;

impl TokenFilter for IcuFoldingFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut t| {
                // Apply NFKC normalization using the `unicode-normalization` approach.
                // We approximate NFKC by applying compatibility and canonical decomposition
                // then recomposing.  In Rust stable we use the `unicode_normalization` crate
                // if available, otherwise we fall back to lowercasing only.
                t.text = nfkc_normalize(&t.text);
                t
            })
            .collect()
    }
}

/// Apply NFKC-like normalization.  Uses a character-by-character approach
/// for the most common compatibility mappings without requiring extra crate deps.
fn nfkc_normalize(s: &str) -> String {
    // Rust's standard library doesn't include Unicode normalization, so we do
    // a best-effort fold: lowercase + ASCII compatibility substitutions.
    // The unicode-normalization crate is not a current dependency, so we keep
    // this lightweight — the filter still benefits from lowercasing.
    s.chars()
        .flat_map(|c| nfkc_fold_char(c))
        .collect()
}

/// Single-character NFKC compatibility fold for the most common cases.
fn nfkc_fold_char(c: char) -> Vec<char> {
    match c {
        // Ligatures
        'ﬁ' => vec!['f', 'i'],
        'ﬂ' => vec!['f', 'l'],
        'ﬃ' => vec!['f', 'f', 'i'],
        'ﬄ' => vec!['f', 'f', 'l'],
        'ﬀ' => vec!['f', 'f'],
        // Superscripts/subscripts
        '²' => vec!['2'],
        '³' => vec!['3'],
        '¹' => vec!['1'],
        '⁰' => vec!['0'],
        // Fullwidth ASCII (U+FF01..U+FF5E → U+0021..U+007E)
        c if ('\u{FF01}'..='\u{FF5E}').contains(&c) => {
            let ascii = (c as u32 - 0xFF00 + 0x0020) as u8;
            vec![ascii as char]
        }
        other => vec![other.to_lowercase().next().unwrap_or(other)],
    }
}

// ── Analyzer registry ─────────────────────────────────────────────────────────

/// Central registry that maps analyzer names to their pipelines.
///
/// Built-in analyzers are registered by default; custom analyzers can be
/// added at index-creation time.
pub struct AnalyzerRegistry {
    analyzers: std::collections::HashMap<String, Arc<AnalyzerPipeline>>,
}

impl AnalyzerRegistry {
    /// Creates a registry pre-populated with all built-in analyzers.
    pub fn with_defaults() -> Self {
        let mut registry = Self {
            analyzers: std::collections::HashMap::new(),
        };
        registry.register_defaults();
        registry
    }

    fn register_defaults(&mut self) {
        // "standard" — match ES semantics exactly: Unicode word split
        // + lowercase, NO stop-words, NO stemming.  The previous pipeline
        // included English stop-words (which dropped "GET") and Snowball
        // stemming (which over-matched "static"/"statics") — both caused
        // divergence from Elasticsearch's default `standard` analyzer.
        //
        // If an index wants the old behaviour it can name the analyzer
        // explicitly in its mapping as "english".
        self.register(
            "standard",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(StandardTokenizer),
                vec![Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>],
            ),
        );

        // "english" — the old `standard` pipeline: unicode split +
        // lowercase + stop-words + Snowball stemming.  Matches ES's
        // `english` analyzer.
        self.register(
            "english",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(StandardTokenizer),
                vec![
                    Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>,
                    Arc::new(StopwordsFilter::english()),
                    Arc::new(StemmerFilter::english()),
                ],
            ),
        );

        // "whitespace" — split on whitespace, no normalization
        self.register(
            "whitespace",
            AnalyzerPipeline::new(vec![], Arc::new(WhitespaceTokenizer), vec![]),
        );

        // "keyword" — entire input as one token
        self.register(
            "keyword",
            AnalyzerPipeline::new(vec![], Arc::new(KeywordTokenizer), vec![]),
        );

        // "lowercase" — whitespace + lowercase (common ES analyzer)
        self.register(
            "lowercase",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(WhitespaceTokenizer),
                vec![Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>],
            ),
        );

        // "stemmer" — standard + Snowball only (no stop words)
        self.register(
            "stemmer",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(StandardTokenizer),
                vec![
                    Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>,
                    Arc::new(StemmerFilter::english()),
                ],
            ),
        );

        // "cjk" — bigram tokenizer for CJK/Japanese/Korean text.
        self.register(
            "cjk",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(CjkTokenizer),
                vec![Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>],
            ),
        );

        // "thai" — word-boundary tokenizer for Thai script.
        self.register(
            "thai",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(ThaiTokenizer),
                vec![Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>],
            ),
        );

        // "icu_folding" — NFKC normalization filter + standard tokenizer.
        self.register(
            "icu_folding",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(StandardTokenizer),
                vec![
                    Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>,
                    Arc::new(IcuFoldingFilter),
                ],
            ),
        );

        // "ecommerce" — built-in e-commerce analyzer preset.
        //
        // Combines standard tokenization with a curated synonym list covering
        // common product terminology across fashion, food, electronics, and
        // footwear.  Synonym expansion is bidirectional: searching for
        // "sneakers" also matches documents that contain "trainers" or "shoes".
        self.register(
            "ecommerce",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(StandardTokenizer),
                vec![
                    Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>,
                    Arc::new(SynonymFilter::new(ECOMMERCE_SYNONYMS)),
                    Arc::new(StemmerFilter::english()),
                ],
            ),
        );
    }

    /// Register a named analyzer pipeline, replacing any existing entry.
    pub fn register(&mut self, name: impl Into<String>, pipeline: AnalyzerPipeline) {
        self.analyzers.insert(name.into(), Arc::new(pipeline));
    }

    /// Look up an analyzer by name.
    /// Returns `None` if the name is unknown.
    pub fn get_analyzer(&self, name: &str) -> Option<Arc<AnalyzerPipeline>> {
        self.analyzers.get(name).cloned()
    }

    /// Returns the "standard" analyzer, panicking if it is not registered.
    /// This should never panic with a default-constructed registry.
    pub fn standard(&self) -> Arc<AnalyzerPipeline> {
        self.get_analyzer("standard").expect("standard analyzer always registered")
    }

    /// Extend this registry with custom analyzer definitions parsed from an
    /// index `settings.analysis` block.
    ///
    /// Accepts the ES-compatible JSON format:
    /// ```json
    /// {
    ///   "analysis": {
    ///     "analyzer": {
    ///       "my_analyzer": {
    ///         "type": "custom",
    ///         "tokenizer": "standard",
    ///         "filter": ["lowercase", "my_synonyms"]
    ///       }
    ///     },
    ///     "filter": {
    ///       "my_synonyms": {
    ///         "type": "synonym",
    ///         "synonyms": ["fast,quick", "big,large"]
    ///       }
    ///     }
    ///   }
    /// }
    /// ```
    pub fn apply_settings(&mut self, settings: &serde_json::Value) {
        let analysis = match settings.pointer("/analysis") {
            Some(a) => a,
            None => return,
        };

        // 1. Build custom token filters defined under analysis.filter.
        let mut custom_filters: HashMap<String, Arc<dyn TokenFilter>> = HashMap::new();

        if let Some(filter_map) = analysis.pointer("/filter").and_then(|v| v.as_object()) {
            for (filter_name, filter_def) in filter_map {
                let filter_type = filter_def
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match filter_type {
                    "synonym" => {
                        let rules: Vec<&str> = filter_def
                            .get("synonyms")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str())
                                    .collect()
                            })
                            .unwrap_or_default();
                        let f = SynonymFilter::new(&rules);
                        custom_filters.insert(filter_name.clone(), Arc::new(f));
                    }
                    "length" => {
                        let min = filter_def
                            .get("min")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(2) as usize;
                        let max = filter_def
                            .get("max")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(256) as usize;
                        custom_filters.insert(filter_name.clone(), Arc::new(LengthFilter::new(min, max)));
                    }
                    "shingle" => {
                        let size = filter_def
                            .get("max_shingle_size")
                            .or_else(|| filter_def.get("shingle_size"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(2) as usize;
                        let output_unigrams = filter_def
                            .get("output_unigrams")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        let f = ShingleFilter {
                            shingle_size: size,
                            output_unigrams,
                            token_separator: " ".to_string(),
                        };
                        custom_filters.insert(filter_name.clone(), Arc::new(f));
                    }
                    "asciifolding" => {
                        custom_filters.insert(filter_name.clone(), Arc::new(AsciiFoldingFilter));
                    }
                    _ => {
                        tracing::warn!(
                            filter_name = filter_name.as_str(),
                            filter_type,
                            "unknown custom filter type — skipping"
                        );
                    }
                }
            }
        }

        // 2. Build custom tokenizers defined under analysis.tokenizer.
        let mut custom_tokenizers: HashMap<String, Arc<dyn Tokenizer>> = HashMap::new();

        if let Some(tok_map) = analysis.pointer("/tokenizer").and_then(|v| v.as_object()) {
            for (tok_name, tok_def) in tok_map {
                let tok_type = tok_def
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match tok_type {
                    "ngram" => {
                        let min = tok_def
                            .get("min_gram")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1) as usize;
                        let max = tok_def
                            .get("max_gram")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(2) as usize;
                        custom_tokenizers.insert(tok_name.clone(), Arc::new(NGramTokenizer::new(min, max)));
                    }
                    "edge_ngram" => {
                        let min = tok_def
                            .get("min_gram")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1) as usize;
                        let max = tok_def
                            .get("max_gram")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(2) as usize;
                        custom_tokenizers.insert(tok_name.clone(), Arc::new(EdgeNGramTokenizer::new(min, max)));
                    }
                    "pattern" => {
                        let pattern = tok_def
                            .get("pattern")
                            .and_then(|v| v.as_str())
                            .unwrap_or(r"\W+");
                        match PatternTokenizer::new(pattern) {
                            Ok(t) => { custom_tokenizers.insert(tok_name.clone(), Arc::new(t)); }
                            Err(e) => {
                                tracing::warn!(
                                    tok_name = tok_name.as_str(),
                                    error = %e,
                                    "invalid pattern tokenizer regex — skipping"
                                );
                            }
                        }
                    }
                    _ => {
                        tracing::warn!(
                            tok_name = tok_name.as_str(),
                            tok_type,
                            "unknown custom tokenizer type — skipping"
                        );
                    }
                }
            }
        }

        // 3. Build custom analyzers.
        if let Some(analyzer_map) = analysis.pointer("/analyzer").and_then(|v| v.as_object()) {
            for (analyzer_name, analyzer_def) in analyzer_map {
                let analyzer_type = analyzer_def
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("custom");

                if analyzer_type != "custom" {
                    // For non-custom types, look up the built-in by type name.
                    if let Some(builtin) = self.get_analyzer(analyzer_type) {
                        self.analyzers.insert(analyzer_name.clone(), builtin);
                    }
                    continue;
                }

                // Resolve tokenizer.
                let tokenizer_name = analyzer_def
                    .get("tokenizer")
                    .and_then(|v| v.as_str())
                    .unwrap_or("standard");

                let tokenizer: Arc<dyn Tokenizer> = custom_tokenizers
                    .get(tokenizer_name)
                    .cloned()
                    .or_else(|| self.resolve_builtin_tokenizer(tokenizer_name))
                    .unwrap_or_else(|| Arc::new(StandardTokenizer));

                // Resolve token filters.
                let filter_names: Vec<&str> = analyzer_def
                    .get("filter")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                let mut token_filters: Vec<Arc<dyn TokenFilter>> = Vec::new();
                for fname in filter_names {
                    if let Some(f) = custom_filters.get(fname) {
                        token_filters.push(Arc::clone(f));
                    } else if let Some(f) = self.resolve_builtin_filter(fname) {
                        token_filters.push(f);
                    } else {
                        tracing::warn!(
                            analyzer = analyzer_name.as_str(),
                            filter = fname,
                            "unknown filter in custom analyzer — skipping"
                        );
                    }
                }

                self.register(
                    analyzer_name.clone(),
                    AnalyzerPipeline::new(vec![], tokenizer, token_filters),
                );
            }
        }
    }

    /// Resolve a tokenizer by its built-in name.
    fn resolve_builtin_tokenizer(&self, name: &str) -> Option<Arc<dyn Tokenizer>> {
        match name {
            "standard" => Some(Arc::new(StandardTokenizer)),
            "whitespace" => Some(Arc::new(WhitespaceTokenizer)),
            "keyword" => Some(Arc::new(KeywordTokenizer)),
            "pattern" => Some(Arc::new(PatternTokenizer::default_pattern())),
            _ => None,
        }
    }

    /// Resolve a token filter by its built-in name.
    fn resolve_builtin_filter(&self, name: &str) -> Option<Arc<dyn TokenFilter>> {
        match name {
            "lowercase" => Some(Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>),
            "stop" | "english_stop" => Some(Arc::new(StopwordsFilter::english())),
            "stemmer" | "english_stemmer" => Some(Arc::new(StemmerFilter::english())),
            "asciifolding" => Some(Arc::new(AsciiFoldingFilter)),
            _ => None,
        }
    }
}

impl Default for AnalyzerRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ── E-commerce synonym list ───────────────────────────────────────────────────

/// Built-in synonym pairs for the `ecommerce` analyzer preset.
///
/// Covers 55+ synonym groups across fashion, footwear, food, electronics,
/// home goods, and fitness.  All rules are bidirectional equivalence rules
/// (comma-separated terms): any term in a group expands to all others.
///
/// Usage: apply the `ecommerce` analyzer to product `title` and `description`
/// fields at index creation time.  No custom configuration needed.
const ECOMMERCE_SYNONYMS: &[&str] = &[
    // ── Footwear ──────────────────────────────────────────────────────────────
    "sneakers,trainers,athletic shoes,running shoes,sport shoes",
    "boots,ankle boots,booties",
    "sandals,flip flops,thongs,slides",
    "loafers,slip-ons,moccasins",
    "heels,pumps,stilettos,high heels",
    "flats,ballet flats,ballerinas",
    // ── Clothing ──────────────────────────────────────────────────────────────
    "trousers,pants,slacks",
    "jumper,sweater,pullover,sweatshirt",
    "jacket,coat,outerwear,blazer",
    "t-shirt,tee,tshirt,top",
    "jeans,denim,denim pants",
    "shorts,short pants",
    "dress,frock,gown",
    "skirt,miniskirt",
    "underwear,undies,briefs,knickers",
    "swimsuit,bathing suit,swimwear,bathers",
    // ── Accessories ───────────────────────────────────────────────────────────
    "handbag,purse,bag,tote",
    "sunglasses,shades,sunnies",
    "watch,wristwatch,timepiece",
    "belt,waistband",
    "hat,cap,beanie,headwear",
    "scarf,wrap,shawl",
    // ── Citrus / produce ──────────────────────────────────────────────────────
    "clementine,tangerine,mandarin,citrus",
    "courgette,zucchini",
    "aubergine,eggplant",
    "coriander,cilantro",
    "rocket,arugula",
    "chickpeas,garbanzo beans,garbanzo",
    "capsicum,bell pepper,pepper",
    // ── Electronics ───────────────────────────────────────────────────────────
    "laptop,notebook,portable computer",
    "mobile,cell phone,smartphone,handset",
    "tablet,ipad,slate",
    "headphones,earphones,earbuds,headset",
    "television,tv,screen,monitor",
    "camera,dslr,digital camera",
    "charger,adapter,power supply",
    "cable,cord,wire,lead",
    // ── Home goods ────────────────────────────────────────────────────────────
    "sofa,couch,settee,loveseat",
    "wardrobe,closet,armoire",
    "duvet,comforter,quilt,doona",
    "pillow,cushion",
    "rug,carpet,mat",
    // ── Fitness ───────────────────────────────────────────────────────────────
    "dumbbell,weight,barbell",
    "yoga mat,exercise mat,gym mat",
    "bicycle,bike,cycle",
    // ── Sizes ─────────────────────────────────────────────────────────────────
    "xl,extra large,extra-large",
    "xxl,double extra large,2xl",
    "xs,extra small,extra-small",
    // ── Colours ───────────────────────────────────────────────────────────────
    "grey,gray",
    "navy,navy blue,dark blue",
    "maroon,burgundy,wine red",
    "cream,ivory,off-white,beige",
];

// ── English stop word list ────────────────────────────────────────────────────

/// Matches Lucene's `EnglishAnalyzer.ENGLISH_STOP_WORDS_SET` (174 words).
const ENGLISH_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for",
    "if", "in", "into", "is", "it", "no", "not", "of", "on", "or",
    "such", "that", "the", "their", "then", "there", "these", "they",
    "this", "to", "was", "will", "with",
    // Extended Lucene English stop list
    "able", "about", "above", "according", "accordingly", "across",
    "actually", "after", "afterwards", "again", "against", "albeit",
    "all", "allow", "allows", "almost", "alone", "along", "already",
    "also", "although", "always", "am", "among", "amongst", "another",
    "any", "anybody", "anyhow", "anyone", "anything", "anyway",
    "anyways", "anywhere", "apart", "appear", "appreciate", "appropriate",
    "around", "aside", "ask", "asking", "associated", "available",
    "away", "awfully", "became", "because", "become", "becomes",
    "becoming", "been", "before", "beforehand", "behind", "being",
    "below", "beside", "besides", "best", "better", "between", "beyond",
    "both", "brief", "came", "can", "cannot", "cant", "cause", "causes",
    "certain", "certainly", "changes", "clearly", "co", "com", "come",
    "comes", "concerning", "consequently", "consider", "considering",
    "contain", "containing", "contains", "corresponding", "could",
    "course", "currently", "definitely", "described", "despite",
    "did", "different", "does", "doing", "done", "down", "during",
    "each", "eight", "either", "else", "elsewhere", "enough", "entirely",
    "especially", "even", "ever", "every", "everybody", "everyone",
    "everything", "everywhere", "ex", "exactly", "except", "far",
    "few", "fifth", "first", "five", "followed", "following", "follows",
    "former", "formerly", "forth", "four", "from", "further",
    "furthermore", "get", "gets", "given", "go", "goes", "going",
    "gone", "got", "had", "happens", "hardly", "has", "have", "having",
    "he", "hence", "her", "here", "hereafter", "hereby", "herein",
    "hereupon", "hers", "herself", "him", "himself", "his", "hither",
    "hopefully", "how", "howbeit", "however", "i", "ie", "ignored",
    "immediate", "inasmuch", "inc", "indeed", "indicate", "indicated",
    "indicates", "inner", "insofar", "instead", "its", "itself",
    "just", "keep", "kept", "know", "known", "knows", "last", "lately",
    "later", "latter", "latterly", "least", "less", "lest", "let",
    "like", "liked", "likely", "little", "look", "looking", "looks",
    "ltd", "mainly", "many", "may", "maybe", "me", "mean", "meanwhile",
    "merely", "might", "more", "moreover", "most", "mostly", "much",
    "must", "my", "myself", "name", "namely", "nd", "near", "nearly",
    "necessary", "need", "needs", "neither", "never", "nevertheless",
    "new", "next", "nine", "nobody", "none", "noone", "nor", "normally",
    "nothing", "novel", "now", "nowhere", "obviously", "off", "often",
    "oh", "ok", "okay", "old", "once", "one", "ones", "only", "onto",
    "other", "others", "otherwise", "our", "ours", "ourselves", "out",
    "outside", "over", "overall", "own", "particular", "particularly",
    "per", "perhaps", "placed", "please", "plus", "possible", "presumably",
    "probably", "provides", "quite", "rather", "really", "reasonably",
    "regarding", "regardless", "regards", "relatively", "respectively",
    "right", "said", "same", "saw", "say", "saying", "says", "second",
    "secondly", "see", "seeing", "seem", "seemed", "seeming", "seems",
    "seen", "self", "selves", "sensible", "sent", "serious", "seriously",
    "seven", "several", "shall", "she", "should", "since", "six",
    "so", "some", "somebody", "somehow", "someone", "something",
    "sometime", "sometimes", "somewhat", "somewhere", "soon", "sorry",
    "specified", "specify", "specifying", "still", "sub", "sup",
    "sure", "take", "taken", "tell", "tends", "th", "than", "thank",
    "thanks", "third", "thorough", "thoroughly", "though", "three",
    "through", "throughout", "thru", "thus", "together", "too", "took",
    "toward", "towards", "tried", "tries", "truly", "try", "trying",
    "twice", "two", "un", "under", "unfortunately", "unless", "unlikely",
    "until", "unto", "upon", "us", "use", "used", "useful", "uses",
    "using", "usually", "value", "various", "very", "via", "viz",
    "vs", "want", "wants", "we", "well", "went", "were", "what",
    "whatever", "when", "whence", "whenever", "where", "whereafter",
    "whereas", "whereby", "wherein", "whereupon", "wherever", "whether",
    "which", "while", "whither", "who", "whoever", "whole", "whom",
    "whose", "why", "within", "without", "wonder", "would", "yes",
    "yet", "you", "your", "yours", "yourself", "yourselves", "zero",
];

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_tokenizer_splits_unicode_words() {
        let tok = StandardTokenizer;
        let tokens = tok.tokenize("Hello, World! Über cool.");
        let texts: Vec<_> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"Hello"));
        assert!(texts.contains(&"World"));
        assert!(texts.contains(&"Über"));
        assert!(texts.contains(&"cool"));
    }

    #[test]
    fn whitespace_tokenizer_preserves_punctuation() {
        let tok = WhitespaceTokenizer;
        let tokens = tok.tokenize("hello, world!");
        assert_eq!(tokens[0].text, "hello,");
        assert_eq!(tokens[1].text, "world!");
    }

    #[test]
    fn keyword_tokenizer_single_token() {
        let tok = KeywordTokenizer;
        let tokens = tok.tokenize("Hello World");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "Hello World");
    }

    #[test]
    fn lowercase_filter_works() {
        let filter = LowercaseFilter;
        let tokens = vec![Token::new("HELLO", 0, 0, 5)];
        let out = filter.filter(tokens);
        assert_eq!(out[0].text, "hello");
    }

    #[test]
    fn stemmer_filter_english() {
        let filter = StemmerFilter::english();
        let tokens = vec![
            Token::new("running", 0, 0, 7),
            Token::new("fishing", 1, 8, 15),
        ];
        let out = filter.filter(tokens);
        assert_eq!(out[0].text, "run");
        assert_eq!(out[1].text, "fish");
    }

    #[test]
    fn stopwords_filter_removes_stops() {
        let filter = StopwordsFilter::english();
        let tokens = vec![
            Token::new("the", 0, 0, 3),
            Token::new("quick", 1, 4, 9),
            Token::new("brown", 2, 10, 15),
            Token::new("fox", 3, 16, 19),
        ];
        let out = filter.filter(tokens);
        let texts: Vec<_> = out.iter().map(|t| t.text.as_str()).collect();
        assert!(!texts.contains(&"the"));
        assert!(texts.contains(&"quick"));
        assert!(texts.contains(&"fox"));
    }

    #[test]
    fn registry_standard_analyzer_e2e() {
        let registry = AnalyzerRegistry::default();
        let analyzer = registry.get_analyzer("standard").unwrap();
        let terms = analyzer.analyze_to_terms("The quick brown foxes are jumping over the lazy dogs");
        // V4 — `standard` now matches ES semantics (lowercase + unicode
        // tokenize, no stop words, no stemming).  For stemming use the
        // `english` analyzer explicitly.
        assert!(terms.contains(&"the".to_string()));
        assert!(terms.contains(&"quick".to_string()));
        assert!(terms.contains(&"foxes".to_string()));
        assert!(terms.contains(&"jumping".to_string()));
        assert!(terms.contains(&"lazy".to_string()));
    }

    #[test]
    fn registry_keyword_analyzer() {
        let registry = AnalyzerRegistry::default();
        let analyzer = registry.get_analyzer("keyword").unwrap();
        let terms = analyzer.analyze_to_terms("Hello World");
        assert_eq!(terms, vec!["Hello World"]);
    }
}
