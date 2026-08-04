//! Built-in, zero-config deterministic text embedder.
//!
//! [`local_embed`] turns arbitrary text into a fixed-dimensional, L2-normalised
//! `f32` vector using the *feature-hashing trick* (a.k.a. the "hashing
//! vectoriser"): word unigrams and intra-word character trigrams are hashed
//! into a fixed number of buckets with a signed contribution, then the whole
//! accumulator is L2-normalised so that cosine similarity is comparable across
//! documents of different lengths.
//!
//! This is **not** a neural model — it captures lexical / sub-word overlap, not
//! deep semantics. Its job is to make the `semantic_text` field type work
//! end-to-end with **zero external dependencies** (offline, no API key), giving
//! a sensible out-of-the-box experience: paraphrases that share vocabulary rank
//! above unrelated text. When a real
//! [`crate::embed::EmbeddingProxy`] is configured the engine uses that instead
//! for production-quality embeddings — the *same* embedder is always used at
//! ingest and query time so the vectors are comparable.
//!
//! Key properties:
//! * **Deterministic** — identical input always yields the identical vector,
//!   across processes and restarts (no learned weights, no RNG).
//! * **Configurable dimensionality** — `dims` (default [`DEFAULT_DIMS`]).
//! * **Comparable** — L2-normalised, so cosine similarity is well-behaved.

/// Default embedding dimensionality for the built-in embedder.
///
/// 384 mirrors the popular `all-MiniLM-L6-v2` output size, so switching from
/// the built-in embedder to a real proxy of that width needs no mapping change.
pub const DEFAULT_DIMS: usize = 384;

/// 64-bit FNV-1a hash — small, fast, stable across platforms.
#[inline]
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001b3);
    }
    h
}

/// Accumulate one hashed feature into `acc` with a signed weight. The low bits
/// pick the bucket; one high bit picks the sign (signed hashing reduces the
/// collision bias of unsigned feature hashing).
#[inline]
fn add_feature(acc: &mut [f32], token: &[u8], weight: f32) {
    let dims = acc.len();
    if dims == 0 {
        return;
    }
    let h = fnv1a64(token);
    let idx = (h % dims as u64) as usize;
    let sign = if (h >> 63) & 1 == 0 { 1.0 } else { -1.0 };
    acc[idx] += weight * sign;
}

