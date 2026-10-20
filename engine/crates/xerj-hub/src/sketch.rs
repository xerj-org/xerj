//! Deterministic Bloom filter over a corpus's terms — the catalogue's offline
//! router hint (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md`, step 4, "Term sketch").
//!
//! The catalogue carries one [`Sketch`] per corpus so the client can rule a
//! corpus out for a question without downloading its pack: a term the question
//! mentions that the sketch reports absent is *certainly* not in that corpus.
//! A Bloom filter answers "possibly present" (with a bounded false-positive
//! rate) or "certainly absent" — never a false negative — which is exactly the
//! guarantee the router needs.
//!
//! The parameters are fixed so that every builder produces byte-identical
//! `bits_base64` for the same set of terms:
//!
//! - The catalogue builder chooses at most [`MAX_TERMS`] lower-case terms per
//!   corpus ([`select_terms`]); [`build`] hashes whatever it is given.
//! - Collect the distinct terms first; `m_bits = max(MIN_BITS, BITS_PER_TERM ·
//!   distinct)` = `max(1024, 16 · distinct)`, rounded up to a multiple of 64.
//!   (The plan's first rule, `8 · max(1024, 10 · distinct)`, made the measured
//!   catalogue ~136 MB at 513 corpora against a 2 MB target.)
//! - `k_hashes = 7`.
//! - Double hashing (Kirsch–Mitzenmacher): `h1 = FNV-1a 64(term)`,
//!   `h2 = FNV-1a 64(term's bytes reversed)`, and probe `i` is
//!   `(h1 + i · h2) mod m_bits`.
//!
//! FNV-1a is implemented here rather than pulled from a hashing crate so the
//! hash stays pinned to this on-the-wire contract and never drifts with a
//! dependency bump.

use std::collections::HashSet;

use base64::Engine as _;
use xerj_common::hub::Sketch;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Number of probes per term. Fixed by the plan.
const K_HASHES: u32 = 7;

/// FNV-1a 64 over a byte sequence.
fn fnv1a64<'a, I: IntoIterator<Item = &'a u8>>(bytes: I) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// `(h1, h2)` for a term: FNV-1a 64 of its bytes, and of its bytes reversed.
fn hashes(term: &str) -> (u64, u64) {
    let h1 = fnv1a64(term.as_bytes());
    let h2 = fnv1a64(term.as_bytes().iter().rev());
    (h1, h2)
}

/// The `i`-th probe bit index for a term: `(h1 + i · h2) mod m_bits`.
/// `m_bits` is always non-zero here, so the modulo never divides by zero.
#[inline]
fn probe(h1: u64, h2: u64, i: u32, m_bits: u64) -> u64 {
    h1.wrapping_add(u64::from(i).wrapping_mul(h2)) % m_bits
}

/// Round `n` up to the next multiple of 64.
fn round_up_to_64(n: u64) -> u64 {
    n.div_ceil(64).saturating_mul(64)
}

/// Bits per distinct term. 16 bits with 7 probes is a false-positive rate of
/// about 0.07% per probe (`(1 - e^(-7/16))^7`): over a 513-corpus catalogue an
/// exact-symbol lookup sees ~0.4 false corpora, which the `--def` path's
/// five-candidate cap absorbs. The plan's first sizing (80 bits per term,
/// ~3e-8) cost 5× the bytes for no routing gain; see
/// `benchmarks/hub-routing-scale/RESULTS.md`.
pub const BITS_PER_TERM: u64 = 16;
/// Smallest filter, in bits.
pub const MIN_BITS: u64 = 1024;
/// Most terms one corpus's sketch holds (see [`select_terms`]). With
/// [`BITS_PER_TERM`] this caps a sketch at 3 KiB of bits (4 KiB of base64),
/// which is what keeps a ~513-corpus catalogue under the plan's 2 MB.
pub const MAX_TERMS: usize = 1500;

/// `m_bits` for `distinct` distinct terms: `max(MIN_BITS, BITS_PER_TERM ·
/// distinct)`, rounded up to a multiple of 64 (so it is always a whole number
/// of bytes).
fn sizing(distinct: u64) -> u64 {
    round_up_to_64(distinct.saturating_mul(BITS_PER_TERM).max(MIN_BITS))
}

