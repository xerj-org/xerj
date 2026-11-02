//! The exact chunk + embed + pool recipe `semantic_text` uses with the
//! built-in lexical embedder.
//!
//! It lives here, not in the engine, so the stored-section codec can rebuild
//! the derived vectors from the text instead of persisting them in every
//! stored `_source` (#1122, #2433). Ingest and regeneration both call these
//! functions, so a regenerated vector is bit-identical to the ingested one.
//! The codec only drops a vector after re-deriving it and comparing bits.
//! Changing anything here changes what old stripped segments rehydrate to,
//! so bump `stored_codec::derived_vectors::MARKER_LEXICAL_V1` if you do.

use crate::chunker::TextChunker;
use crate::local::local_embed;

/// Default chunk size (characters) for auto-embed-on-ingest chunking.
/// `EmbeddingConfig` has no per-field chunk parameter yet, so this is a
/// sensible fixed default (roughly a paragraph).
pub const SEMANTIC_CHUNK_SIZE: usize = 512;
/// Default overlap (characters) between consecutive chunks.
pub const SEMANTIC_CHUNK_OVERLAP: usize = 64;

/// The chunker used by the auto-embed-on-ingest path.
pub fn semantic_chunker() -> TextChunker {
    TextChunker::new(SEMANTIC_CHUNK_SIZE, SEMANTIC_CHUNK_OVERLAP)
}

/// UTF-8 byte offsets of the passages ingest embeds for `text`: the chunker's
/// passages, or the whole text when it cuts one or none. This is the offset
/// list `xerj-engine` `apply_semantic_embeddings_batch` builds and stores in
/// the passage metadata, so a client can size a document before it sends it
/// (#2807, `xerj_common::document_limit::semantic_vector_bytes`).
pub fn semantic_passage_offsets(text: &str) -> Vec<(usize, usize)> {
    // A text of at most `SEMANTIC_CHUNK_SIZE` bytes has at most that many
    // chars, so the chunker returns it whole; skip its per-char pass.
    if text.len() <= SEMANTIC_CHUNK_SIZE {
        return vec![(0, text.len())];
    }
    let chunks = semantic_chunker().chunk(text, None);
    if chunks.len() <= 1 {
        return vec![(0, text.len())];
    }
    chunks
        .iter()
        .map(|chunk| (chunk.start_offset, chunk.end_offset))
        .collect()
}

/// L2-normalize each input vector, mean-pool across all of them, then
/// re-normalize the pooled result. Produces a unit-norm vector of the same
/// dimensionality as the inputs (empty input => empty vec). Vectors whose
/// length differs from the first are skipped.
pub fn mean_pool_normalize(vectors: &[Vec<f32>]) -> Vec<f32> {
    let dims = match vectors.first() {
        Some(v) => v.len(),
        None => return Vec::new(),
    };
    let mut acc = vec![0.0f32; dims];
    let mut used = 0usize;
    for v in vectors {
        if v.len() != dims {
            continue;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for (a, x) in acc.iter_mut().zip(v.iter()) {
                *a += x / norm;
            }
            used += 1;
        }
    }
    if used > 0 {
        let inv = 1.0 / used as f32;
        for a in acc.iter_mut() {
            *a *= inv;
        }
    }
    // Re-normalize the pooled vector to unit length.
    let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for a in acc.iter_mut() {
            *a /= norm;
        }
    }
    acc
}

/// Chunk `text` and embed each overlapping chunk with the built-in *local*
/// embedder, returning one unit-norm vector per passage. Short text
/// (<= chunk_size) produces a single chunk, so the result is
/// `vec![local_embed(text, dims)]`.
pub fn local_chunk_vectors(text: &str, dims: usize) -> Vec<Vec<f32>> {
    let chunks = semantic_chunker().chunk(text, None);
    if chunks.len() <= 1 {
        return vec![local_embed(text, dims)];
    }
    chunks.iter().map(|c| local_embed(&c.text, dims)).collect()
}

/// A pooled vector plus, for multi-passage text, the per-passage vectors.
pub type LexicalSemanticVectors = (Vec<f32>, Option<Vec<Vec<f32>>>);

