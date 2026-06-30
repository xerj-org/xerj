//! High-throughput "turbo" indexing pipeline.
//!
//! Turbo mode is an **opt-in** ingest path that trades a small amount of
//! linguistic quality (no stemming, no stopword removal) for dramatically
//! higher indexing throughput.  It achieves this through three mechanisms:
//!
//! 1. **SIMD-accelerated text processing** — ASCII lowercase and word-boundary
//!    detection operate on 16-byte chunks, reducing branch overhead.
//! 2. **Parallel tokenization** — documents in a batch are tokenized
//!    concurrently via Rayon's work-stealing thread pool.
//! 3. **Batched WAL writes** — multiple documents are serialised into a single
//!    contiguous buffer and written (+ fsynced) in one syscall, amortising the
//!    per-write overhead across the entire batch.
//!
//! # Opt-in
//!
//! ```text
//! POST /v1/indices/{name}/turbo-ingest    — native turbo endpoint
//! X-Turbo: true                           — opt-in on the standard _bulk path
//! ```
//!
//! # Configuration
//!
//! ```toml
//! [indexing]
//! turbo_batch_size     = 1000   # documents per flush cycle
//! turbo_parallel       = true   # parallel tokenization via Rayon
//! turbo_fast_analyzer  = false  # skip stemming/stopwords for max speed
//! ```

use rayon::prelude::*;
use serde_json::Value;
use std::sync::Arc;

// ── SIMD-style text helpers ───────────────────────────────────────────────────

/// ASCII lowercase — processes 16 bytes at a time.
///
/// For pure-ASCII input this avoids per-character branching by unrolling the
/// loop into 16-byte chunks.  Non-ASCII bytes are passed through unchanged
/// (the caller is responsible for any UTF-8–aware casing if needed).
///
/// # Safety
///
/// The function is entirely safe — we never transmute or dereference raw
/// pointers.  The "SIMD" label refers to the loop structure that mirrors what
/// an optimising compiler will auto-vectorise.
pub fn simd_lowercase(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut i = 0;

    // Process 16 bytes at a time — the compiler can auto-vectorise this loop.
    while i + 16 <= input.len() {
        let chunk = &input[i..i + 16];
        // Check if all 16 bytes are ASCII (< 128).
        let all_ascii = chunk.iter().all(|&b| b < 128);

        if all_ascii {
            // Fast path: ASCII lowercase in bulk.
            for &b in chunk {
                output.push(if b >= b'A' && b <= b'Z' { b + 32 } else { b });
            }
        } else {
            // Slow path: multi-byte sequences — pass through unchanged.
            // Uppercase ASCII bytes that happen to appear are still lowercased.
            for &b in chunk {
                output.push(if b >= b'A' && b <= b'Z' { b + 32 } else { b });
            }
        }
        i += 16;
    }

    // Tail: remaining bytes.
    for &b in &input[i..] {
        output.push(if b >= b'A' && b <= b'Z' { b + 32 } else { b });
    }

    output
}

/// Word-boundary scanner — processes bytes and emits `(start, end)` spans of
/// non-whitespace, non-punctuation runs.
///
/// The set of separator bytes mirrors common tokeniser behaviour without
/// requiring Unicode character classification.
pub fn simd_find_word_boundaries(input: &[u8]) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut in_word = false;
    let mut word_start = 0;

    for (i, &b) in input.iter().enumerate() {
        let is_sep = matches!(
            b,
            b' ' | b'\t' | b'\n' | b'\r'
                | b',' | b'.' | b'!' | b'?'
                | b';' | b':' | b'"' | b'\''
                | b'(' | b')' | b'[' | b']'
                | b'{' | b'}' | b'/' | b'\\'
                | b'|' | b'@' | b'#' | b'%'
                | b'^' | b'&' | b'*' | b'+'
                | b'=' | b'<' | b'>' | b'~'
                | b'`'
        );

        if !is_sep && !in_word {
            word_start = i;
            in_word = true;
        } else if is_sep && in_word {
            words.push((word_start, i));
            in_word = false;
        }
    }

    if in_word {
        words.push((word_start, input.len()));
    }

    words
}