/// Choose the terms a corpus's sketch holds, at most `max_terms` of them.
///
/// `raw` is everything the catalogue builder collected (definition symbols and
/// path components in their own case, plus description words); `curated` is
/// the corpus's own description (slug parts, `ask_for` and topic words).
///
/// Every term is lower-cased (the router and the `--def` lookup probe the
/// lower-case form), and each symbol / path component also contributes its
/// sub-words — split on `_`, `-`, `.` and camelCase, 3+ characters, starting
/// with a letter — so `compaction_picker` makes `compaction` routable and
/// `LZ4F_compressFrame` makes `compress` and `frame` routable. A term's weight
/// is how many distinct raw terms produce it (as themselves or as a sub-word):
/// the words a corpus is built from rank first.
///
/// Selection: every curated word, then the rest by weight (descending), then
/// shorter first, then alphabetical — deterministic, so the same inputs
/// produce the same sketch. A corpus with fewer distinct terms than the cap
/// keeps all of them; a larger one keeps its most characteristic vocabulary and
/// loses its rarest full symbols (an exact `--def` lookup of such a symbol then
/// needs the corpus named; routing the question's words is unaffected).
pub fn select_terms<I: IntoIterator<Item = String>>(
    raw: I,
    curated: &[String],
    max_terms: usize,
) -> Vec<String> {
    // Each distinct lower-case term, with its raw spellings (camelCase
    // boundaries need the original case).
    let mut spellings: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for term in raw {
        let lower = term.to_lowercase();
        let entry = spellings.entry(lower).or_default();
        if !entry.contains(&term) {
            entry.push(term);
        }
    }
    let mut weight: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (lower, raws) in &spellings {
        let mut produced: HashSet<String> = raws.iter().flat_map(|r| sub_words(r)).collect();
        produced.insert(lower.clone());
        for w in produced {
            *weight.entry(w).or_default() += 1;
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut curated_sorted: Vec<String> = curated.iter().map(|c| c.to_lowercase()).collect();
    curated_sorted.sort();
    for c in curated_sorted {
        if !c.is_empty() && taken.insert(c.clone()) {
            out.push(c);
        }
    }
    let mut ranked: Vec<(String, u32)> = weight
        .into_iter()
        .filter(|(w, _)| !w.is_empty() && !taken.contains(w))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.len().cmp(&b.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });
    for (w, _) in ranked {
        if out.len() >= max_terms {
            break;
        }
        out.push(w);
    }
    out
}

/// The sub-words of an identifier or path component: split on every
/// non-alphanumeric character and at camelCase boundaries (`fooBar`,
/// `HTTPServer` → `http`, `server`), lower-cased, 3+ characters, starting with
/// a letter. A term that is one word yields nothing extra.
fn sub_words(term: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in term.split(|c: char| !c.is_ascii_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut start = 0;
        for i in 1..=chars.len() {
            let boundary = i == chars.len()
                || (chars[i].is_ascii_uppercase()
                    && (chars[i - 1].is_ascii_lowercase()
                        || chars[i - 1].is_ascii_digit()
                        || (chars[i - 1].is_ascii_uppercase()
                            && chars.get(i + 1).is_some_and(|c| c.is_ascii_lowercase()))));
            if boundary {
                let word: String = chars[start..i].iter().collect();
                if word.len() >= 3 && word.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    out.push(word.to_ascii_lowercase());
                }
                start = i;
            }
        }
    }
    let whole = term.to_lowercase();
    out.retain(|w| *w != whole);
    out
}

/// The sketch the catalogue builder publishes for one corpus:
/// [`select_terms`] capped at [`MAX_TERMS`], then [`build`].
pub fn build_for_corpus<I: IntoIterator<Item = String>>(raw: I, curated: &[String]) -> Sketch {
    build(select_terms(raw, curated, MAX_TERMS))
}

/// Build a term sketch from an iterator of terms (distinct terms are what counts).
pub fn build<I: IntoIterator<Item = String>>(terms: I) -> Sketch {
    let distinct: HashSet<String> = terms.into_iter().collect();
    let m_bits = sizing(distinct.len() as u64);

    // m_bits is a multiple of 64, so this is exact and bit indices stay in bounds.
    let mut bits = vec![0u8; m_bits.div_ceil(8) as usize];
    for term in &distinct {
        let (h1, h2) = hashes(term);
        for i in 0..K_HASHES {
            let bit = probe(h1, h2, i, m_bits);
            bits[(bit / 8) as usize] |= 1u8 << (bit % 8);
        }
    }

    Sketch {
        m_bits: m_bits as u32,
        k_hashes: K_HASHES,
        bits_base64: base64::engine::general_purpose::STANDARD.encode(&bits),
    }
}