/// Embed `text` into a deterministic, L2-normalised vector of length `dims`.
///
/// Empty / whitespace-only text (or `dims == 0`) yields an all-zero vector;
/// cosine similarity against a zero vector is defined as 0 by
/// `compute_vector_similarity`, so this degrades gracefully rather than
/// panicking.
pub fn local_embed(text: &str, dims: usize) -> Vec<f32> {
    let mut acc = vec![0.0f32; dims];
    if dims == 0 {
        return acc;
    }

    let lower = text.to_lowercase();
    for token in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        // Whole-word unigram — the dominant lexical signal.
        add_feature(&mut acc, token.as_bytes(), 1.0);

        // Intra-word character trigrams (padded so word boundaries matter),
        // at a lower weight. These give partial credit to morphological
        // variants (e.g. "run" / "running" share the "run" shingle), which
        // nudges the embedding a little past exact-keyword matching.
        let padded: Vec<char> = std::iter::once('#')
            .chain(token.chars())
            .chain(std::iter::once('#'))
            .collect();
        if padded.len() >= 3 {
            for w in padded.windows(3) {
                let mut buf = [0u8; 12];
                let mut n = 0;
                for &ch in w {
                    let s = ch.len_utf8();
                    ch.encode_utf8(&mut buf[n..]);
                    n += s;
                }
                add_feature(&mut acc, &buf[..n], 0.35);
            }
        }
    }

    // L2-normalise so cosine similarity is length-invariant.
    let norm: f32 = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in acc.iter_mut() {
            *x /= norm;
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn dims_and_determinism() {
        let a = local_embed("the quick brown fox", 128);
        let b = local_embed("the quick brown fox", 128);
        assert_eq!(a.len(), 128);
        assert_eq!(a, b, "embedding must be deterministic");
    }

    /// Deliberately independent of [`fnv1a64`] so that changing the embedder's
    /// hash cannot silently move the checksum with it.
    fn djb2_over_bits(v: &[f32]) -> u64 {
        let mut h: u64 = 5381;
        for x in v {
            for byte in x.to_le_bytes() {
                h = h.wrapping_mul(33) ^ byte as u64;
            }
        }
        h
    }

    /// GOLDEN VECTOR — do not "fix" this by re-recording the constants.
    ///
    /// `lexical` is the default backend and the one most deployments run, and
    /// `GET /v1/embedding/identity` reports it as `resumable: true`, which
    /// tells `xerj autoindex` it may resume an existing semantic run straight
    /// across a restart. That identity is derived from the literal string
    /// `lexical-feature-hash.v1;dimensions=384`, so nothing in it is computed
    /// from this function: changing the tokenizer, the trigram padding, the
    /// feature weights, the hash seed, or the normalisation would move every
    /// vector into a new space while `identity_sha256` stayed byte-identical,
    /// and autoindex would resume across the change and mix the two spaces —
    /// exactly the failure the identity endpoint exists to prevent.
    ///
    /// If this test fails, the lexical vector space changed. The fix is to
    /// bump `lexical-feature-hash.v1` to `.v2` in
    /// `xerj-engine/src/index.rs::embedding_execution_identity` (which changes
    /// `identity_sha256` and so makes resumes across the change fail closed),
    /// and only then re-record the constants below.
    #[test]
    fn lexical_vector_space_is_frozen() {
        let v = local_embed(
            "quarterly revenue increased in the london office",
            DEFAULT_DIMS,
        );
        assert_eq!(v.len(), DEFAULT_DIMS);

        // Whole-vector fingerprint: catches any change to any bucket.
        assert_eq!(
            djb2_over_bits(&v),
            14262738046721457820,
            "the lexical vector space changed; see this test's doc comment"
        );

        // A few exact buckets, so a failure says *what* moved rather than just
        // that a checksum differs. Bit-exact: these are IEEE-754 f32 results
        // of a fixed sequence of adds and one divide, so they are reproducible
        // across platforms.
        for (index, expected) in [
            (0usize, 0.09425097f32),
            (43, 0.09425097),
            (120, -0.09425097),
            (123, 0.26928848),
        ] {
            assert_eq!(
                v[index], expected,
                "bucket {index} moved: {} != {expected}",
                v[index]
            );
        }
    }

    #[test]
    fn l2_normalised() {
        let v = local_embed("hello world of vectors", DEFAULT_DIMS);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "expected unit norm, got {norm}");
    }

    #[test]
    fn empty_text_is_zero_vector() {
        let v = local_embed("   ", 64);
        assert_eq!(v.len(), 64);
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn zero_dims_is_empty() {
        assert!(local_embed("anything", 0).is_empty());
    }

    #[test]
    fn paraphrase_ranks_above_unrelated() {
        // A paraphrase sharing vocabulary must be closer (higher cosine) than
        // a topically-unrelated sentence — the property `semantic_text` relies
        // on for kNN retrieval.
        let dims = DEFAULT_DIMS;
        let doc_relevant = local_embed("a hungry dog chased the ball across the green park", dims);
        let doc_unrelated = local_embed(
            "investors sold technology shares amid rising interest rates",
            dims,
        );
        let query = local_embed("a dog ran after a ball in the park", dims);

        let sim_relevant = dot(&query, &doc_relevant);
        let sim_unrelated = dot(&query, &doc_unrelated);
        assert!(
            sim_relevant > sim_unrelated,
            "relevant {sim_relevant} should beat unrelated {sim_unrelated}"
        );
    }
}