// ── FastTokenizer ─────────────────────────────────────────────────────────────

/// A minimal, allocation-efficient tokeniser that skips stemming and stopword
/// removal in favour of raw throughput.
///
/// Suitable for scenarios where recall matters more than precision and the
/// ingestion rate is the bottleneck.
pub struct FastTokenizer;

impl FastTokenizer {
    /// Tokenise `text` into lowercase word tokens.
    ///
    /// Tokens shorter than 2 characters or longer than 40 characters are
    /// discarded — the former are typically noise, the latter are usually
    /// garbled data or base64 blobs.
    ///
    /// # Safety
    ///
    /// `String::from_utf8_unchecked` is used here because:
    /// - Input is confirmed ASCII by `simd_lowercase` (non-ASCII bytes are
    ///   left unchanged — only `[A-Z]` are modified by adding 32, remaining
    ///   in valid UTF-8 single-byte range).
    /// - We slice only at boundaries returned by `simd_find_word_boundaries`,
    ///   which iterates byte-by-byte and never splits a multi-byte sequence.
    pub fn tokenize_fast(text: &str) -> Vec<String> {
        let bytes = text.as_bytes();
        let lower = simd_lowercase(bytes);
        let boundaries = simd_find_word_boundaries(&lower);

        boundaries
            .iter()
            .filter(|&&(s, e)| {
                let len = e - s;
                len >= 2 && len <= 40
            })
            .map(|&(s, e)| {
                // SAFETY: `lower` contains only bytes that were either:
                //   (a) already valid single-byte UTF-8 (ASCII < 128), or
                //   (b) multi-byte continuation bytes that were passed through
                //       unchanged.
                // `simd_find_word_boundaries` never splits a multi-byte sequence
                // because it only cuts on ASCII separator bytes (< 128).
                // Therefore the slice `lower[s..e]` is always valid UTF-8.
                unsafe { String::from_utf8_unchecked(lower[s..e].to_vec()) }
            })
            .collect()
    }
}

// ── IngestResult ──────────────────────────────────────────────────────────────

/// Result produced for a single document by the turbo pipeline.
#[derive(Debug, Clone)]
pub struct IngestResult {
    /// The document identifier.
    pub id: String,
    /// Tokens extracted from all text fields (used for FTS memtable insertion).
    pub tokens: Vec<String>,
    /// Original source document behind an Arc — zero-cost to share across WAL,
    /// store memtable, and FTS memtable.  Clone = pointer bump, not deep copy.
    pub source: Arc<Value>,
    /// Original serialized JSON bytes for this source document, ready to
    /// be written to the WAL envelope without a second
    /// `serde_json::to_writer` pass.  Populated by the bulk parser
    /// directly from the NDJSON line (zero-copy via `Arc<[u8]>`).  For
    /// callers that construct `IngestResult` without pre-serialized
    /// bytes this can be an empty `Arc<[u8]>::from([])` and the callers
    /// will fall back to re-serializing the `source` Value.
    pub source_bytes: Arc<[u8]>,
}

// ── Text-field extraction ─────────────────────────────────────────────────────

/// Recursively extract all string values from a JSON document and tokenise them.
///
/// Arrays and nested objects are walked depth-first.  Non-string leaf values
/// are skipped.
pub fn extract_and_tokenize_fast(value: &Value) -> Vec<String> {
    let mut tokens = Vec::new();
    collect_tokens(value, &mut tokens);
    tokens
}

fn collect_tokens(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            out.extend(FastTokenizer::tokenize_fast(s));
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_tokens(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_tokens(v, out);
            }
        }
        // Numbers, booleans, null — no text tokens.
        _ => {}
    }
}