/// Is `term` possibly present? (Bloom: false is certain, true may be a false positive.)
pub fn contains(sketch: &Sketch, term: &str) -> bool {
    let m_bits = u64::from(sketch.m_bits);
    if m_bits == 0 {
        return false;
    }
    let Ok(bits) = base64::engine::general_purpose::STANDARD.decode(sketch.bits_base64.as_bytes())
    else {
        return false;
    };

    let (h1, h2) = hashes(term);
    for i in 0..sketch.k_hashes {
        let bit = probe(h1, h2, i, m_bits);
        let byte = (bit / 8) as usize;
        // A missing or unset bit means the term was certainly never inserted.
        if byte >= bits.len() || bits[byte] & (1u8 << (bit % 8)) == 0 {
            return false;
        }
    }
    true
}

/// A sketch with its bit array decoded once. [`contains`] base64-decodes the
/// whole filter on every probe, which is fine for one lookup but not for a
/// router that probes every question term against every corpus in a
/// several-hundred-entry catalogue; decode once with [`Decoded::new`] and probe
/// with [`Decoded::contains`].
pub struct Decoded {
    m_bits: u64,
    k_hashes: u32,
    bits: Vec<u8>,
}

impl Decoded {
    /// `None` when the sketch is unusable (zero bits, bad base64): such a
    /// sketch reports every term absent, exactly as [`contains`] does.
    pub fn new(sketch: &Sketch) -> Option<Self> {
        let m_bits = u64::from(sketch.m_bits);
        if m_bits == 0 {
            return None;
        }
        let bits = base64::engine::general_purpose::STANDARD
            .decode(sketch.bits_base64.as_bytes())
            .ok()?;
        Some(Self {
            m_bits,
            k_hashes: sketch.k_hashes,
            bits,
        })
    }

