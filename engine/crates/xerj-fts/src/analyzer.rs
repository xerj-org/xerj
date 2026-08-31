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
use tracing;
use unicode_segmentation::UnicodeSegmentation;

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
        self.analyze(input).into_iter().map(|t| t.text).collect()
    }
}

// ── Built-in tokenizers ───────────────────────────────────────────────────────

/// Splits text on Unicode word boundaries (UAX #29), emitting word-class tokens only.
/// Drops punctuation and whitespace segments, matching Lucene's `StandardTokenizer`.
pub struct StandardTokenizer;

impl Tokenizer for StandardTokenizer {
    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut tokens = Vec::new();

        for (position, word) in input.unicode_words().enumerate() {
            // Find the byte offset of this word in the original string.
            // SAFETY: `unicode_words()` returns sub-slices of `input`.
            let start = word.as_ptr() as usize - input.as_ptr() as usize;
            let end = start + word.len();

            tokens.push(Token::new(word, position as u32, start as u32, end as u32));
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
                let byte_end: usize = byte_start
                    + chars[start_char..end_char]
                        .iter()
                        .map(|c| c.len_utf8())
                        .sum::<usize>();
                tokens.push(Token::new(
                    text,
                    position,
                    byte_start as u32,
                    byte_end as u32,
                ));
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

        for (position, gram_size) in (self.min_gram..=self.max_gram).enumerate() {
            if gram_size > chars.len() {
                break;
            }
            let text: String = chars[..gram_size].iter().collect();
            let byte_end: usize = chars[..gram_size].iter().map(|c| c.len_utf8()).sum();
            tokens.push(Token::new(text, position as u32, 0, byte_end as u32));
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

        for (position, mat) in self
            .pattern
            .split(input)
            .filter(|s| !s.is_empty())
            .enumerate()
        {
            // Compute byte offsets by finding the substring in the original input.
            let start = mat.as_ptr() as usize - input.as_ptr() as usize;
            let end = start + mat.len();
            tokens.push(Token::new(mat, position as u32, start as u32, end as u32));
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
                let outputs: Vec<String> =
                    rhs.split(',').map(|s| s.trim().to_lowercase()).collect();
                for input in inputs {
                    map.entry(input)
                        .or_default()
                        .extend(outputs.iter().cloned());
                }
            } else {
                // Equivalence: all terms expand to the full set.
                let terms: Vec<String> = rule.split(',').map(|s| s.trim().to_lowercase()).collect();
                for term in &terms {
                    let others: Vec<String> =
                        terms.iter().filter(|t| *t != term).cloned().collect();
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

/// Identifier-aware sub-word splitter for source-code fields, modelled on
/// Elasticsearch/Lucene's `word_delimiter` filter.
///
/// For every input token it re-emits the **whole token unchanged** (so exact
/// identifier and phrase queries still hit) and additionally emits sub-word
/// tokens at the **same position** (mirroring [`SynonymFilter`]) split on:
///  - non-alphanumeric delimiters (`_`, `-`, `.`, …),
///  - lowerUpper camelCase transitions (`fooBar` → `foo`, `Bar`),
///  - upper-run acronym boundaries (`getHTTPResponse` → `get`, `HTTP`,
///    `Response`),
///  - letter/digit boundaries (`utf8` → `utf`, `8`).
///
/// It also keeps the alphanumeric **run** intact as a sub-word (the "catenate"
/// form), so a query for `utf8` matches even though the run is further split
/// into `utf` + `8`. Sub-words identical to the whole token are dropped to
/// avoid duplicate postings, and duplicates within one token are collapsed.
///
/// Case is intentionally preserved here — run this **before** [`LowercaseFilter`]
/// so camelCase boundaries survive; the lowercase fold then normalises every
/// emitted sub-word for matching.
pub struct WordDelimiterFilter;

impl WordDelimiterFilter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WordDelimiterFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Split a single alphanumeric run on camelCase, acronym, and letter/digit
/// boundaries. The run must already be free of delimiter characters.
fn split_run(run: &str) -> Vec<String> {
    let chars: Vec<char> = run.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let lower_to_upper = prev.is_lowercase() && c.is_uppercase();
            let letter_to_digit = prev.is_alphabetic() && c.is_ascii_digit();
            let digit_to_letter = prev.is_ascii_digit() && c.is_alphabetic();
            // Acronym boundary: UPPER followed by UPPER-then-lower marks the
            // start of a new word (`HTTPResponse` → `HTTP` | `Response`).
            let acronym_tail = prev.is_uppercase()
                && c.is_uppercase()
                && i + 1 < chars.len()
                && chars[i + 1].is_lowercase();
            if lower_to_upper || letter_to_digit || digit_to_letter || acronym_tail {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Produce the ordered, de-duplicated set of sub-words for one token.
/// Does not include the whole token itself.
fn word_delimiter_subwords(token: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for run in token.split(|c: char| !c.is_alphanumeric()) {
        if run.is_empty() {
            continue;
        }
        // Keep the whole alphanumeric run (catenate form) …
        if run != token && seen.insert(run.to_string()) {
            out.push(run.to_string());
        }
        // … and its camelCase / digit sub-words.
        let parts = split_run(run);
        if parts.len() > 1 {
            for w in parts {
                if w != token && seen.insert(w.clone()) {
                    out.push(w);
                }
            }
        }
    }
    out
}

impl TokenFilter for WordDelimiterFilter {
    fn filter(&self, tokens: Vec<Token>) -> Vec<Token> {
        let mut result = Vec::with_capacity(tokens.len());
        for token in tokens {
            for sub in word_delimiter_subwords(&token.text) {
                result.push(Token::new(
                    sub,
                    token.position,
                    token.start_offset,
                    token.end_offset,
                ));
            }
            // Emit the whole token last so it wins de-dup ties downstream,
            // but position is shared so ordering does not affect matching.
            result.push(token);
        }
        result
    }
}

/// Converts Unicode characters to their ASCII equivalents.
///
/// Folds the Latin-1 Supplement diacritics *and* the full Latin Extended-A
/// block (à→a, é→e, ñ→n, ł→l, č→c, ő→o, ĳ→ij, …), and drops standalone
/// combining diacritical marks (U+0300–U+036F) so that decomposed / NFD input
/// folds too (`"e" + U+0301` → `"e"`).  Characters with no ASCII mapping are
/// kept unchanged so that non-Latin scripts are preserved rather than dropped.
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

/// Fold a string to ASCII: strips combining marks, maps Latin-1 Supplement and
/// Latin Extended-A letters to their ASCII base(s), and passes everything else
/// through unchanged.
fn fold_to_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_ascii_fold(c, &mut out);
    }
    out
}

/// Push the ASCII equivalent(s) of `c` into `buf`.
///
/// Case is normalised to lowercase (this pipeline's convention — a
/// `LowercaseFilter` typically runs first, but the fold is correct standalone
/// too).  Characters outside the covered ranges are passed through unchanged.
fn push_ascii_fold(c: char, buf: &mut String) {
    let cp = c as u32;

    // Combining diacritical marks (U+0300–U+036F): emit nothing, so decomposed
    // / NFD input such as `"e" + U+0301` folds down to `"e"`.
    if (0x0300..=0x036F).contains(&cp) {
        return;
    }

    // Latin Extended-A (U+0100–U+017F).  The block is laid out alphabetically
    // by base letter, so each contiguous sub-range folds to a single ASCII
    // base (both upper- and lower-case forms → lowercase base).
    if (0x0100..=0x017F).contains(&cp) {
        match cp {
            0x0100..=0x0105 => buf.push('a'),      // Ā ā Ă ă Ą ą
            0x0106..=0x010D => buf.push('c'),      // Ć ć Ĉ ĉ Ċ ċ Č č
            0x010E..=0x0111 => buf.push('d'),      // Ď ď Đ đ
            0x0112..=0x011B => buf.push('e'),      // Ē ē Ĕ ĕ Ė ė Ę ę Ě ě
            0x011C..=0x0123 => buf.push('g'),      // Ĝ ĝ Ğ ğ Ġ ġ Ģ ģ
            0x0124..=0x0127 => buf.push('h'),      // Ĥ ĥ Ħ ħ
            0x0128..=0x0131 => buf.push('i'),      // Ĩ ĩ Ī ī Ĭ ĭ Į į İ ı
            0x0132..=0x0133 => buf.push_str("ij"), // Ĳ ĳ
            0x0134..=0x0135 => buf.push('j'),      // Ĵ ĵ
            0x0136..=0x0138 => buf.push('k'),      // Ķ ķ ĸ
            0x0139..=0x0142 => buf.push('l'),      // Ĺ ĺ Ļ ļ Ľ ľ Ŀ ŀ Ł ł
            0x0143..=0x014B => buf.push('n'),      // Ń ń Ņ ņ Ň ň ŉ Ŋ ŋ
            0x014C..=0x0151 => buf.push('o'),      // Ō ō Ŏ ŏ Ő ő
            0x0152..=0x0153 => buf.push_str("oe"), // Œ œ
            0x0154..=0x0159 => buf.push('r'),      // Ŕ ŕ Ŗ ŗ Ř ř
            0x015A..=0x0161 => buf.push('s'),      // Ś ś Ŝ ŝ Ş ş Š š
            0x0162..=0x0167 => buf.push('t'),      // Ţ ţ Ť ť Ŧ ŧ
            0x0168..=0x0173 => buf.push('u'),      // Ũ ũ Ū ū Ŭ ŭ Ů ů Ű ű Ų ų
            0x0174..=0x0175 => buf.push('w'),      // Ŵ ŵ
            0x0176..=0x0178 => buf.push('y'),      // Ŷ ŷ Ÿ
            0x0179..=0x017E => buf.push('z'),      // Ź ź Ż ż Ž ž
            0x017F => buf.push('s'),               // ſ (long s)
            _ => buf.push(c),
        }
        return;
    }

    // Latin-1 Supplement diacritics and ligatures.
    match c {
        // A
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => {
            buf.push('a')
        }
        // AE
        'Æ' | 'æ' => buf.push_str("ae"),
        // C
        'Ç' | 'ç' => buf.push('c'),
        // D (eth)
        'Ð' | 'ð' => buf.push('d'),
        // E
        'È' | 'É' | 'Ê' | 'Ë' | 'è' | 'é' | 'ê' | 'ë' => buf.push('e'),
        // I
        'Ì' | 'Í' | 'Î' | 'Ï' | 'ì' | 'í' | 'î' | 'ï' => buf.push('i'),
        // N
        'Ñ' | 'ñ' => buf.push('n'),
        // O
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => {
            buf.push('o')
        }
        // SS
        'ß' => buf.push_str("ss"),
        // TH (thorn)
        'Þ' | 'þ' => buf.push_str("th"),
        // U
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'ù' | 'ú' | 'û' | 'ü' => buf.push('u'),
        // Y
        'Ý' | 'ÿ' | 'ý' => buf.push('y'),
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

/// Thai tokenizer — **Thai-run isolation only, NOT dictionary word segmentation.**
///
/// This tokenizer performs *script-run isolation*, not linguistic word breaking:
/// - Each maximal contiguous run of Thai-script characters is emitted as **one
///   token**, verbatim (Thai has no case, so no lowercasing is applied).
/// - Non-Thai runs are split on whitespace and lowercased, like the other
///   Latin-oriented tokenizers.
///
/// Example: `"สวัสดีabc def"` → `["สวัสดี", "abc", "def"]`.
///
/// Thai is written without spaces between words, so an entire Thai phrase or
/// sentence collapses into a single token here. This is a deliberate,
/// recall-limited simplification: Elasticsearch's `thai` analyzer uses an ICU
/// `BreakIterator` (dictionary-based word segmentation) that would split that
/// same run into multiple word tokens. Queries relying on matching individual
/// Thai words within a run will therefore under-recall against XERJ compared
/// with ES. Full dictionary word segmentation is out of scope for this tokenizer.
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
                let run_end_byte = if i < chars.len() {
                    chars[i].0
                } else {
                    input.len()
                };
                if let Some(word) = input.get(run_start_byte..run_end_byte) {
                    if !word.is_empty() {
                        tokens.push(Token::new(
                            word,
                            position,
                            run_start_byte as u32,
                            run_end_byte as u32,
                        ));
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
                t.text = nfkc_normalize(&t.text);
                t
            })
            .collect()
    }
}

/// Apply full Unicode NFKC normalization (compatibility decomposition +
/// canonical composition) via the `unicode-normalization` crate.
///
/// This is real NFKC, matching the Unicode standard the ICU folding token
/// filter in Elasticsearch relies on — not a hand-picked table.  Examples:
/// - Composes combining sequences: `e` + U+0301 → `é`.
/// - Folds compatibility characters: `ﬁ` → `fi`, `²` → `2`, `Ⅸ` → `IX`.
/// - Maps fullwidth forms to ASCII: `！` → `!`.
///
/// Casing is intentionally left to the pipeline's `LowercaseFilter`; this
/// filter is pure NFKC so it can be composed independently.
fn nfkc_normalize(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfkc().collect()
}

// ── Analyzer registry ─────────────────────────────────────────────────────────

/// `analysis.filter.*.type` values [`AnalyzerRegistry::apply_settings`] can
/// actually build. Must stay in step with the `match filter_type` arms there —
/// `supported_analysis_types_all_build` pins that.
pub const SUPPORTED_FILTER_TYPES: &[&str] = &["synonym", "length", "shingle", "asciifolding"];

/// `analysis.tokenizer.*.type` values [`AnalyzerRegistry::apply_settings`] can
/// actually build. Must stay in step with the `match tok_type` arms there.
pub const SUPPORTED_TOKENIZER_TYPES: &[&str] = &["ngram", "edge_ngram", "pattern"];

/// Which spellings of the index `analysis` block a registry build honours.
///
/// Elasticsearch accepts both the shorthand (`settings.analysis.*`) and the
/// canonical namespaced form (`settings.index.analysis.*`). Only the shorthand
/// used to be read here, so an index created with the canonical shape was
/// written with `standard` for every field that named a custom analyzer
/// (issue #204).
///
/// Fixing that at index CREATE is a fix. Applying it on the index OPEN path
/// would be a silent data bug: an index created before the fix, with the
/// canonical shape, has postings on disk that were produced by `standard`.
/// Reopening it with the declared analyzers live makes query-time analysis stop
/// matching them — no error, no log, just results that quietly disappear. So
/// the binding is a property of the index, decided when it is created, exactly
/// as Lucene keys back-compat off the version an index was created with rather
/// than the running one (`LiveIndexWriterConfig.getIndexCreatedVersionMajor`,
/// lucene/core/src/java/org/apache/lucene/index/LiveIndexWriterConfig.java:290-298).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisBinding {
    /// `settings.analysis` and `settings.index.analysis`. What a NEW index
    /// gets, and what the create-time gate validates against.
    Canonical,
    /// `settings.analysis` only — what every build before the #204 sweep did,
    /// and therefore what an index created by one of them was written with.
    LegacyShorthandOnly,
}

/// Central registry that maps analyzer names to their pipelines.
///
/// Built-in analyzers are registered by default; custom analyzers can be
/// added at index-creation time.
pub struct AnalyzerRegistry {
    analyzers: std::collections::HashMap<String, Arc<AnalyzerPipeline>>,
}

/// The built-in analyzer pipelines, constructed once for the whole process
/// (#873).
///
/// Every open index owns an `AnalyzerRegistry`, and building the built-ins per
/// registry meant every index got its own copy of the English stop-word set,
/// the Snowball stemmers, the e-commerce synonym table and the rest —
/// **92.7 KB of heap per registry, measured** (500 registries, RSS delta,
/// x86-64 release), duplicated across every index on the node for data that is
/// immutable and identical in all of them. That was the second-largest term in
/// the #873 idle-RSS floor after the per-index concurrent maps.
///
/// The pipelines are pure immutable trait objects behind `Arc`s (no interior
/// mutability anywhere in this module), and `register` replaces a *map entry*
/// rather than mutating a pipeline, so sharing them across registries is
/// invisible: a registry that overrides `standard` from its index settings
/// still gets its own entry, and no other index sees it.
fn builtin_analyzers() -> &'static HashMap<String, Arc<AnalyzerPipeline>> {
    static BUILTINS: std::sync::OnceLock<HashMap<String, Arc<AnalyzerPipeline>>> =
        std::sync::OnceLock::new();
    BUILTINS.get_or_init(|| {
        let mut registry = AnalyzerRegistry {
            analyzers: HashMap::new(),
        };
        registry.register_defaults();
        registry.analyzers
    })
}

impl AnalyzerRegistry {
    /// Creates a registry pre-populated with all built-in analyzers.
    ///
    /// The built-ins themselves are process-wide (see [`builtin_analyzers`]);
    /// what this allocates per registry is the name → `Arc` table, so a
    /// registry that adds nothing costs a few hundred bytes rather than the
    /// ~92 KB of stop-word sets, stemmers and synonym tables it used to
    /// rebuild (#873).
    pub fn with_defaults() -> Self {
        Self {
            analyzers: builtin_analyzers().clone(),
        }
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

        // "code" — identifier-aware analyzer for source-code fields.
        //
        // Splits snake_case / camelCase / letter-digit identifiers into their
        // constituent sub-words while preserving the whole identifier, so a
        // behavioural query like `field norm quantization` can match an
        // identifier such as `id_to_fieldnorm` or `fieldNormQuant`.  The split
        // runs BEFORE lowercasing so camelCase case boundaries survive.
        self.register(
            "code",
            AnalyzerPipeline::new(
                vec![],
                Arc::new(StandardTokenizer),
                vec![
                    Arc::new(WordDelimiterFilter::new()) as Arc<dyn TokenFilter>,
                    Arc::new(LowercaseFilter) as Arc<dyn TokenFilter>,
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
        self.get_analyzer("standard")
            .expect("standard analyzer always registered")
    }

    /// Extend this registry with custom analyzer definitions parsed from an
    /// index `settings.analysis` block.
    ///
    /// **Total by design.** Every construct this method cannot honour is
    /// skipped or replaced rather than raised, because it also runs on the
    /// index-open path where a hard failure would make an existing index
    /// unopenable. Callers on the *create* path must gate on
    /// [`AnalyzerRegistry::unsupported_analysis`] first — see issue #204.
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
        self.apply_settings_with_binding(settings, AnalysisBinding::Canonical);
    }

    /// [`Self::apply_settings`], with the set of accepted `analysis` spellings
    /// chosen by the caller. See [`AnalysisBinding`].
    pub fn apply_settings_with_binding(
        &mut self,
        settings: &serde_json::Value,
        binding: AnalysisBinding,
    ) {
        let analysis = match Self::analysis_block_with_binding(settings, binding) {
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
                            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                            .unwrap_or_default();
                        let f = SynonymFilter::new(&rules);
                        custom_filters.insert(filter_name.clone(), Arc::new(f));
                    }
                    "length" => {
                        let min =
                            filter_def.get("min").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                        let max = filter_def
                            .get("max")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(256) as usize;
                        custom_filters
                            .insert(filter_name.clone(), Arc::new(LengthFilter::new(min, max)));
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
                let tok_type = tok_def.get("type").and_then(|v| v.as_str()).unwrap_or("");

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
                        custom_tokenizers
                            .insert(tok_name.clone(), Arc::new(NGramTokenizer::new(min, max)));
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
                        custom_tokenizers.insert(
                            tok_name.clone(),
                            Arc::new(EdgeNGramTokenizer::new(min, max)),
                        );
                    }
                    "pattern" => {
                        let pattern = tok_def
                            .get("pattern")
                            .and_then(|v| v.as_str())
                            .unwrap_or(r"\W+");
                        match PatternTokenizer::new(pattern) {
                            Ok(t) => {
                                custom_tokenizers.insert(tok_name.clone(), Arc::new(t));
                            }
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
                    match self.get_analyzer(analyzer_type) {
                        Some(builtin) => {
                            self.analyzers.insert(analyzer_name.clone(), builtin);
                        }
                        None => tracing::warn!(
                            analyzer = analyzer_name.as_str(),
                            analyzer_type,
                            "unknown analyzer type — analyzer not registered, \
                             fields referencing it fall back to `standard`"
                        ),
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
                    .unwrap_or_else(|| {
                        // Not equivalent: substituting `standard` for (say)
                        // `edge_ngram` builds a completely different index.
                        // `unsupported_analysis` rejects this at index-create
                        // time; reaching it here means we are re-opening an
                        // index whose settings.json predates that check, so
                        // say so loudly rather than degrade in silence
                        // (issue #204).
                        tracing::warn!(
                            analyzer = analyzer_name.as_str(),
                            tokenizer = tokenizer_name,
                            "unknown tokenizer in custom analyzer — falling back to \
                             `standard`; this index does NOT tokenize as configured"
                        );
                        Arc::new(StandardTokenizer)
                    });

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

    /// Report every `settings.analysis` construct that [`apply_settings`] would
    /// accept and then silently NOT honour.
    ///
    /// [`apply_settings`]: AnalyzerRegistry::apply_settings
    ///
    /// Issue #204 — "degrade loudly or not at all". `apply_settings` is
    /// deliberately total: it never fails, because it also runs on the
    /// index-*open* path where refusing to build a registry would brick an
    /// existing index. That leniency is right at open time and wrong at create
    /// time, where every one of these constructs used to be accepted with a
    /// `200 {"acknowledged": true}` and then quietly replaced by something
    /// weaker.
    ///
    /// # What is checked
    ///
    /// 1. a declared `filter` / `tokenizer` whose `type` this build cannot
    ///    construct **and** whose *name* does not resolve to an equivalent
    ///    built-in (see [`Self::builtin_filter_honours`] — the by-name fallback
    ///    `apply_settings` actually takes);
    /// 2. a `pattern` tokenizer whose regex does not compile;
    /// 3. an analyzer naming a `tokenizer` / `filter` that is neither declared
    ///    nor built in — the unresolvable tokenizer used to become `standard`
    ///    (an `edge_ngram` autocomplete index that matches nothing) and the
    ///    unresolvable filter used to be dropped (a missing `lowercase` turns
    ///    every match case-sensitive);
    /// 4. an unknown non-custom analyzer `type`, which left the analyzer
    ///    unregistered so fields naming it silently got `standard`;
    /// 5. `char_filter` — declared at either level and never built by
    ///    `apply_settings`, so the stripping/mapping never happens;
    /// 6. `normalizer` — accepted by ES for `keyword` fields, never built here;
    /// 7. an analyzer whose `filter` is not an array or whose `tokenizer` is
    ///    not a string: `apply_settings` drops the value shape-first and falls
    ///    back to "no filters" / `standard`;
    /// 8. a `synonym` filter whose `synonyms` is not an array of strings —
    ///    dropped the same shape-first way, leaving a filter that expands
    ///    nothing.
    ///
    /// # What is NOT checked
    ///
    /// Option-level divergence *inside* a type this build does construct — e.g.
    /// `synonym.synonyms_path` (file-backed rule lists are not read),
    /// `length.min` (defaults to 2 here, 0 in ES), `asciifolding
    /// .preserve_original`. Those are pre-existing gaps, tracked separately;
    /// this function does not claim to cover them, and the by-name fallback in
    /// (1) is deliberately conservative about them: a name that resolves to a
    /// built-in is only accepted when the declared options ask for exactly what
    /// that built-in does.
    ///
    /// Returns one human-readable message per problem, empty when the block can
    /// be honoured as written. `Index::create_with_settings` turns a non-empty
    /// result into a 400 so the caller learns at the door.
    ///
    /// `settings` may be either the full envelope (`{"settings": {"analysis":
    /// …}}`) or the inner settings object — the same two shapes
    /// `build_registry_from_settings` accepts.
    pub fn unsupported_analysis(settings: &serde_json::Value) -> Vec<String> {
        let root = settings.pointer("/settings").unwrap_or(settings);
        let mut problems = Vec::new();

        // The DOTTED spelling first, because it is not a variant of the block
        // below — it is a block this build never reads at all.
        // `analysis_block` resolves two JSON pointers, so
        // `{"index.analysis.filter.my_lower.type": "lowercase"}` finds nothing:
        // the gate passes it, `apply_settings` builds no filter from it, and
        // `GET /{index}/_settings` echoes back an analyzer that analyses
        // nothing. Byte-for-byte the same request as the nested form this gate
        // 400s, one spelling apart — and `PUT /{index}/_settings` already
        // refuses all four spellings, so accepting it here made the two gates
        // disagree about the same string (issue #204).
        //
        // Reported rather than honoured: teaching the registry to expand
        // dotted keys would change what an EXISTING index analyses the moment
        // it reopens, which is the upgrade break `analysis-binding.json`
        // exists to prevent. Refusing at create time changes nothing already
        // on disk.
        for key in Self::dotted_analysis_keys(root) {
            problems.push(format!(
                "setting [{key}]: the flat dotted spelling of an `analysis` declaration is \
                 not read by this build — the analyzer registry resolves `analysis.*` and \
                 `index.analysis.*` as nested objects only, so this declaration would be \
                 stored, echoed back by `GET /_settings`, and applied to nothing. Write it \
                 as a nested object instead"
            ));
        }

        let Some(analysis) = Self::analysis_block(root) else {
            return problems;
        };
        let probe = Self::with_defaults();

        // Declared names are collected even when their `type` is unsupported,
        // so a bad type is reported once (as a type problem) rather than twice
        // (again as a dangling reference from every analyzer that uses it).
        let mut declared_filters: HashSet<&str> = HashSet::new();
        if let Some(filter_map) = analysis.pointer("/filter").and_then(|v| v.as_object()) {
            for (filter_name, filter_def) in filter_map {
                declared_filters.insert(filter_name.as_str());
                let filter_type = filter_def
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if SUPPORTED_FILTER_TYPES.contains(&filter_type) {
                    // A supported TYPE can still be given a wrong-shaped
                    // option, and `apply_settings` drops those shape-first:
                    // `synonyms` is read with `and_then(as_array)` and its
                    // entries with `filter_map(as_str)`, so
                    // `"synonyms": "fast,quick"` builds a synonym filter with
                    // ZERO rules — registered, referenced, expanding nothing,
                    // and reported nowhere. Same silent-loss class as the
                    // analyzer-level `filter`/`tokenizer` shapes below.
                    if filter_type == "synonym" {
                        match filter_def.get("synonyms") {
                            None => {}
                            Some(serde_json::Value::Array(arr)) => {
                                for v in arr {
                                    if !v.is_string() {
                                        problems.push(format!(
                                            "token filter [{filter_name}]: `synonyms` entries \
                                             must be rule strings, got {v} — it would be dropped"
                                        ));
                                    }
                                }
                            }
                            Some(other) => problems.push(format!(
                                "token filter [{filter_name}]: `synonyms` must be an array of \
                                 rule strings, got {other} — it would be ignored and the filter \
                                 would expand nothing"
                            )),
                        }
                    }
                    continue;
                }
                // The `type` is one `apply_settings` cannot BUILD — but that is
                // not the whole story, and reading only the type is what made
                // this check reject settings blocks xerj serves correctly.
                // `apply_settings` looks the name up in `custom_filters` first
                // and falls through to `resolve_builtin_filter(name)`
                // (analyzer.rs:1284-1286), so `{"english_stop": {"type":
                // "stop", "stopwords": "_english_"}}` — the canonical
                // Elasticsearch-docs `rebuilt_english` block — really does
                // strip English stopwords. Complaining about its type 400s a
                // `PUT /{index}` that used to work AND analysed as asked.
                if Self::builtin_filter_honours(filter_name, filter_def) {
                    continue;
                }
                if probe.resolve_builtin_filter(filter_name).is_some() {
                    // The name resolves, but to something that is NOT what the
                    // declaration asks for (a custom `stopwords` list, another
                    // `language`). Saying "unsupported type" here would be
                    // misleading — the type is fine, the options are not.
                    problems.push(format!(
                        "token filter [{filter_name}]: [{filter_type}] with these options is \
                         not supported — the built-in [{filter_name}] filter would be used \
                         instead, which is not what this declares"
                    ));
                    continue;
                }
                problems.push(format!(
                    "token filter [{filter_name}]: unsupported type [{filter_type}] \
                     (supported: {})",
                    SUPPORTED_FILTER_TYPES.join(", ")
                ));
            }
        }

        let mut declared_tokenizers: HashSet<&str> = HashSet::new();
        if let Some(tok_map) = analysis.pointer("/tokenizer").and_then(|v| v.as_object()) {
            for (tok_name, tok_def) in tok_map {
                declared_tokenizers.insert(tok_name.as_str());
                let tok_type = tok_def.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if !SUPPORTED_TOKENIZER_TYPES.contains(&tok_type) {
                    // Same by-name fallback as filters: `apply_settings` reaches
                    // `resolve_builtin_tokenizer(name)` (analyzer.rs:1454-1473)
                    // when the declared type built nothing.
                    if Self::builtin_tokenizer_honours(tok_name, tok_def) {
                        continue;
                    }
                    if probe.resolve_builtin_tokenizer(tok_name).is_some() {
                        problems.push(format!(
                            "tokenizer [{tok_name}]: [{tok_type}] with these options is not \
                             supported — the built-in [{tok_name}] tokenizer would be used \
                             instead, which is not what this declares"
                        ));
                        continue;
                    }
                    problems.push(format!(
                        "tokenizer [{tok_name}]: unsupported type [{tok_type}] \
                         (supported: {})",
                        SUPPORTED_TOKENIZER_TYPES.join(", ")
                    ));
                    continue;
                }
                if tok_type == "pattern" {
                    let pattern = tok_def
                        .get("pattern")
                        .and_then(|v| v.as_str())
                        .unwrap_or(r"\W+");
                    if PatternTokenizer::new(pattern).is_err() {
                        problems.push(format!(
                            "tokenizer [{tok_name}]: invalid pattern regex [{pattern}]"
                        ));
                    }
                }
            }
        }

        // `char_filter` never reaches the built pipeline: every analyzer is
        // registered as `AnalyzerPipeline::new(vec![], tokenizer, filters)` —
        // the char-filter slot is hard-coded empty. A declared `html_strip` or
        // `mapping` therefore strips/maps nothing, with no signal anywhere.
        if analysis
            .pointer("/char_filter")
            .and_then(|v| v.as_object())
            .is_some_and(|m| !m.is_empty())
        {
            problems.push(
                "char filters are not supported (analysis.char_filter is declared but no \
                 char filter is ever applied)"
                    .to_string(),
            );
        }
        // `normalizer` is the keyword-field analogue and is equally unbuilt.
        if analysis
            .pointer("/normalizer")
            .and_then(|v| v.as_object())
            .is_some_and(|m| !m.is_empty())
        {
            problems.push(
                "normalizers are not supported (analysis.normalizer is declared but never \
                 built, so keyword fields naming one are indexed unchanged)"
                    .to_string(),
            );
        }

        if let Some(analyzer_map) = analysis.pointer("/analyzer").and_then(|v| v.as_object()) {
            for (analyzer_name, analyzer_def) in analyzer_map {
                let analyzer_type = analyzer_def
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("custom");

                if analyzer_type != "custom" {
                    if probe.get_analyzer(analyzer_type).is_none() {
                        problems.push(format!(
                            "analyzer [{analyzer_name}]: unknown analyzer type [{analyzer_type}]"
                        ));
                    }
                    continue;
                }

                // Per-analyzer char filters: same hard-coded-empty slot.
                if analyzer_def
                    .get("char_filter")
                    .is_some_and(|v| !matches!(v, serde_json::Value::Array(a) if a.is_empty()))
                {
                    problems.push(format!(
                        "analyzer [{analyzer_name}]: `char_filter` is not supported — the \
                         declared char filters are never applied"
                    ));
                }

                // Shape errors are silent losses, not type errors:
                // `apply_settings` reads `tokenizer` with `as_str()` and
                // `filter` with `as_array()`, so a wrong-shaped value is
                // dropped and the analyzer is built with `standard` / no
                // filters — accepted, and not what was written.
                match analyzer_def.get("tokenizer") {
                    None | Some(serde_json::Value::String(_)) => {}
                    Some(other) => problems.push(format!(
                        "analyzer [{analyzer_name}]: `tokenizer` must be a tokenizer name \
                         (string), got {other} — it would be ignored and `standard` used"
                    )),
                }
                let mut filter_names: Vec<&str> = Vec::new();
                match analyzer_def.get("filter") {
                    None => {}
                    Some(serde_json::Value::Array(arr)) => {
                        for v in arr {
                            match v.as_str() {
                                Some(s) => filter_names.push(s),
                                None => problems.push(format!(
                                    "analyzer [{analyzer_name}]: `filter` entries must be \
                                     filter names (strings), got {v} — it would be ignored"
                                )),
                            }
                        }
                    }
                    Some(other) => problems.push(format!(
                        "analyzer [{analyzer_name}]: `filter` must be an array of filter \
                         names, got {other} — it would be ignored and no filters applied"
                    )),
                }

                let tokenizer_name = analyzer_def
                    .get("tokenizer")
                    .and_then(|v| v.as_str())
                    .unwrap_or("standard");
                if !declared_tokenizers.contains(tokenizer_name)
                    && probe.resolve_builtin_tokenizer(tokenizer_name).is_none()
                {
                    problems.push(format!(
                        "analyzer [{analyzer_name}]: unknown tokenizer [{tokenizer_name}]"
                    ));
                }

                for fname in filter_names {
                    if !declared_filters.contains(fname)
                        && probe.resolve_builtin_filter(fname).is_none()
                    {
                        problems.push(format!(
                            "analyzer [{analyzer_name}]: unknown token filter [{fname}]"
                        ));
                    }
                }
            }
        }

        problems.sort();
        problems.dedup();
        problems
    }

    /// Does a declared token filter resolve, by NAME, to a built-in that does
    /// exactly what the declaration asks for?
    ///
    /// This is the fallback `apply_settings` genuinely takes: a `type` it
    /// cannot build leaves `custom_filters` without an entry, and the analyzer
    /// loop then calls [`Self::resolve_builtin_filter`] with the *declared
    /// name*. Judging such a declaration on its `type` alone reports a filter
    /// that is in fact honoured — and, through the create-time gate, 400s a
    /// `PUT /{index}` that previously worked.
    ///
    /// Deliberately narrow: only the exact option sets the built-in reproduces
    /// return `true`. A custom `stopwords` list under the name `english_stop`
    /// is still a problem, because the English list is what would actually run.
    fn builtin_filter_honours(name: &str, def: &serde_json::Value) -> bool {
        let ty = def.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match (name, ty) {
            // `LowercaseFilter`. ES's `lowercase` filter takes an optional
            // `language` (greek/irish/turkish); only the default is this.
            ("lowercase", "lowercase") => def.get("language").is_none(),
            // `StopwordsFilter::english()`. ES's `stop` defaults to
            // `_english_`, which is exactly the list built here.
            ("stop" | "english_stop", "stop") => {
                def.get("stopwords_path").is_none()
                    && match def.get("stopwords") {
                        None => true,
                        Some(serde_json::Value::String(s)) => s == "_english_",
                        _ => false,
                    }
                    && def.get("ignore_case").is_none()
                    && def.get("remove_trailing").is_none()
            }
            // `StemmerFilter::english()` — rust-stemmers' Snowball English.
            ("stemmer" | "english_stemmer", "stemmer") => {
                match def.get("language").or_else(|| def.get("name")) {
                    None => true,
                    Some(serde_json::Value::String(s)) => {
                        matches!(s.as_str(), "english" | "porter2")
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Tokenizer counterpart of [`Self::builtin_filter_honours`].
    ///
    /// `apply_settings` resolves an unbuilt tokenizer by declared name through
    /// [`Self::resolve_builtin_tokenizer`], so `{"standard": {"type":
    /// "standard"}}` is honoured exactly as written.
    fn builtin_tokenizer_honours(name: &str, def: &serde_json::Value) -> bool {
        let ty = def.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match (name, ty) {
            // `max_token_length` is not implemented by `StandardTokenizer`.
            ("standard", "standard") => def.get("max_token_length").is_none(),
            ("whitespace", "whitespace") => def.get("max_token_length").is_none(),
            // `buffer_size` is a performance knob with no semantic effect.
            ("keyword", "keyword") => true,
            _ => false,
        }
    }

    /// Locate the `analysis` block inside an index-settings object.
    ///
    /// ES accepts both the shorthand (`settings.analysis.*`) and the canonical
    /// namespaced form (`settings.index.analysis.*`). Only the shorthand used
    /// to be read here, so a settings body written the canonical way was
    /// accepted, echoed back by `GET /{index}/_settings`, and then contributed
    /// no analyzers at all — every field naming one silently got `standard`
    /// (issue #204). Kept as one helper so the builder and
    /// [`AnalyzerRegistry::unsupported_analysis`] can never disagree about
    /// which block is in force.
    fn analysis_block(settings: &serde_json::Value) -> Option<&serde_json::Value> {
        Self::analysis_block_with_binding(settings, AnalysisBinding::Canonical)
    }

    fn analysis_block_with_binding(
        settings: &serde_json::Value,
        binding: AnalysisBinding,
    ) -> Option<&serde_json::Value> {
        let shorthand = settings.pointer("/analysis");
        match binding {
            AnalysisBinding::Canonical => shorthand.or_else(|| settings.pointer("/index/analysis")),
            AnalysisBinding::LegacyShorthandOnly => shorthand,
        }
    }

    /// Every key in `settings` that declares something under `analysis` using
    /// a DOTTED path rather than a nested object, in the order they appear.
    ///
    /// The three spellings below are one request as far as an Elasticsearch
    /// client is concerned, and xerj's settings handlers accept all of them for
    /// other namespaces (`index.sort.field` is parsed dotted today):
    ///
    /// ```json
    /// {"index.analysis.analyzer.x.type": "custom"}   // fully dotted
    /// {"analysis.analyzer.x.type": "custom"}         // dotted, unnamespaced
    /// {"index": {"analysis.analyzer.x.type": "custom"}}  // half-dotted
    /// ```
    ///
    /// [`Self::analysis_block`] resolves none of them, so on their own they are
    /// accepted-and-ignored. Callers that gate on
    /// [`Self::unsupported_analysis`] get them reported; the registry builder
    /// deliberately still ignores them, so nothing already on disk changes
    /// meaning (issue #204).
    ///
    /// `settings` may be the full envelope or the inner settings object, the
    /// same two shapes [`Self::unsupported_analysis`] accepts.
    pub fn dotted_analysis_keys(settings: &serde_json::Value) -> Vec<String> {
        let root = settings.pointer("/settings").unwrap_or(settings);
        let mut keys = Vec::new();
        if let Some(obj) = root.as_object() {
            for k in obj.keys() {
                // A bare `analysis` key IS the nested form `analysis_block`
                // reads; it is not a dotted spelling and is gated elsewhere.
                // `index.analysis` is dotted even though its value is nested,
                // because the pointer this build resolves is `/index/analysis`
                // — an `index` OBJECT with an `analysis` member, which a key
                // literally named `index.analysis` is not.
                if k == "analysis" {
                    continue;
                }
                let bare = k.strip_prefix("index.").unwrap_or(k);
                if bare == "analysis" || bare.starts_with("analysis.") {
                    keys.push(k.clone());
                }
            }
        }
        if let Some(ix) = root.get("index").and_then(serde_json::Value::as_object) {
            keys.extend(
                ix.keys()
                    .filter(|k| k.starts_with("analysis."))
                    .map(|k| format!("index.{k}")),
            );
        }
        keys
    }

    /// True when `settings` reaches into the `analysis` namespace in ANY
    /// spelling — nested, namespaced, dotted or half-dotted.
    ///
    /// One helper so a caller cannot gate on a subset of the spellings it
    /// accepts. A gate that tests only the nested one is not a gate (#204).
    pub fn declares_analysis(settings: &serde_json::Value) -> bool {
        let root = settings.pointer("/settings").unwrap_or(settings);
        root.pointer("/analysis").is_some()
            || root.pointer("/index/analysis").is_some()
            || !Self::dotted_analysis_keys(root).is_empty()
    }

    /// True when the only `analysis` block in `settings` is the canonical
    /// namespaced one — i.e. this is exactly the settings shape whose meaning
    /// [`AnalysisBinding`] changes.
    ///
    /// Accepts the same two envelopes as [`Self::unsupported_analysis`].
    pub fn declares_namespaced_analysis_only(settings: &serde_json::Value) -> bool {
        let root = settings.pointer("/settings").unwrap_or(settings);
        root.pointer("/analysis").is_none() && root.pointer("/index/analysis").is_some()
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
            "word_delimiter" | "word_delimiter_graph" => {
                Some(Arc::new(WordDelimiterFilter::new()) as Arc<dyn TokenFilter>)
            }
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
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "but",
    "by",
    "for",
    "if",
    "in",
    "into",
    "is",
    "it",
    "no",
    "not",
    "of",
    "on",
    "or",
    "such",
    "that",
    "the",
    "their",
    "then",
    "there",
    "these",
    "they",
    "this",
    "to",
    "was",
    "will",
    "with",
    // Extended Lucene English stop list
    "able",
    "about",
    "above",
    "according",
    "accordingly",
    "across",
    "actually",
    "after",
    "afterwards",
    "again",
    "against",
    "albeit",
    "all",
    "allow",
    "allows",
    "almost",
    "alone",
    "along",
    "already",
    "also",
    "although",
    "always",
    "am",
    "among",
    "amongst",
    "another",
    "any",
    "anybody",
    "anyhow",
    "anyone",
    "anything",
    "anyway",
    "anyways",
    "anywhere",
    "apart",
    "appear",
    "appreciate",
    "appropriate",
    "around",
    "aside",
    "ask",
    "asking",
    "associated",
    "available",
    "away",
    "awfully",
    "became",
    "because",
    "become",
    "becomes",
    "becoming",
    "been",
    "before",
    "beforehand",
    "behind",
    "being",
    "below",
    "beside",
    "besides",
    "best",
    "better",
    "between",
    "beyond",
    "both",
    "brief",
    "came",
    "can",
    "cannot",
    "cant",
    "cause",
    "causes",
    "certain",
    "certainly",
    "changes",
    "clearly",
    "co",
    "com",
    "come",
    "comes",
    "concerning",
    "consequently",
    "consider",
    "considering",
    "contain",
    "containing",
    "contains",
    "corresponding",
    "could",
    "course",
    "currently",
    "definitely",
    "described",
    "despite",
    "did",
    "different",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "eight",
    "either",
    "else",
    "elsewhere",
    "enough",
    "entirely",
    "especially",
    "even",
    "ever",
    "every",
    "everybody",
    "everyone",
    "everything",
    "everywhere",
    "ex",
    "exactly",
    "except",
    "far",
    "few",
    "fifth",
    "first",
    "five",
    "followed",
    "following",
    "follows",
    "former",
    "formerly",
    "forth",
    "four",
    "from",
    "further",
    "furthermore",
    "get",
    "gets",
    "given",
    "go",
    "goes",
    "going",
    "gone",
    "got",
    "had",
    "happens",
    "hardly",
    "has",
    "have",
    "having",
    "he",
    "hence",
    "her",
    "here",
    "hereafter",
    "hereby",
    "herein",
    "hereupon",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "hither",
    "hopefully",
    "how",
    "howbeit",
    "however",
    "i",
    "ie",
    "ignored",
    "immediate",
    "inasmuch",
    "inc",
    "indeed",
    "indicate",
    "indicated",
    "indicates",
    "inner",
    "insofar",
    "instead",
    "its",
    "itself",
    "just",
    "keep",
    "kept",
    "know",
    "known",
    "knows",
    "last",
    "lately",
    "later",
    "latter",
    "latterly",
    "least",
    "less",
    "lest",
    "let",
    "like",
    "liked",
    "likely",
    "little",
    "look",
    "looking",
    "looks",
    "ltd",
    "mainly",
    "many",
    "may",
    "maybe",
    "me",
    "mean",
    "meanwhile",
    "merely",
    "might",
    "more",
    "moreover",
    "most",
    "mostly",
    "much",
    "must",
    "my",
    "myself",
    "name",
    "namely",
    "nd",
    "near",
    "nearly",
    "necessary",
    "need",
    "needs",
    "neither",
    "never",
    "nevertheless",
    "new",
    "next",
    "nine",
    "nobody",
    "none",
    "noone",
    "nor",
    "normally",
    "nothing",
    "novel",
    "now",
    "nowhere",
    "obviously",
    "off",
    "often",
    "oh",
    "ok",
    "okay",
    "old",
    "once",
    "one",
    "ones",
    "only",
    "onto",
    "other",
    "others",
    "otherwise",
    "our",
    "ours",
    "ourselves",
    "out",
    "outside",
    "over",
    "overall",
    "own",
    "particular",
    "particularly",
    "per",
    "perhaps",
    "placed",
    "please",
    "plus",
    "possible",
    "presumably",
    "probably",
    "provides",
    "quite",
    "rather",
    "really",
    "reasonably",
    "regarding",
    "regardless",
    "regards",
    "relatively",
    "respectively",
    "right",
    "said",
    "same",
    "saw",
    "say",
    "saying",
    "says",
    "second",
    "secondly",
    "see",
    "seeing",
    "seem",
    "seemed",
    "seeming",
    "seems",
    "seen",
    "self",
    "selves",
    "sensible",
    "sent",
    "serious",
    "seriously",
    "seven",
    "several",
    "shall",
    "she",
    "should",
    "since",
    "six",
    "so",
    "some",
    "somebody",
    "somehow",
    "someone",
    "something",
    "sometime",
    "sometimes",
    "somewhat",
    "somewhere",
    "soon",
    "sorry",
    "specified",
    "specify",
    "specifying",
    "still",
    "sub",
    "sup",
    "sure",
    "take",
    "taken",
    "tell",
    "tends",
    "th",
    "than",
    "thank",
    "thanks",
    "third",
    "thorough",
    "thoroughly",
    "though",
    "three",
    "through",
    "throughout",
    "thru",
    "thus",
    "together",
    "too",
    "took",
    "toward",
    "towards",
    "tried",
    "tries",
    "truly",
    "try",
    "trying",
    "twice",
    "two",
    "un",
    "under",
    "unfortunately",
    "unless",
    "unlikely",
    "until",
    "unto",
    "upon",
    "us",
    "use",
    "used",
    "useful",
    "uses",
    "using",
    "usually",
    "value",
    "various",
    "very",
    "via",
    "viz",
    "vs",
    "want",
    "wants",
    "we",
    "well",
    "went",
    "were",
    "what",
    "whatever",
    "when",
    "whence",
    "whenever",
    "where",
    "whereafter",
    "whereas",
    "whereby",
    "wherein",
    "whereupon",
    "wherever",
    "whether",
    "which",
    "while",
    "whither",
    "who",
    "whoever",
    "whole",
    "whom",
    "whose",
    "why",
    "within",
    "without",
    "wonder",
    "would",
    "yes",
    "yet",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
    "zero",
];

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// #873 - two registries must SHARE their built-in pipelines rather than
    /// each build its own copy of the stop-word sets, stemmers and synonym
    /// tables. One registry is built per open index, and rebuilding the
    /// built-ins per registry measured 92.7 KB of heap each.
    ///
    /// Pointer identity is the property, not a byte count: it is what makes
    /// the cost O(1) in the number of indices instead of O(n). The negative
    /// arm matters just as much - an index that overrides a built-in from its
    /// own settings must get its own pipeline and must not disturb anyone
    /// else's.
    #[test]
    fn built_in_analyzers_are_shared_between_registries_not_rebuilt() {
        let a = AnalyzerRegistry::with_defaults();
        let b = AnalyzerRegistry::with_defaults();
        assert!(
            !a.analyzers.is_empty(),
            "fixture: the registry must have built-ins to share"
        );
        for (name, pipeline) in &a.analyzers {
            let other = b
                .get_analyzer(name)
                .unwrap_or_else(|| panic!("{name} missing from the second registry"));
            assert!(
                Arc::ptr_eq(pipeline, &other),
                "built-in analyzer `{name}` was rebuilt instead of shared"
            );
        }

        // Overriding one is local to the registry that did it.
        let mut c = AnalyzerRegistry::with_defaults();
        c.register(
            "standard",
            AnalyzerPipeline::new(vec![], Arc::new(WhitespaceTokenizer), vec![]),
        );
        assert!(
            !Arc::ptr_eq(
                &c.get_analyzer("standard").unwrap(),
                &a.get_analyzer("standard").unwrap()
            ),
            "an override must replace this registry's entry"
        );
        assert!(
            Arc::ptr_eq(
                &a.get_analyzer("standard").unwrap(),
                &b.get_analyzer("standard").unwrap()
            ),
            "and must not disturb any other registry"
        );
        // The shared built-in still analyzes exactly as it did.
        assert_eq!(
            a.get_analyzer("standard")
                .unwrap()
                .analyze_to_terms("Hello, World!"),
            vec!["hello".to_string(), "world".to_string()]
        );
    }

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
    fn thai_tokenizer_isolates_runs_not_words() {
        // Documents the honest behaviour: the ThaiTokenizer performs
        // Thai-script-run ISOLATION, not dictionary word segmentation.
        // A contiguous Thai run is emitted verbatim as a single token
        // (no word breaking, no lowercasing — Thai has no case), while
        // non-Thai runs are split on whitespace and lowercased.
        let tok = ThaiTokenizer;
        let tokens = tok.tokenize("สวัสดีabc def");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["สวัสดี", "abc", "def"]);

        // The whole Thai run stays one token — it is NOT segmented into
        // the individual words "สวัสดี" + "ครับ" that ES's ICU BreakIterator
        // would produce. This asserts the recall-limited contract on purpose.
        let joined = tok.tokenize("สวัสดีครับ");
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].text, "สวัสดีครับ");
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
        let terms =
            analyzer.analyze_to_terms("The quick brown foxes are jumping over the lazy dogs");
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

    #[test]
    fn icu_folding_filter_applies_real_nfkc() {
        let filter = IcuFoldingFilter;
        // Combining sequence composes: "e" + U+0301 (COMBINING ACUTE) -> "é".
        // Fullwidth exclamation folds to ASCII "!".
        // Roman numeral nine (U+2168) decomposes to "IX".
        // Ligature "ﬁ" -> "fi"; superscript "²" -> "2".
        let tokens = vec![
            Token::new("e\u{0301}", 0, 0, 0),
            Token::new("\u{FF01}", 1, 0, 0),
            Token::new("\u{2168}", 2, 0, 0),
            Token::new("ﬁ", 3, 0, 0),
            Token::new("²", 4, 0, 0),
        ];
        let out = filter.filter(tokens);
        assert_eq!(out[0].text, "\u{00E9}"); // é (single precomposed codepoint)
        assert_eq!(out[0].text.chars().count(), 1);
        assert_eq!(out[1].text, "!");
        assert_eq!(out[2].text, "IX"); // NFKC keeps case; lowercasing is the pipeline's job
        assert_eq!(out[3].text, "fi");
        assert_eq!(out[4].text, "2");
    }

    #[test]
    fn ascii_folding_covers_latin_extended_a_and_combining_marks() {
        let filter = AsciiFoldingFilter;
        let tokens = vec![
            Token::new("łódź", 0, 0, 0), // Polish: Ł/ł (Ext-A) + ó (Latin-1) + ź (Ext-A)
            Token::new("žluťoučký", 1, 0, 0), // Czech: ž ť č (Ext-A) + ý (Latin-1)
            Token::new("đžem", 2, 0, 0), // Croatian: đ ž (Ext-A)
            Token::new("cafe\u{0301}", 3, 0, 0), // NFD: e + combining acute → e
            Token::new("ĳsselmeer", 4, 0, 0), // ĳ ligature → ij
            Token::new("œuvre", 5, 0, 0), // œ ligature → oe
        ];
        let out = filter.filter(tokens);
        assert_eq!(out[0].text, "lodz");
        assert_eq!(out[1].text, "zlutoucky");
        assert_eq!(out[2].text, "dzem");
        assert_eq!(out[3].text, "cafe");
        assert_eq!(out[4].text, "ijsselmeer");
        assert_eq!(out[5].text, "oeuvre");
    }

    #[test]
    fn registry_icu_folding_analyzer_lowercases_and_nfkc() {
        let registry = AnalyzerRegistry::default();
        let analyzer = registry.get_analyzer("icu_folding").unwrap();
        // Pipeline = StandardTokenizer -> LowercaseFilter -> IcuFoldingFilter,
        // so the analyzer both lowercases and NFKC-folds.
        let terms = analyzer.analyze_to_terms("Ⅸ ﬁ TEST");
        assert!(terms.contains(&"ix".to_string()), "terms={terms:?}");
        assert!(terms.contains(&"fi".to_string()), "terms={terms:?}");
        assert!(terms.contains(&"test".to_string()), "terms={terms:?}");
    }

    #[test]
    fn word_delimiter_splits_snake_camel_and_digits() {
        let filter = WordDelimiterFilter::new();
        let out: Vec<String> = filter
            .filter(vec![Token::new("id_to_fieldnorm", 0, 0, 0)])
            .into_iter()
            .map(|t| t.text)
            .collect();
        // Whole identifier preserved …
        assert!(out.contains(&"id_to_fieldnorm".to_string()), "out={out:?}");
        // … and its snake_case sub-words emitted.
        for w in ["id", "to", "fieldnorm"] {
            assert!(out.contains(&w.to_string()), "missing {w} in {out:?}");
        }
    }

    #[test]
    fn word_delimiter_handles_acronym_runs() {
        let filter = WordDelimiterFilter::new();
        let out: Vec<String> = filter
            .filter(vec![Token::new("getHTTPResponse", 0, 0, 0)])
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert!(out.contains(&"getHTTPResponse".to_string()), "out={out:?}");
        for w in ["get", "HTTP", "Response"] {
            assert!(out.contains(&w.to_string()), "missing {w} in {out:?}");
        }
    }

    #[test]
    fn word_delimiter_keeps_all_positions_shared() {
        let filter = WordDelimiterFilter::new();
        let out = filter.filter(vec![Token::new("fooBar", 3, 0, 6)]);
        // All emitted tokens share the source position.
        assert!(out.iter().all(|t| t.position == 3), "out={out:?}");
    }

    #[test]
    fn code_analyzer_splits_and_lowercases_identifiers() {
        let registry = AnalyzerRegistry::default();
        let analyzer = registry
            .get_analyzer("code")
            .expect("code analyzer registered");

        let terms = analyzer.analyze_to_terms("id_to_fieldnorm");
        for w in ["id", "to", "fieldnorm", "id_to_fieldnorm"] {
            assert!(terms.contains(&w.to_string()), "missing {w} in {terms:?}");
        }

        let terms = analyzer.analyze_to_terms("getHTTPResponse");
        for w in ["get", "http", "response", "gethttpresponse"] {
            assert!(terms.contains(&w.to_string()), "missing {w} in {terms:?}");
        }

        // Letter/digit split keeps both the joined run and its parts.
        let terms = analyzer.analyze_to_terms("utf8_len");
        for w in ["utf8", "utf", "8", "len"] {
            assert!(terms.contains(&w.to_string()), "missing {w} in {terms:?}");
        }
    }

    #[test]
    fn word_delimiter_filter_resolvable_by_name() {
        let registry = AnalyzerRegistry::default();
        assert!(registry.resolve_builtin_filter("word_delimiter").is_some());
        assert!(registry
            .resolve_builtin_filter("word_delimiter_graph")
            .is_some());
    }

    #[test]
    fn standard_analyzer_unchanged_by_code_additions() {
        // Regression guard: the `standard` analyzer must NOT split identifiers,
        // protecting ES-YAML conformance.
        let registry = AnalyzerRegistry::default();
        let analyzer = registry.standard();
        let terms = analyzer.analyze_to_terms("id_to_fieldnorm getHTTPResponse");
        // snake_case stays intact; camelCase stays a single (lowercased) token.
        assert!(
            terms.contains(&"id_to_fieldnorm".to_string()),
            "terms={terms:?}"
        );
        assert!(
            terms.contains(&"gethttpresponse".to_string()),
            "terms={terms:?}"
        );
        assert!(!terms.contains(&"fieldnorm".to_string()), "terms={terms:?}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #204 — `settings.analysis` constructs that used to degrade in silence
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod unsupported_analysis_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_settings_block_we_can_honour_reports_nothing() {
        let settings = json!({
            "analysis": {
                "filter": {
                    "my_synonyms": { "type": "synonym", "synonyms": ["fast,quick"] },
                    "my_length":   { "type": "length", "min": 3, "max": 50 }
                },
                "tokenizer": {
                    "autocomplete_tok": { "type": "edge_ngram", "min_gram": 1, "max_gram": 10 }
                },
                "analyzer": {
                    "a": { "type": "custom", "tokenizer": "autocomplete_tok",
                           "filter": ["lowercase", "my_synonyms", "my_length"] },
                    "b": { "type": "english" }
                }
            }
        });
        assert!(
            AnalyzerRegistry::unsupported_analysis(&settings).is_empty(),
            "{:?}",
            AnalyzerRegistry::unsupported_analysis(&settings)
        );
        // Absent analysis block, and the outer-envelope shape, both work.
        assert!(AnalyzerRegistry::unsupported_analysis(&json!({})).is_empty());
        assert!(
            AnalyzerRegistry::unsupported_analysis(&json!({ "settings": settings })).is_empty()
        );
    }

    /// ES's canonical `index.analysis.*` form must be BUILT, not ignored — and
    /// therefore also validated.
    #[test]
    fn the_index_namespaced_form_is_honoured_by_builder_and_validator() {
        let namespaced = json!({
            "index": {
                "analysis": {
                    "tokenizer": { "edge": { "type": "edge_ngram", "min_gram": 1, "max_gram": 8 } },
                    "analyzer": { "ac": { "type": "custom", "tokenizer": "edge" } }
                }
            }
        });
        assert!(
            AnalyzerRegistry::unsupported_analysis(&namespaced).is_empty(),
            "{:?}",
            AnalyzerRegistry::unsupported_analysis(&namespaced)
        );

        let mut registry = AnalyzerRegistry::with_defaults();
        registry.apply_settings(&namespaced);
        let ac = registry
            .get_analyzer("ac")
            .expect("index.analysis.analyzer.ac must be registered, not silently dropped");
        assert!(
            ac.analyze_to_terms("java").contains(&"ja".to_string()),
            "the edge-ngram tokenizer must be the one in force"
        );

        // And a broken one in that form is caught rather than ignored.
        let bad = json!({ "index": { "analysis": { "analyzer": {
            "ac": { "type": "custom", "tokenizer": "nope" } } } } });
        assert_eq!(AnalyzerRegistry::unsupported_analysis(&bad).len(), 1);
    }

    /// Honouring `index.analysis` is a fix when an index is CREATED and a data
    /// bug when one is REOPENED — an index written before this build resolved
    /// that block has postings the declared analyzers do not match. The
    /// binding is therefore selectable, and `declares_namespaced_analysis_only`
    /// is what tells the open path whether the choice even matters.
    #[test]
    fn the_legacy_binding_reads_the_shorthand_only() {
        let namespaced = json!({ "index": { "analysis": { "analyzer": {
            "ac": { "type": "custom", "tokenizer": "whitespace" } } } } });
        let shorthand = json!({ "analysis": { "analyzer": {
            "ac": { "type": "custom", "tokenizer": "whitespace" } } } });

        // Canonical resolves both spellings…
        for settings in [&namespaced, &shorthand] {
            let mut registry = AnalyzerRegistry::with_defaults();
            registry.apply_settings_with_binding(settings, AnalysisBinding::Canonical);
            assert!(
                registry.get_analyzer("ac").is_some(),
                "canonical binding must resolve {settings}"
            );
        }

        // …the legacy binding resolves only the shorthand, which is exactly
        // what every build before the #204 sweep did.
        let mut registry = AnalyzerRegistry::with_defaults();
        registry.apply_settings_with_binding(&namespaced, AnalysisBinding::LegacyShorthandOnly);
        assert!(
            registry.get_analyzer("ac").is_none(),
            "the legacy binding must NOT resolve `index.analysis`"
        );
        let mut registry = AnalyzerRegistry::with_defaults();
        registry.apply_settings_with_binding(&shorthand, AnalysisBinding::LegacyShorthandOnly);
        assert!(
            registry.get_analyzer("ac").is_some(),
            "the legacy binding still resolves the shorthand"
        );
    }

    #[test]
    fn declares_namespaced_analysis_only_identifies_the_shape_that_diverges() {
        // Only the canonical nesting → the binding decides the outcome.
        assert!(AnalyzerRegistry::declares_namespaced_analysis_only(
            &json!({ "index": { "analysis": { "analyzer": {} } } })
        ));
        // …including through the outer `settings` envelope.
        assert!(AnalyzerRegistry::declares_namespaced_analysis_only(
            &json!({ "settings": { "index": { "analysis": { "analyzer": {} } } } })
        ));
        // Shorthand present → both bindings resolve identically.
        assert!(!AnalyzerRegistry::declares_namespaced_analysis_only(
            &json!({ "analysis": { "analyzer": {} } })
        ));
        assert!(!AnalyzerRegistry::declares_namespaced_analysis_only(
            &json!({ "analysis": { "analyzer": {} },
                     "index": { "analysis": { "analyzer": {} } } })
        ));
        // No analysis at all → nothing to decide.
        assert!(!AnalyzerRegistry::declares_namespaced_analysis_only(
            &json!({})
        ));
        assert!(!AnalyzerRegistry::declares_namespaced_analysis_only(
            &json!({ "index": { "number_of_replicas": 0 } })
        ));
    }

    /// The flat dotted spelling of an analysis declaration is one `apply_settings`
    /// never reads, so accepting it is accepting a block that analyses nothing.
    ///
    /// `PUT /{index}/_settings` refused all four spellings while `PUT /{index}`
    /// refused two: `unsupported_analysis` resolved the pointers `/analysis` and
    /// `/index/analysis` and nothing else, so the byte-equivalent dotted request
    /// got a `200` and `GET /_settings` echoed the analyzer straight back
    /// (issue #204).
    #[test]
    fn the_dotted_spellings_of_an_analysis_declaration_are_reported() {
        for dotted in [
            json!({ "index.analysis.filter.my_lower.type": "lowercase" }),
            json!({ "analysis.filter.my_lower.type": "lowercase" }),
            json!({ "index": { "analysis.filter.my_lower.type": "lowercase" } }),
            // Dotted namespace, nested value — `pointer("/index/analysis")`
            // wants an `index` OBJECT, which this key is not.
            json!({ "index.analysis": { "filter": { "my_lower": { "type": "lowercase" } } } }),
            // …and through the outer `settings` envelope, the shape index
            // creation actually hands over.
            json!({ "settings": { "index.analysis.analyzer.a.type": "custom" } }),
        ] {
            assert!(
                AnalyzerRegistry::declares_analysis(&dotted),
                "must be recognised as an analysis declaration: {dotted}"
            );
            let problems = AnalyzerRegistry::unsupported_analysis(&dotted);
            assert_eq!(
                problems.len(),
                1,
                "exactly one problem, naming the offending key: {dotted} -> {problems:?}"
            );
            assert!(
                problems[0].contains("dotted"),
                "the message must say what is wrong with it: {problems:?}"
            );
        }
    }

    /// …and the check stays narrow: a nested block is judged on its merits, and
    /// a dotted key outside the `analysis` namespace is not an analysis
    /// declaration at all. `index.sort.field` is parsed dotted by the same
    /// create handler, so a prefix-only test would have 400'd it.
    #[test]
    fn a_dotted_key_outside_the_analysis_namespace_is_not_a_declaration() {
        for innocent in [
            json!({ "index.number_of_shards": 1 }),
            json!({ "index.sort.field": "ts", "index.sort.order": "desc" }),
            json!({ "index": { "number_of_replicas": 0 } }),
            json!({}),
            // The nested spellings are NOT dotted — they are read, and judged
            // by the checks below rather than reported here.
            json!({ "analysis": { "filter": { "lowercase": { "type": "lowercase" } } } }),
            json!({ "index": { "analysis": { "filter": {} } } }),
        ] {
            assert!(
                AnalyzerRegistry::dotted_analysis_keys(&innocent).is_empty(),
                "not a dotted analysis declaration: {innocent}"
            );
        }
        // A nested, honourable block is still accepted — the dotted check must
        // not have turned the gate into a blanket refusal.
        assert!(
            AnalyzerRegistry::unsupported_analysis(&json!({
                "analysis": { "filter": { "lowercase": { "type": "lowercase" } } }
            }))
            .is_empty(),
            "a declaration whose NAME resolves to a built-in is honoured"
        );
    }

    #[test]
    fn unresolvable_tokenizer_is_reported() {
        // Pre-fix: this analyzer was registered with a StandardTokenizer and
        // the index tokenized nothing like the caller asked for.
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": {
                "analyzer": {
                    "autocomplete": { "type": "custom", "tokenizer": "edge_ngram_tok" }
                }
            }
        }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("edge_ngram_tok"), "{problems:?}");
    }

    #[test]
    fn unresolvable_filter_is_reported() {
        // `word_delimiter` is now a supported builtin filter, so it can no
        // longer stand in as the "unknown filter" example here. Use a name
        // that genuinely resolves to nothing instead. The typo'd `lowercse`
        // remains the second unknown so the reported count stays 2.
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": {
                "analyzer": {
                    "a": { "type": "custom", "tokenizer": "standard",
                           "filter": ["lowercse", "definitely_not_a_real_filter"] }
                }
            }
        }));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems.iter().any(|p| p.contains("lowercse")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("definitely_not_a_real_filter")),
            "{problems:?}"
        );
    }

    #[test]
    fn unsupported_component_type_is_reported_once_not_twice() {
        // A filter declared with an unsupported `type` is skipped by
        // apply_settings, so every analyzer naming it also loses it. Report
        // the actionable cause (the type) and not the knock-on reference.
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": {
                "filter": { "stem": { "type": "hunspell", "locale": "en_US" } },
                "tokenizer": { "tok": { "type": "char_group" } },
                "analyzer": {
                    "a": { "type": "custom", "tokenizer": "tok", "filter": ["stem"] }
                }
            }
        }));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems.iter().any(|p| p.contains("hunspell")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("char_group")),
            "{problems:?}"
        );
    }

    #[test]
    fn unknown_non_custom_analyzer_type_is_reported() {
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": { "analyzer": { "a": { "type": "kuromoji" } } }
        }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("kuromoji"), "{problems:?}");
    }

    #[test]
    fn invalid_pattern_tokenizer_regex_is_reported() {
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": { "tokenizer": { "t": { "type": "pattern", "pattern": "[unclosed" } } }
        }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("invalid pattern regex"),
            "{problems:?}"
        );
    }

    /// The allow-lists are the contract `unsupported_analysis` enforces; if one
    /// drifts from the `match` arms in `apply_settings` we would start
    /// rejecting configurations we can serve (or accepting ones we cannot).
    #[test]
    fn supported_analysis_types_all_build() {
        for t in SUPPORTED_FILTER_TYPES {
            let mut registry = AnalyzerRegistry::with_defaults();
            registry.apply_settings(&json!({
                "analysis": {
                    "filter": { "f": { "type": t, "synonyms": ["a,b"] } },
                    "analyzer": { "a": { "type": "custom", "tokenizer": "standard",
                                         "filter": ["f"] } }
                }
            }));
            assert!(
                registry.get_analyzer("a").is_some(),
                "filter type `{t}` is on the supported list but did not build"
            );
        }
        for t in SUPPORTED_TOKENIZER_TYPES {
            let mut registry = AnalyzerRegistry::with_defaults();
            registry.apply_settings(&json!({
                "analysis": {
                    "tokenizer": { "tk": { "type": t } },
                    "analyzer": { "a": { "type": "custom", "tokenizer": "tk" } }
                }
            }));
            assert!(
                registry.get_analyzer("a").is_some(),
                "tokenizer type `{t}` is on the supported list but did not build"
            );
        }
    }

    /// The canonical Elasticsearch-docs `rebuilt_english` shape, cut down to the
    /// parts xerj can serve. Judging `english_stop` on its `type` alone made
    /// `unsupported_analysis` report it as unsupported — and, through the
    /// create-time gate, 400 a `PUT /{index}` that xerj had always accepted AND
    /// analysed correctly. Both halves are asserted here: no complaint, and the
    /// registry built from the same block really does strip English stopwords.
    #[test]
    fn declared_filter_resolved_by_name_is_honoured_not_reported() {
        let settings = json!({
            "analysis": {
                "filter": {
                    "english_stop": { "type": "stop", "stopwords": "_english_" }
                },
                "analyzer": {
                    "rebuilt_english": {
                        "type": "custom",
                        "tokenizer": "standard",
                        "filter": ["lowercase", "english_stop"]
                    }
                }
            }
        });
        assert_eq!(
            AnalyzerRegistry::unsupported_analysis(&settings),
            Vec::<String>::new()
        );

        let mut registry = AnalyzerRegistry::with_defaults();
        registry.apply_settings(&settings);
        let terms = registry
            .get_analyzer("rebuilt_english")
            .expect("analyzer must be registered")
            .analyze_to_terms("The Quick Brown Fox");
        assert_eq!(
            terms,
            vec!["quick".to_string(), "brown".to_string(), "fox".to_string()],
            "the built registry must apply the stopword filter the block declares"
        );
    }

    /// …but only when the built-in is what the declaration actually asks for.
    /// A custom stopword list under the same name is NOT honoured — the English
    /// list would run instead — so it must still be reported, and with a message
    /// about the options rather than a misleading "unsupported type".
    #[test]
    fn declared_filter_with_options_the_builtin_cannot_reproduce_is_reported() {
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": {
                "filter": {
                    "english_stop": { "type": "stop", "stopwords": ["pelican", "walrus"] }
                }
            }
        }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("not supported") && problems[0].contains("english_stop"),
            "{problems:?}"
        );
    }

    #[test]
    fn builtin_tokenizer_resolved_by_name_is_honoured_not_reported() {
        assert_eq!(
            AnalyzerRegistry::unsupported_analysis(&json!({
                "analysis": {
                    "tokenizer": { "whitespace": { "type": "whitespace" } },
                    "analyzer": { "a": { "type": "custom", "tokenizer": "whitespace" } }
                }
            })),
            Vec::<String>::new()
        );
    }

    /// `AnalyzerPipeline::new(vec![], …)` — the char-filter slot is hard-coded
    /// empty, so a declared `html_strip` strips nothing. Measured pre-fix:
    /// `<b>hello</b>` tokenised to `["b", "hello", "b"]` with `problems == []`.
    #[test]
    fn char_filters_are_reported_because_they_are_never_built() {
        let settings = json!({
            "analysis": {
                "char_filter": { "strip_html": { "type": "html_strip" } },
                "analyzer": {
                    "a": {
                        "type": "custom",
                        "tokenizer": "standard",
                        "char_filter": ["strip_html"]
                    }
                }
            }
        });
        let problems = AnalyzerRegistry::unsupported_analysis(&settings);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems.iter().all(|p| p.contains("char filter")),
            "{problems:?}"
        );

        // The reason it must be reported: nothing strips the tags.
        let mut registry = AnalyzerRegistry::with_defaults();
        registry.apply_settings(&settings);
        assert!(
            registry
                .get_analyzer("a")
                .expect("registered")
                .analyze_to_terms("<b>hello</b>")
                .contains(&"b".to_string()),
            "char filters really are not applied — that is why they are refused"
        );
    }

    #[test]
    fn normalizers_are_reported_because_they_are_never_built() {
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": {
                "normalizer": { "lower": { "type": "custom", "filter": ["lowercase"] } }
            }
        }));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("normalizer"), "{problems:?}");
    }

    /// `apply_settings` reads `filter` with `as_array()`, so a bare string is
    /// dropped and the analyzer is built with no filters at all. Measured
    /// pre-fix: `problems == []`, terms `["ABC"]` from `"ABC"` — the
    /// `lowercase` the caller wrote never ran.
    #[test]
    fn wrong_shaped_analyzer_keys_are_reported() {
        let problems = AnalyzerRegistry::unsupported_analysis(&json!({
            "analysis": {
                "analyzer": {
                    "a": { "type": "custom", "tokenizer": "standard", "filter": "lowercase" },
                    "b": { "type": "custom", "tokenizer": ["standard"] }
                }
            }
        }));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems.iter().any(|p| p.contains("`filter` must be")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("`tokenizer` must be")),
            "{problems:?}"
        );
    }
}