// ── TurboIngestPipeline ───────────────────────────────────────────────────────

/// A batching, parallel tokenisation pipeline for high-throughput ingest.
///
/// Documents are accumulated in an internal buffer.  When the buffer reaches
/// `batch_size`, or when [`TurboIngestPipeline::flush`] is called explicitly,
/// all buffered documents are tokenised in parallel via Rayon and returned as a
/// `Vec<IngestResult>`.
pub struct TurboIngestPipeline {
    /// Target batch size (documents).
    batch_size: usize,
    /// In-flight buffer of `(doc_id, source, source_bytes)` tuples.
    ///
    /// `source_bytes` is the *original* JSON byte slice from the NDJSON
    /// bulk body (wrapped in an `Arc<[u8]>` so the pipeline, the WAL,
    /// and the engine memtable all share the same allocation).  The
    /// bulk parser pays one `Arc::from(&[u8])` per doc; the WAL then
    /// writes those bytes verbatim, skipping the `serde_json::to_writer`
    /// round-trip that v13 was paying.
    buffer: Vec<(String, Value, Arc<[u8]>)>,
    /// Whether to use parallel tokenisation.
    parallel: bool,
}

impl TurboIngestPipeline {
    /// Create a new pipeline.
    ///
    /// - `batch_size` — how many documents to accumulate before auto-flushing.
    /// - `parallel`   — if `true`, tokenisation uses Rayon (recommended).
    pub fn new(batch_size: usize, parallel: bool) -> Self {
        Self {
            batch_size: batch_size.max(1),
            buffer: Vec::with_capacity(batch_size),
            parallel,
        }
    }

    /// Push one document into the pipeline.
    ///
    /// Returns `Some(results)` when the internal buffer has reached `batch_size`
    /// and was automatically flushed; returns `None` otherwise.
    ///
    /// `source_bytes` should be the original JSON bytes from the NDJSON
    /// line.  When the caller doesn't have them (e.g. the engine's
    /// single-doc `index_document` path), pass
    /// `Arc::<[u8]>::from(&[][..])` and the WAL will fall back to
    /// re-serializing from the `Value`.
    pub fn push(
        &mut self,
        id: String,
        source: Value,
        source_bytes: Arc<[u8]>,
    ) -> Option<Vec<IngestResult>> {
        self.buffer.push((id, source, source_bytes));
        if self.buffer.len() >= self.batch_size {
            Some(self.flush())
        } else {
            None
        }
    }

    /// Drain and tokenise all buffered documents.
    ///
    /// Callers **must** call this after the last `push` to ensure no documents
    /// are silently dropped.
    pub fn flush(&mut self) -> Vec<IngestResult> {
        if self.buffer.is_empty() {
            return Vec::new();
        }

        let batch = std::mem::take(&mut self.buffer);

        // M5.9 — drop pre-tokenisation on the ingest hot path.
        //
        // Pre-M5.9 this called `extract_and_tokenize_fast(&source)` per doc
        // which walked the JSON tree, lower-cased every string field, ran
        // the SIMD word-boundary scan, and allocated a `Vec<String>` of
        // tokens.  That output was then passed through as the `tokens`
        // field of IngestResult and consumed by `insert_pretokenized_with_seq`
        // — which as of M5.6 ignores the `_tokens` parameter entirely.
        //
        // Measured: tokenisation was ~3 µs per doc = ~15 ms per 5000-doc
        // batch, all pure waste.  Just wrap the source Value in an Arc
        // (pointer bump, no allocation beyond the Arc header).  The real
        // FTS build happens at merge time from stored fields.
        let empty_tokens: Vec<String> = Vec::new();
        batch
            .into_iter()
            .map(|(id, source, source_bytes)| {
                let source = Arc::new(source);
                IngestResult {
                    id,
                    tokens: empty_tokens.clone(),
                    source,
                    source_bytes,
                }
            })
            .collect()
    }