/// The vectors ingest persists for one lexical `semantic_text` value: the
/// pooled vector, plus the per-passage vectors when there is more than one
/// passage. Mirrors `apply_semantic_embeddings_batch`: one passage is used
/// as-is, several are mean-pooled and re-normalized. `None` for `dims == 0`.
pub fn lexical_semantic_vectors(text: &str, dims: usize) -> Option<LexicalSemanticVectors> {
    if dims == 0 {
        return None;
    }
    let mut chunks: Vec<Vec<f32>> = local_chunk_vectors(text, dims)
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect();
    match chunks.len() {
        0 => None,
        1 => chunks.pop().map(|pooled| (pooled, None)),
        _ => {
            let pooled = mean_pool_normalize(&chunks);
            Some((pooled, Some(chunks)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passage_offsets_match_the_chunker_on_both_sides_of_the_fast_path() {
        // #2807: the short-text shortcut must give what the chunker gives.
        let multibyte = "é".repeat(300); // 600 bytes, 300 chars: one passage
        let long = "Word after word. ".repeat(100);
        for text in ["", "short", multibyte.as_str(), long.as_str()] {
            let chunks = semantic_chunker().chunk(text, None);
            let expected: Vec<(usize, usize)> = if chunks.len() <= 1 {
                vec![(0, text.len())]
            } else {
                chunks
                    .iter()
                    .map(|c| (c.start_offset, c.end_offset))
                    .collect()
            };
            assert_eq!(
                semantic_passage_offsets(text),
                expected,
                "{} bytes",
                text.len()
            );
        }
        assert!(semantic_passage_offsets(&long).len() > 1);
    }

    #[test]
    fn short_text_is_one_unpooled_passage() {
        let (pooled, chunks) = lexical_semantic_vectors("a short passage", 16).unwrap();
        assert!(chunks.is_none());
        assert_eq!(pooled, local_embed("a short passage", 16));
    }

    #[test]
    fn long_text_pools_its_passages() {
        let text = "lorem ipsum dolor sit amet ".repeat(60);
        let (pooled, chunks) = lexical_semantic_vectors(&text, 32).unwrap();
        let chunks = chunks.expect("long text has several passages");
        assert!(chunks.len() > 1);
        assert_eq!(pooled, mean_pool_normalize(&chunks));
        assert!(lexical_semantic_vectors(&text, 0).is_none());
    }

    /// FNV-1a 64 over the f32 bit patterns of the pooled vector, then each
    /// passage vector (none for single-passage text).
    fn fingerprint((pooled, chunks): &LexicalSemanticVectors) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |v: &[f32]| {
            for f in v {
                for b in f.to_bits().to_le_bytes() {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            // Separator so passage boundaries are part of the hash.
            h ^= 0xff;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        feed(pooled);
        for c in chunks.iter().flatten() {
            feed(c);
        }
        h
    }

    /// Stored segments drop these vectors and rebuild them on read, tagged
    /// `lexical-v1` (#1122/#2433). If tokenization, case folding, Unicode
    /// tables (a toolchain bump), hashing or chunking change their output,
    /// already-written segments would read back different vectors than the
    /// HNSW index holds. A change here needs a new marker version, not a
    /// re-pinned hash.
    #[test]
    fn lexical_v1_vectors_are_pinned() {
        let long_mixed = "Grüße aus Köln. Ünïcödé façade naïve café. Ελληνικά κείμενα. \
                          Кириллица текст. 日本語のテキスト。中文文本。한국어 텍스트. \
                          עברית العربية हिन्दी ไทย. İstanbul ß ǅ ﬁ Ⅻ 𝔘𝔫𝔦𝔠𝔬𝔡𝔢 🚀🎉 "
            .repeat(12);
        let corpus: Vec<(&str, String, usize)> = vec![
            (
                "ascii",
                "Quarterly revenue grew in the storage ledger".into(),
                32,
            ),
            (
                "latin1",
                "Grüße aus Köln — façade, naïve café, ÆØÅ æøå".into(),
                32,
            ),
            ("turkic", "İstanbul ISTANBUL istanbul ıi Iİ".into(), 16),
            ("sharp_s", "STRASSE straße Straße ß ẞ".into(), 16),
            (
                "greek_cyrillic",
                "ΣΊΣΥΦΟΣ σίσυφος Ελληνικά — Москва МОСКВА ёЁ".into(),
                64,
            ),
            ("cjk", "日本語のテキスト 中文文本 한국어 텍스트".into(), 64),
            ("rtl_indic_thai", "עברית العربية हिन्दी ไทย".into(), 32),
            ("compat_marks", "ǅ ﬁ Ⅻ e\u{301} é Å Å 𝔘𝔫𝔦𝔠𝔬𝔡𝔢".into(), 32),
            ("emoji_digits", "🚀🎉 👩‍👩‍👧 ١٢٣ ４５６ 123".into(), 16),
            ("multi_passage_mixed", long_mixed, 32),
        ];
        let got: Vec<(&str, u64)> = corpus
            .iter()
            .map(|(name, text, dims)| {
                let v = lexical_semantic_vectors(text, *dims).unwrap();
                (*name, fingerprint(&v))
            })
            .collect();
        let multi = lexical_semantic_vectors(&corpus[9].1, 32).unwrap();
        assert!(multi.1.as_ref().is_some_and(|c| c.len() > 1));
        let want: &[(&str, u64)] = &[
            ("ascii", 0xb18dc7157381c097),
            ("latin1", 0x5c3d6b7e97441622),
            ("turkic", 0xdbd5eb2aec430f62),
            ("sharp_s", 0x2ddf8010c6cf313d),
            ("greek_cyrillic", 0x67d0ea0cf16331cc),
            ("cjk", 0xf2f4de163e1b3b89),
            ("rtl_indic_thai", 0x08493924a0bb30aa),
            ("compat_marks", 0x36bc49586424f6e8),
            ("emoji_digits", 0x40d6b03c9056dd2b),
            ("multi_passage_mixed", 0x2337850d5df9865d),
        ];
        assert_eq!(got, want, "lexical-v1 embeddings drifted");
    }
}