    /// Same answer as [`contains`] on the sketch this was decoded from.
    pub fn contains(&self, term: &str) -> bool {
        let (h1, h2) = hashes(term);
        for i in 0..self.k_hashes {
            let bit = probe(h1, h2, i, self.m_bits);
            let byte = (bit / 8) as usize;
            if byte >= self.bits.len() || self.bits[byte] & (1u8 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic PRNG so the "random" probe terms are reproducible
    /// in CI (no `rand` dependency). Seed is non-zero, as xorshift requires.
    fn xorshift64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn every_inserted_term_is_present() {
        let terms: Vec<String> = (0..1000).map(|i| format!("member_{i}")).collect();
        let sketch = build(terms.clone());
        for term in &terms {
            assert!(
                contains(&sketch, term),
                "inserted term {term} reported absent"
            );
        }
    }

    #[test]
    fn false_positive_rate_under_two_percent() {
        let terms: Vec<String> = (0..1000).map(|i| format!("member_{i}")).collect();
        let sketch = build(terms);

        // 16-char hex strings never contain '_', so none can equal a "member_i"
        // term — every probe below is a genuine non-member.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let trials = 10_000u32;
        let mut false_positives = 0u32;
        for _ in 0..trials {
            let candidate = format!("{:016x}", xorshift64(&mut state));
            if contains(&sketch, &candidate) {
                false_positives += 1;
            }
        }
        let rate = f64::from(false_positives) / f64::from(trials);
        // Visible with `--nocapture`; a Bloom filter sized at 8 · 10 bits per
        // term keeps this far below the 2% ceiling.
        eprintln!("sketch false-positive rate: {false_positives}/{trials} = {rate:.4}");
        assert!(
            rate < 0.02,
            "false-positive rate {rate} ({false_positives}/{trials}) exceeds 2%"
        );
    }

    #[test]
    fn deterministic_across_insertion_order() {
        let a = vec![
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
            "delta".to_string(),
            "epsilon".to_string(),
        ];
        let mut b = a.clone();
        b.reverse();
        b.rotate_left(2);

        let sa = build(a);
        let sb = build(b);
        assert_eq!(
            sa.bits_base64, sb.bits_base64,
            "order changed the bit array"
        );
        assert_eq!(sa.m_bits, sb.m_bits);
        assert_eq!(sa.k_hashes, sb.k_hashes);
    }

    #[test]
    fn duplicate_terms_do_not_change_the_sketch() {
        // Same distinct set, one copy heavily duplicated → identical sketch.
        let unique = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let mut dupes = unique.clone();
        for _ in 0..50 {
            dupes.extend(unique.clone());
        }
        assert_eq!(build(unique).bits_base64, build(dupes).bits_base64);
    }

    #[test]
    fn m_bits_is_multiple_of_64_and_at_least_the_floor() {
        for n in [0usize, 1, 103, 500, 2000] {
            let terms: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
            let sketch = build(terms);
            assert_eq!(
                sketch.m_bits % 64,
                0,
                "m_bits {} not a 64-multiple",
                sketch.m_bits
            );
            assert!(
                u64::from(sketch.m_bits) >= MIN_BITS,
                "m_bits {} below floor",
                sketch.m_bits
            );
            assert_eq!(sketch.k_hashes, 7);
            // bits_base64 decodes to exactly ceil(m_bits/8) bytes.
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(sketch.bits_base64.as_bytes())
                .expect("sketch holds valid base64");
            assert_eq!(bytes.len(), (sketch.m_bits as usize).div_ceil(8));
        }
    }

    #[test]
    fn empty_corpus_has_floor_sizing_and_absent_everything() {
        let sketch = build(Vec::<String>::new());
        assert_eq!(u64::from(sketch.m_bits), MIN_BITS);
        assert!(!contains(&sketch, "anything"));
    }

    #[test]
    fn large_corpus_uses_the_bits_per_term_branch() {
        // 2000 distinct: max(1024, 16 · 2000) = 32000, already a 64-multiple.
        let terms: Vec<String> = (0..2000).map(|i| format!("sym_{i}")).collect();
        let sketch = build(terms);
        assert_eq!(u64::from(sketch.m_bits), BITS_PER_TERM * 2000);
    }

    #[test]
    fn fnv1a64_matches_known_vectors() {
        // Canonical FNV-1a 64 test vectors.
        assert_eq!(fnv1a64(b"".iter()), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a".iter()), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar".iter()), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn sub_words_split_snake_camel_and_paths() {
        assert_eq!(sub_words("compaction_picker"), ["compaction", "picker"]);
        // `LZ4F` splits at the digit→letter boundary, dropping the 1-char `F`.
        assert_eq!(
            sub_words("LZ4F_compressFrame"),
            ["lz4", "compress", "frame"]
        );
        assert_eq!(sub_words("HTTPServer"), ["http", "server"]);
        // One word yields nothing beyond itself; short and digit-led parts drop.
        assert!(sub_words("zstd").is_empty());
        assert_eq!(sub_words("a_b_2x_window"), ["window"]);
    }

    #[test]
    fn select_terms_keeps_curated_lowercases_and_caps() {
        let raw: Vec<String> = ["LZ4_compress", "LZ4_decompress", "lz4frame", "Zebra_x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let curated = vec!["lz4".to_string(), "block".to_string()];
        let all = select_terms(raw.clone(), &curated, 100);
        for t in [
            "lz4",
            "block",
            "lz4_compress",
            "compress",
            "decompress",
            "lz4frame",
        ] {
            assert!(all.contains(&t.to_string()), "{t} in {all:?}");
        }
        assert!(all.iter().all(|t| *t == t.to_lowercase()), "{all:?}");

        // Capped: curated first, then the most-produced word ("lz4" is curated;
        // "compress"/"decompress"/… are produced once each, shortest first).
        let capped = select_terms(raw.clone(), &curated, 3);
        assert_eq!(capped, ["block", "lz4", "zebra"]);
        // Deterministic whatever the input order.
        let mut rev = raw;
        rev.reverse();
        assert_eq!(select_terms(rev, &curated, 3), capped);
    }

    #[test]
    fn a_corpus_sketch_never_exceeds_the_cap() {
        let raw: Vec<String> = (0..10_000).map(|i| format!("sym_{i}")).collect();
        let sketch = build_for_corpus(raw, &["mycorpus".to_string()]);
        assert!(u64::from(sketch.m_bits) <= MAX_TERMS as u64 * BITS_PER_TERM + 64);
        assert!(contains(&sketch, "mycorpus"));
        // "sym" is a sub-word of all 10,000 terms: the corpus's characteristic word.
        assert!(contains(&sketch, "sym"));
    }
}