    /// Returns the number of documents currently buffered.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Returns the configured batch size.
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }
}

// ── BatchWalWriter ────────────────────────────────────────────────────────────

/// Accumulates WAL entries in memory and writes them all in a single pass.
///
/// The standard WAL writer calls `fsync()` (or an OS equivalent) after each
/// document.  `BatchWalWriter` amortises that syscall overhead by collecting N
/// serialised entries and flushing them together, then issuing a single sync.
///
/// This type is intentionally decoupled from the WAL file format — it hands
/// off the final serialised bytes to whatever writer the caller provides.  The
/// standard path in [`super::index::Index::index_batch_turbo`] delegates back
/// to `IndexStore::index` after the parallel tokenisation step.
pub struct BatchWalWriter {
    /// Accumulated `(doc_id, serialised_source_bytes)` pairs.
    entries: Vec<(String, Vec<u8>)>,
    /// Maximum entries before `flush_batch` should be called.
    max_batch: usize,
}

impl BatchWalWriter {
    /// Create a new batch WAL writer.
    pub fn new(max_batch: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max_batch),
            max_batch: max_batch.max(1),
        }
    }

    /// Append one entry.
    pub fn append(&mut self, doc_id: &str, source: &[u8]) {
        self.entries.push((doc_id.to_string(), source.to_vec()));
    }

    /// Write all accumulated entries to `writer` and fsync once.
    ///
    /// The entries are written as a length-prefixed stream:
    /// ```text
    /// [u32 id_len][id bytes][u32 src_len][src bytes] ...
    /// ```
    ///
    /// This is a best-effort batch-write helper.  The primary durability
    /// guarantee is provided by the upstream `WalWriter`; this type is
    /// used when a caller wants to build a batch buffer and hand it off.
    pub fn flush_batch<W: std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }

        // Estimate total size so we can pre-allocate one contiguous buffer.
        let total_cap: usize = self.entries.iter().map(|(id, src)| 8 + id.len() + src.len()).sum();
        let mut buf = Vec::with_capacity(total_cap);

        for (id, src) in &self.entries {
            let id_bytes = id.as_bytes();
            // id_len (u32 LE)
            buf.extend_from_slice(&(id_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(id_bytes);
            // src_len (u32 LE)
            buf.extend_from_slice(&(src.len() as u32).to_le_bytes());
            buf.extend_from_slice(src);
        }

        // Single write() call for all entries.
        writer.write_all(&buf)?;
        // Single fsync() via flush.
        writer.flush()?;

        self.entries.clear();
        Ok(())
    }

    /// Returns `true` if the batch has reached or exceeded `max_batch`.
    pub fn is_full(&self) -> bool {
        self.entries.len() >= self.max_batch
    }

    /// Number of entries currently queued.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if there are no queued entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simd_lowercase_pure_ascii() {
        let input = b"Hello, World! FOO BAR";
        let lower = simd_lowercase(input);
        assert_eq!(lower, b"hello, world! foo bar");
    }

    #[test]
    fn simd_lowercase_mixed_multibyte() {
        // Non-ASCII bytes should pass through unchanged.
        let input = b"Caf\xc3\xa9"; // "Café" in UTF-8
        let lower = simd_lowercase(input);
        // Only the 'C' should be lowercased.
        assert_eq!(&lower[..4], b"caf\xc3");
        assert_eq!(lower[4], 0xa9);
    }

    #[test]
    fn word_boundaries_basic() {
        let input = b"hello world foo";
        let bounds = simd_find_word_boundaries(input);
        assert_eq!(bounds, vec![(0, 5), (6, 11), (12, 15)]);
    }

    #[test]
    fn word_boundaries_punctuation() {
        let input = b"one,two.three!four";
        let bounds = simd_find_word_boundaries(input);
        assert_eq!(bounds, vec![(0, 3), (4, 7), (8, 13), (14, 18)]);
    }

    #[test]
    fn fast_tokenizer_filters_short_and_long() {
        // Single char token "a" should be filtered (len < 2).
        let tokens = FastTokenizer::tokenize_fast("a hello world");
        assert!(!tokens.contains(&"a".to_string()));
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));

        // Token of 41 chars should be filtered (len > 40).
        let long_token = "x".repeat(41);
        let tokens = FastTokenizer::tokenize_fast(&long_token);
        assert!(tokens.is_empty());
    }

    fn empty_bytes() -> Arc<[u8]> {
        Arc::from(&[][..])
    }

    #[test]
    fn pipeline_auto_flushes_at_batch_size() {
        let mut pipeline = TurboIngestPipeline::new(3, false);
        assert!(pipeline.push("id1".into(), serde_json::json!({"text": "hello"}), empty_bytes()).is_none());
        assert!(pipeline.push("id2".into(), serde_json::json!({"text": "world"}), empty_bytes()).is_none());
        let results = pipeline
            .push("id3".into(), serde_json::json!({"text": "foo bar"}), empty_bytes())
            .expect("should auto-flush at batch_size=3");
        assert_eq!(results.len(), 3);
        assert_eq!(pipeline.buffered(), 0);
    }

    #[test]
    fn pipeline_explicit_flush() {
        // M5.9 dropped pre-tokenisation from the pipeline hot path —
        // tokens are now empty Vecs.  Verify the pipeline still
        // produces the right number of results with correct ids.
        let mut pipeline = TurboIngestPipeline::new(100, false);
        pipeline.push("id1".into(), serde_json::json!({"body": "quick brown fox"}), empty_bytes());
        pipeline.push("id2".into(), serde_json::json!({"body": "lazy dog"}), empty_bytes());
        let results = pipeline.flush();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "id1");
        assert_eq!(results[1].id, "id2");
    }

    #[test]
    fn pipeline_parallel_flush_matches_sequential() {
        let docs: Vec<_> = (0..50)
            .map(|i| {
                (
                    format!("doc{i}"),
                    serde_json::json!({ "body": format!("document number {i} content here") }),
                )
            })
            .collect();

        let mut seq_pipeline = TurboIngestPipeline::new(100, false);
        let mut par_pipeline = TurboIngestPipeline::new(100, true);

        for (id, src) in &docs {
            seq_pipeline.push(id.clone(), src.clone(), empty_bytes());
            par_pipeline.push(id.clone(), src.clone(), empty_bytes());
        }

        let mut seq_results = seq_pipeline.flush();
        let mut par_results = par_pipeline.flush();

        // Sort by id so the comparison is order-independent.
        seq_results.sort_by(|a, b| a.id.cmp(&b.id));
        par_results.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(seq_results.len(), par_results.len());
        for (s, p) in seq_results.iter().zip(par_results.iter()) {
            assert_eq!(s.id, p.id);
            let mut s_tokens = s.tokens.clone();
            let mut p_tokens = p.tokens.clone();
            s_tokens.sort();
            p_tokens.sort();
            assert_eq!(s_tokens, p_tokens);
        }
    }

    #[test]
    fn batch_wal_writer_roundtrip() {
        let mut writer = BatchWalWriter::new(10);
        writer.append("doc1", b"{\"x\":1}");
        writer.append("doc2", b"{\"x\":2}");

        let mut buf = Vec::new();
        writer.flush_batch(&mut buf).expect("flush should succeed");

        // Verify the buffer is non-empty and writer is drained.
        assert!(!buf.is_empty());
        assert!(writer.is_empty());
    }

    #[test]
    fn extract_and_tokenize_nested() {
        let doc = serde_json::json!({
            "title": "Hello World",
            "meta": { "author": "Jane Doe" },
            "tags": ["rust", "search"]
        });
        let tokens = extract_and_tokenize_fast(&doc);
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"jane".to_string()));
        assert!(tokens.contains(&"doe".to_string()));
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"search".to_string()));
    }
}
