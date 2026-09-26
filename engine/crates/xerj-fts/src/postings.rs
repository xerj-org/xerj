//! Posting list encoding and decoding for xerj FTS.
//!
//! ## Format
//!
//! Posting lists are split into **128-doc blocks** (matching Lucene's default).
//! Within each block, doc IDs are delta-encoded and PFOR-packed using the
//! `bitpacking` crate for SIMD-accelerated bit-manipulation.
//!
//! Per-term layout written into the `.post` blob (see
//! [`PostingsWriter::encode_term`]):
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │  Block 0: packed doc_id deltas          │
//! │           packed term_freqs (positioned)│
//! │           positions (vbyte, positioned) │
//! ├─────────────────────────────────────────┤
//! │  Block 1 …                              │
//! ├─────────────────────────────────────────┤
//! │  Residual (< 128 docs, vbyte)           │
//! └─────────────────────────────────────────┘
//! ```
//!
//! ## Block framing: ZPS2 (current) and ZPS1 (legacy)
//!
//! The `.post` envelope magic doubles as the inner-framing version.  A `ZPS2`
//! file uses the framing below; a `ZPS1` file uses the older
//! `[num_bits ≥ 1][u32 byte_len][payload]` framing, which the reader still
//! decodes (see [`PostCodec`]).
//!
//! ZPS2 packed streams (doc-id deltas, term freqs) are frame-of-reference
//! coded per block — the same shape tantivy uses for sorted blocks (a vint
//! base plus bit-packed residuals; `postings/compression/mod.rs`
//! `uncompress_block_sorted` in tantivy @ 6182c6062) — with three extra rules
//! that remove bytes the old framing paid even when they carried no
//! information:
//!
//! ```text
//!   [width: u8]        0 = every lane equals `min`, no payload follows
//!                     1..=32 = bit width of (value − min) lanes
//!   [min: vbyte]       the frame's base, added back on decode
//!   [payload]          128 × width / 8 bytes — length DERIVED, never stored
//! ```
//!
//! An all-equal block (dense-field doc-id deltas of 1, a term whose freqs are
//! all k) costs 2–3 bytes total instead of the 21 the old framing paid.  The
//! old `u32 byte_len` was pure overhead: it is always `128 × width / 8`.
//!
//! The frame's `min` is subtracted when that narrows the width, and also on
//! narrow blocks (≤ [`ZPS2_FOR_MAX_EQUAL_WIDTH`] bits) where it normalises
//! level-shifted repeats into identical payloads for the outer zstd pass.
//! On WIDE equal-width blocks the subtraction only rewrites lanes zstd would
//! have matched verbatim, so those emit `min = 0` with raw lanes — see the
//! constant's doc comment for the measured story.
//!
//! ## Term-frequency elision
//!
//! A positioned term whose `total_term_frequency == doc_frequency` has, by
//! construction, `term_freq == 1` in every document (df positive ints
//! summing to df).  The writer omits the term's ENTIRE freq stream — full
//! blocks and residual vbytes alike — and the reader re-derives the same
//! predicate from the `.meta` record it already loaded (tantivy gates its
//! freq block on `term_has_freq`, `postings/serializer.rs::write_block`;
//! Lucene 50 elides per term when ttf == df).  Docs-only fields have always
//! done this via `store_positions = false`.
//!
//! Residual docs (< 128) at the end use variable-byte encoding.  In docs-only
//! mode (`store_positions = false`) the freq and position sub-blocks are
//! omitted and the reader synthesises `term_freq = 1`.
//!
//! The per-term `(doc_frequency, total_term_frequency, offset)` header lives in
//! the FST term dictionary / `.meta` file, **not** inline in the `.post` blob.
//!
//! ## Skip-list acceleration: NOT implemented
//!
//! There is **no on-disk skip table**.  [`PostingsWriter::encode_term`] computes
//! a [`SkipEntry`] vector in memory (one entry every [`SKIP_INTERVAL`] blocks)
//! but it is **never serialised** — the caller in `xerj-fts::index` discards it,
//! and the whole `.post` blob is wrapped in a Zstd envelope, so intra-blob byte
//! offsets would not be usable as seek targets anyway.  Consequently
//! [`PostingsReader::advance_to`] performs a **linear scan**, not a skip-list
//! seek.  Traversal is correct, just unaccelerated; the [`SkipEntry`] machinery
//! is scaffolding for a future block-skip implementation.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io::{self, Cursor};

/// Number of docs per PFOR block (must be a multiple of 128 for bitpacking).
pub const BLOCK_SIZE: usize = 128;

/// Cadence at which [`PostingsWriter::encode_term`] emits an in-memory
/// [`SkipEntry`].  NOTE: these entries are never persisted or consulted — see
/// the module docs' "Skip-list acceleration: NOT implemented" note.
pub const SKIP_INTERVAL: usize = 8; // every 1024 docs

// ── Term metadata ─────────────────────────────────────────────────────────────

/// Metadata stored in the term dictionary alongside the FST key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermPostings {
    /// Number of documents that contain this term (≡ Lucene's `docFreq`).
    pub doc_frequency: u32,
    /// Sum of term frequency across all documents (≡ Lucene's `totalTermFreq`).
    pub total_term_frequency: u64,
    /// Byte offset into the postings file where this term's data starts.
    pub postings_offset: u64,
    /// Byte length of the postings data for this term.
    pub postings_length: u32,
}

impl TermPostings {
    /// Serialise to a fixed-size 20-byte record.
    pub fn encode(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        let mut c = Cursor::new(buf.as_mut_slice());
        c.write_u32::<LittleEndian>(self.doc_frequency).unwrap();
        c.write_u64::<LittleEndian>(self.total_term_frequency)
            .unwrap();
        c.write_u64::<LittleEndian>(self.postings_offset).unwrap();
        // last 4 bytes: length — split from offset to stay 20 B
        // rewrite cleanly:
        let mut out = [0u8; 20];
        out[0..4].copy_from_slice(&self.doc_frequency.to_le_bytes());
        out[4..12].copy_from_slice(&self.total_term_frequency.to_le_bytes());
        out[12..20].copy_from_slice(&self.postings_offset.to_le_bytes());
        out
    }

    /// Parse from the 20-byte record produced by [`encode`].
    /// `postings_length` and `postings_offset` are stored separately in the
    /// FST value (offset only fits in 64 bits); length is stored here.
    pub fn decode(buf: &[u8]) -> io::Result<Self> {
        if buf.len() < 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "buffer too short for TermPostings",
            ));
        }
        let mut c = Cursor::new(buf);
        let doc_frequency = c.read_u32::<LittleEndian>()?;
        let total_term_frequency = c.read_u64::<LittleEndian>()?;
        let postings_offset = c.read_u64::<LittleEndian>()?;
        Ok(Self {
            doc_frequency,
            total_term_frequency,
            postings_offset,
            postings_length: 0, // filled by caller from separate source
        })
    }
}

// ── Skip table entry ──────────────────────────────────────────────────────────

/// A single would-be skip pointer into a term's block stream.
///
/// **Currently unused scaffolding.**  [`PostingsWriter::encode_term`] builds a
/// `Vec<SkipEntry>` but the value is discarded by its only caller and is never
/// written to the `.post` file, so no reader ever consults it.  It exists to
/// mark the intended shape of a future block-skip index; see the module docs.
#[derive(Debug, Clone)]
pub struct SkipEntry {
    /// Highest doc ID in the preceding block range.
    pub max_doc_id: u32,
    /// Byte offset of the first byte of the corresponding block.
    pub byte_offset: u32,
}

// ── PostingsWriter ────────────────────────────────────────────────────────────

/// Accumulated posting data for one term during index construction.
///
/// `positions` is empty (capacity 0, no heap allocation) when the writer
/// is configured with `store_positions = false` — the common case for
/// `keyword`, `ip`, numeric, and status-like log fields.  Skipping
/// positions here turns each posting from `32 + 24 + Vec<u32>` bytes
/// down to just `32 + 24` bytes of stack, and eliminates the per-occurrence
/// `Vec::push` that dominates ingest time for high-cardinality
/// repeating-value fields (e.g. nginx `method` with 27 000 occurrences
/// of `"GET"` per segment, each currently allocating a 1-element position
/// list into a growable Vec).
#[derive(Default)]
struct RawPosting {
    doc_id: u32,
    term_freq: u32,
    positions: Vec<u32>,
}

/// Accumulates postings for all terms in one field/segment, then serialises
/// them into the compact block format.
pub struct PostingsWriter {
    /// Per-term posting data, sorted by doc_id within each term.
    postings: std::collections::BTreeMap<String, Vec<RawPosting>>,
    /// When `false`, `add_occurrence` discards positions and `encode_term`
    /// writes a zero-length positions block per 128-doc block.  Saves ~60 %
    /// of `.post` bytes on keyword fields.
    store_positions: bool,
}

impl PostingsWriter {
    pub fn new() -> Self {
        Self {
            postings: std::collections::BTreeMap::new(),
            store_positions: true,
        }
    }

    /// Build a writer that omits positions.  Use for `keyword`, numeric,
    /// and other exact-match fields — they never answer phrase queries.
    pub fn new_no_positions() -> Self {
        Self {
            postings: std::collections::BTreeMap::new(),
            store_positions: false,
        }
    }

    pub fn store_positions(&self) -> bool {
        self.store_positions
    }

    /// Record one occurrence of `term` in `doc_id` at the given `position`.
    ///
    /// When the writer was constructed without positions, `position` is
    /// ignored and no heap growth happens in the `positions` Vec.
    pub fn add_occurrence(&mut self, term: &str, doc_id: u32, position: u32) {
        // Lookup-first to avoid the `term.to_owned()` String allocation
        // that `entry()` forced on EVERY occurrence (13M+ allocs per 1M
        // docs at flush time).  Misses (first occurrence of a term) pay
        // one extra BTreeMap descent — postings are Zipf-shaped, so hits
        // dominate.
        if let Some(list) = self.postings.get_mut(term) {
            if let Some(last) = list.last_mut() {
                if last.doc_id == doc_id {
                    last.term_freq += 1;
                    if self.store_positions {
                        last.positions.push(position);
                    }
                    return;
                }
            }
            list.push(RawPosting {
                doc_id,
                term_freq: 1,
                positions: if self.store_positions {
                    vec![position]
                } else {
                    Vec::new()
                },
            });
            return;
        }
        self.postings.insert(
            term.to_owned(),
            vec![RawPosting {
                doc_id,
                term_freq: 1,
                positions: if self.store_positions {
                    vec![position]
                } else {
                    Vec::new()
                },
            }],
        );
    }

    /// Append a whole run of already-decoded postings for `term`.
    ///
    /// This is the merge path's ingest entry point.  `add_occurrence` is the
    /// INDEXING entry point: it takes one token at a time because that is
    /// what an analyzer produces.  A merge has no analyzer — it has one
    /// [`DecodedPosting`] per (term, document) straight out of a source
    /// segment's posting list — so replaying it occurrence by occurrence
    /// would pay a `BTreeMap<String, _>` descent per TOKEN.  Taking the run
    /// in one call pays one descent per (term, source segment) instead:
    /// measured at roughly half the replay's CPU on a 100 MiB corpus, and it
    /// also moves each posting's position vector in rather than copying it.
    ///
    /// Positions are dropped when this writer omits them.  `term_freq` is
    /// stored as given — the `.post` format records the frequency and the
    /// position count as separate fields, and a merge must reproduce both
    /// exactly as the source segment had them.
    ///
    /// Two obligations on the caller, both of which a merge satisfies by
    /// construction:
    ///
    /// * a document may appear at most ONCE per term across every run (each
    ///   merged ordinal comes from exactly one source document, and a posting
    ///   list holds one entry per document), because this appends without
    ///   coalescing equal doc ids;
    /// * runs may arrive out of doc order — a merged segment interleaves its
    ///   inputs' documents — so [`Self::sort_postings_by_doc`] MUST run before
    ///   `encode_term`.
    pub fn extend_postings(&mut self, term: &str, run: Vec<DecodedPosting>) {
        if run.is_empty() {
            return;
        }
        let store_positions = self.store_positions;
        let convert = move |posting: DecodedPosting| RawPosting {
            doc_id: posting.doc_id,
            term_freq: posting.term_freq,
            positions: if store_positions {
                posting.positions
            } else {
                Vec::new()
            },
        };
        match self.postings.get_mut(term) {
            Some(list) => {
                list.reserve(run.len());
                list.extend(run.into_iter().map(convert));
            }
            None => {
                self.postings
                    .insert(term.to_owned(), run.into_iter().map(convert).collect());
            }
        }
    }

    /// Restore the ascending-doc-id invariant every posting list must hold
    /// before it is encoded.
    ///
    /// The indexing path never needs this: it walks documents in ordinal
    /// order, so each list is built ascending.  The merge path replays
    /// several source segments whose surviving documents interleave in the
    /// merged ordinal space, so a shared term's list arrives as k ascending
    /// runs.  `encode_block_doc_ids` delta-encodes with a plain `-`, which
    /// would underflow on an out-of-order pair.
    ///
    /// `sort_by_key` is the stable, run-detecting sort, so k concatenated
    /// ascending runs cost O(n log k), not O(n log n); lists that are already
    /// ordered are detected by the scan below and never sorted at all.
    pub fn sort_postings_by_doc(&mut self) {
        for list in self.postings.values_mut() {
            if list.windows(2).any(|pair| pair[0].doc_id > pair[1].doc_id) {
                list.sort_by_key(|posting| posting.doc_id);
            }
        }
    }

    /// Direct `(doc_frequency, total_term_frequency)` lookup for ONE term.
    ///
    /// O(log T + postings(term)) — use this in per-term loops.  The segment
    /// writer (`write_field_static`) previously resolved stats via
    /// `term_stats().find(term)`, which walks the term map from the start
    /// *and sums every earlier term's postings* on each call — O(T² × P)
    /// across the write loop.  Invisible on 625-doc flush segments, but a
    /// merged segment over a float field (≈1 distinct term per doc) spun
    /// 100 % CPU for HOURS at 3 M docs, pinning rayon workers and starving
    /// every search + flush behind it (the read-under-write collapse).
    pub fn stats_for(&self, term: &str) -> Option<(u32, u64)> {
        self.postings.get(term).map(|postings| {
            let doc_freq = postings.len() as u32;
            let ttf: u64 = postings.iter().map(|p| p.term_freq as u64).sum();
            (doc_freq, ttf)
        })
    }

    /// Returns an iterator over `(term, doc_frequency, total_term_frequency)`
    /// without consuming self (for building the FST term dictionary).
    pub fn term_stats(&self) -> impl Iterator<Item = (&str, u32, u64)> {
        self.postings.iter().map(|(term, postings)| {
            let doc_freq = postings.len() as u32;
            let ttf: u64 = postings.iter().map(|p| p.term_freq as u64).sum();
            (term.as_str(), doc_freq, ttf)
        })
    }

    /// Encode the posting list for `term` into `output`.
    ///
    /// Returns `(postings_offset, skip_table)` where `postings_offset` is
    /// the byte position of the encoded data within `output` at the time of
    /// writing (caller tracks the global byte offset).
    ///
    /// NOTE: the returned `skip_table` is **not** written into `output` and is
    /// currently discarded by the caller — only the packed blocks and residual
    /// are serialised.  See the module docs' "Skip-list acceleration: NOT
    /// implemented" note.
    pub fn encode_term(&self, term: &str, output: &mut Vec<u8>) -> Option<(u64, Vec<SkipEntry>)> {
        let postings = self.postings.get(term)?;

        let start_offset = output.len() as u64;
        let mut skip_table: Vec<SkipEntry> = Vec::new();
        let mut block_count = 0usize;
        let mut i = 0usize;
        let n = postings.len();
        // Term-frequency elision: df positive integers summing to df iff
        // every one of them is 1.  The reader re-derives this SAME predicate
        // from the `.meta` record (ttf == df), so the decision costs no
        // bytes on disk and the two sides cannot drift.
        let ttf: u64 = postings.iter().map(|p| p.term_freq as u64).sum();
        let freq_omitted = self.store_positions && ttf == n as u64;
        // Track the last doc_id written, so that delta[0] of the next block
        // is the gap from the previous block's final doc.  The reader
        // (`decode_next_full_block`) assumes that the first delta in each
        // block is relative to `last_doc_id`, not absolute.
        let mut prev_block_last_doc_id: u32 = 0;

        while i + BLOCK_SIZE <= n {
            // Full block
            let block = &postings[i..i + BLOCK_SIZE];

            if block_count.is_multiple_of(SKIP_INTERVAL) {
                skip_table.push(SkipEntry {
                    max_doc_id: block[BLOCK_SIZE - 1].doc_id,
                    byte_offset: (output.len() - start_offset as usize) as u32,
                });
            }

            encode_block_doc_ids(block, output, prev_block_last_doc_id);
            if self.store_positions && !freq_omitted {
                encode_block_freqs(block, output);
            }
            if self.store_positions {
                encode_block_positions(block, output);
            }
            // docs-only mode: emit nothing after the packed doc-id block.
            // The reader is told about the mode via the field meta and
            // skips the freq/position decode paths entirely.

            prev_block_last_doc_id = block[BLOCK_SIZE - 1].doc_id;
            i += BLOCK_SIZE;
            block_count += 1;
        }

        // Residual (< BLOCK_SIZE docs) — delta-encoded from the last full
        // block's final doc_id as well.
        if i < n {
            let residual = &postings[i..];
            encode_residual(
                residual,
                output,
                prev_block_last_doc_id,
                self.store_positions,
                freq_omitted,
            );
        }

        Some((start_offset, skip_table))
    }

    pub fn terms(&self) -> impl Iterator<Item = &str> {
        self.postings.keys().map(|s| s.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.postings.is_empty()
    }
}

impl Default for PostingsWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Block encoding helpers ────────────────────────────────────────────────────

/// Width at or below which a block always takes the frame-of-reference
/// form, even when subtracting `min` would not narrow the width.
///
/// The whole-file zstd envelope is the second compression stage, and the
/// two stages interact in opposite directions by block width (measured on
/// the 100k `benchmarks/index-size` harness, A/B against both always-FOR
/// and never-FOR-at-equal-width encoders):
///
/// * NARROW blocks (≤ 4 bits): the packed payload is a short, highly
///   regular bit pattern.  FOR normalises level-shifted repeats — freqs
///   5/6 and freqs 9/10 both pack to the same `[0,1,0,1,…]` lanes — so
///   equal payloads recur across blocks and zstd matches them exactly.
///   Skipping the subtraction at equal width cost `body.post` 16 KB.
/// * WIDE blocks: the lanes are quasi-random bits, so the only cross-block
///   repetition zstd can find is payload EQUALITY, which subtracting a
///   varying per-block `min` destroys.  FOR-at-equal-width inflated the
///   compressed `top_doc` field 12.7 % while its uncompressed stream
///   shrank — pure post-compression loss.
const ZPS2_FOR_MAX_EQUAL_WIDTH: u8 = 4;

/// Bit-pack a 128-element `u32` array into the output buffer in the ZPS2
/// framing: `[width: u8][min: vbyte][payload]` with the payload omitted
/// entirely when every lane equals `min` (width 0).  Frame-of-reference
/// subtracts `min` first, so the width tracks the block's SPREAD, not its
/// maximum — a block of freqs 100..=110 packs at 4 bits, not 7.
fn pack_u32_block_v2(values: &[u32; BLOCK_SIZE], output: &mut Vec<u8>) {
    use bitpacking::BitPacker;
    let min = *values.iter().min().unwrap_or(&0);
    let max = *values.iter().max().unwrap_or(&0);
    let spread = max - min;

    if spread == 0 {
        output.push(0u8);
        vbyte_encode(min, output);
        return;
    }

    let num_bits_framed = (32 - spread.leading_zeros()) as u8;
    let num_bits_raw = (32 - max.leading_zeros()) as u8; // 1..=32, max > 0 here
    let (min, num_bits) =
        if num_bits_framed < num_bits_raw || num_bits_raw <= ZPS2_FOR_MAX_EQUAL_WIDTH {
            (min, num_bits_framed)
        } else {
            (0, num_bits_raw)
        };
    output.push(num_bits);
    vbyte_encode(min, output);

    let mut lanes = [0u32; BLOCK_SIZE];
    for (lane, &value) in lanes.iter_mut().zip(values.iter()) {
        *lane = value - min;
    }
    // bitpacking::BitPacker4x::BLOCK_LEN == 128; byte_len is derived on read.
    let byte_len = BLOCK_SIZE * num_bits as usize / 8;
    let mut compressed = vec![0u8; byte_len];
    bitpacking::BitPacker4x::new().compress(&lanes, &mut compressed, num_bits);
    output.extend_from_slice(&compressed);
}

/// Decode a 128-element block previously written by `pack_u32_block_v2`.
/// Reads the cursor forward past the data and fills `out`.
fn unpack_u32_block_v2(
    data: &[u8],
    cursor: &mut Cursor<&[u8]>,
    out: &mut [u32; BLOCK_SIZE],
) -> io::Result<()> {
    use bitpacking::BitPacker;
    let num_bits = cursor.read_u8()?;
    let min = vbyte_decode(cursor)?;
    if num_bits == 0 {
        *out = [min; BLOCK_SIZE];
        return Ok(());
    }
    if num_bits > 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "postings block: invalid bit width",
        ));
    }
    let byte_len = BLOCK_SIZE * num_bits as usize / 8;
    let start = cursor.position() as usize;
    let end = start + byte_len;
    if end > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "postings data truncated",
        ));
    }
    bitpacking::BitPacker4x::new().decompress(&data[start..end], out, num_bits);
    for value in out.iter_mut() {
        *value = value.wrapping_add(min);
    }
    cursor.set_position(end as u64);
    Ok(())
}

/// Decode a 128-element block in the legacy ZPS1 framing
/// `[num_bits ≥ 1][u32 byte_len][payload]`.  Old segments stay readable.
fn unpack_u32_block_v1(
    data: &[u8],
    cursor: &mut Cursor<&[u8]>,
    out: &mut [u32; BLOCK_SIZE],
) -> io::Result<()> {
    use bitpacking::BitPacker;
    let num_bits = cursor.read_u8()?;
    let byte_len = cursor.read_u32::<LittleEndian>()? as usize;
    let start = cursor.position() as usize;
    let end = start + byte_len;
    if end > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "postings data truncated",
        ));
    }
    let compressed = &data[start..end];
    bitpacking::BitPacker4x::new().decompress(compressed, out, num_bits);
    cursor.set_position(end as u64);
    Ok(())
}

fn encode_block_doc_ids(block: &[RawPosting], output: &mut Vec<u8>, prev_last_doc_id: u32) {
    debug_assert_eq!(block.len(), BLOCK_SIZE);

    // Delta-encode doc IDs.  The first delta is relative to the previous
    // block's final doc_id (or 0 for the very first block) so the reader
    // can reconstruct absolute positions by running-sum across blocks.
    let mut deltas = [0u32; BLOCK_SIZE];
    deltas[0] = block[0].doc_id - prev_last_doc_id;
    for j in 1..BLOCK_SIZE {
        deltas[j] = block[j].doc_id - block[j - 1].doc_id;
    }

    pack_u32_block_v2(&deltas, output);
}

fn encode_block_freqs(block: &[RawPosting], output: &mut Vec<u8>) {
    debug_assert_eq!(block.len(), BLOCK_SIZE);

    let mut freqs = [0u32; BLOCK_SIZE];
    for (i, p) in block.iter().enumerate() {
        freqs[i] = p.term_freq;
    }

    pack_u32_block_v2(&freqs, output);
}

fn encode_block_positions(block: &[RawPosting], output: &mut Vec<u8>) {
    // Positions: variable-byte encode per-document, delta within document.
    // No length prefix — the reader decodes exactly BLOCK_SIZE documents'
    // (count + deltas) groups, which pins the stream's end; the u32 the old
    // framing spent per block was information the structure already had.
    for posting in block {
        vbyte_encode(posting.positions.len() as u32, output);
        let mut prev = 0u32;
        for &pos in &posting.positions {
            vbyte_encode(pos - prev, output);
            prev = pos;
        }
    }
}

fn encode_residual(
    residual: &[RawPosting],
    output: &mut Vec<u8>,
    prev_last_doc_id: u32,
    store_positions: bool,
    freq_omitted: bool,
) {
    // Mark as residual with a sentinel: count byte
    output.push(residual.len() as u8);

    // The residual continues the delta chain from the last full block.
    let mut prev_doc = prev_last_doc_id;
    for posting in residual {
        let delta = posting.doc_id - prev_doc;
        prev_doc = posting.doc_id;
        vbyte_encode(delta, output);

        // docs-only mode: skip freq and positions.  Reader synthesises
        // term_freq = 1 when decoding this field.  `freq_omitted` is the
        // positioned variant of the same elision — a term with ttf == df
        // has term_freq 1 everywhere, so there is nothing to store.
        if store_positions && !freq_omitted {
            vbyte_encode(posting.term_freq, output);
        }
        if store_positions {
            vbyte_encode(posting.positions.len() as u32, output);
            let mut prev_pos = 0u32;
            for &pos in &posting.positions {
                vbyte_encode(pos - prev_pos, output);
                prev_pos = pos;
            }
        }
    }
}

// ── Variable-byte encoding ────────────────────────────────────────────────────

/// Encodes `value` as a variable-byte integer into `buf`.
pub fn vbyte_encode(mut value: u32, buf: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte | 0x80); // high bit = last byte
            break;
        } else {
            buf.push(byte);
        }
    }
}

/// Decodes a variable-byte integer from `cursor`.
pub fn vbyte_decode(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
    let mut result = 0u32;
    let mut shift = 0u32;
    loop {
        let byte = cursor.read_u8()?;
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 != 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 35 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "vbyte overflow"));
        }
    }
}

/// Decode one document's position group — a vbyte count followed by that
/// many delta-encoded positions.
fn decode_position_group(cursor: &mut Cursor<&[u8]>) -> io::Result<Vec<u32>> {
    let count = vbyte_decode(cursor)? as usize;
    let mut positions = Vec::with_capacity(count);
    let mut prev = 0u32;
    for _ in 0..count {
        let delta = vbyte_decode(cursor)?;
        prev += delta;
        positions.push(prev);
    }
    Ok(positions)
}

// ── PostingsReader ────────────────────────────────────────────────────────────

/// Inner framing of a term's postings blob — set by the `.post` envelope
/// magic the segment was written with (see `xerj_fts::index`).
///
/// `V2` is the ZPS2 framing (frame-of-reference blocks, derived lengths,
/// width-0 constant blocks).  `freq_omitted` marks a positioned term whose
/// `ttf == df`, whose freq stream was therefore never written; the reader
/// synthesises `term_freq = 1`, exactly as docs-only fields always have.
///
/// `V1` is the legacy ZPS1 framing; segments written before the ZPS2 bump
/// decode through it unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostCodec {
    V1,
    V2 { freq_omitted: bool },
}

impl PostCodec {
    /// The codec decision a ZPS2 reader makes for one term from facts the
    /// `.meta` record already carries.  The writer applies the identical
    /// predicate at encode time, so the two cannot drift.
    pub fn v2_for(has_positions: bool, total_term_frequency: u64, doc_frequency: u32) -> Self {
        PostCodec::V2 {
            freq_omitted: has_positions && total_term_frequency == doc_frequency as u64,
        }
    }
}

/// Iterator over decoded doc IDs from a posting list.
pub struct PostingsReader<'a> {
    data: &'a [u8],
    cursor: Cursor<&'a [u8]>,
    /// Total doc count for this term (determines how many blocks to decode).
    doc_frequency: u32,
    /// Docs consumed so far.
    docs_read: u32,
    /// Current block's decoded doc IDs (up to BLOCK_SIZE).
    block_docs: Vec<u32>,
    /// Current block's decoded term freqs.
    block_freqs: Vec<u32>,
    /// Current block's decoded positions per doc.
    block_positions: Vec<Vec<u32>>,
    /// Index within the current block.
    block_idx: usize,
    /// Number of full blocks.
    num_full_blocks: usize,
    /// Full blocks processed so far.
    blocks_processed: usize,
    /// Whether we are in the residual section.
    in_residual: bool,
    /// Residual entries (decoded all at once).
    residual: Vec<DecodedPosting>,
    /// Index within the residual.
    residual_idx: usize,
    /// Last doc ID seen (for delta decoding across blocks).
    last_doc_id: u32,
    /// `true` when the posting list contains freq + position blocks.
    /// `false` for docs-only fields (keyword, numeric, ip), in which
    /// case the reader synthesises `term_freq = 1` and empty positions
    /// for every posting.
    has_positions: bool,
    /// Inner block framing (ZPS1 legacy vs ZPS2) + freq-elision marker.
    codec: PostCodec,
}

#[derive(Debug, Clone)]
pub struct DecodedPosting {
    pub doc_id: u32,
    pub term_freq: u32,
    pub positions: Vec<u32>,
}

impl<'a> PostingsReader<'a> {
    /// Create a reader over the raw posting bytes for one term.
    /// Defaults to the positioned format.  For docs-only fields use
    /// [`PostingsReader::new_with_positions`] with `has_positions =
    /// false` — the reader will skip freq / position decoding and
    /// synthesise `term_freq = 1` for every posting.
    pub fn new(data: &'a [u8], doc_frequency: u32) -> Self {
        Self::new_with_positions(data, doc_frequency, true)
    }

    /// Like [`new`] but explicit about whether the posting list carries
    /// term frequencies + positions.
    ///
    /// This constructs the **legacy ZPS1** reader.  Segments written with
    /// the ZPS2 envelope must go through [`Self::new_with_codec`] — the
    /// block framing differs — which is what `FtsIndexReader`'s postings
    /// factory does for every caller.
    pub fn new_with_positions(data: &'a [u8], doc_frequency: u32, has_positions: bool) -> Self {
        Self::new_with_codec(data, doc_frequency, has_positions, PostCodec::V1)
    }

    /// Like [`new_with_positions`] but explicit about the inner block
    /// framing, so a ZPS2 segment (FOR blocks, derived lengths, optional
    /// freq elision) and a legacy ZPS1 segment decode through the same
    /// iterator.
    pub fn new_with_codec(
        data: &'a [u8],
        doc_frequency: u32,
        has_positions: bool,
        codec: PostCodec,
    ) -> Self {
        let num_full_blocks = doc_frequency as usize / BLOCK_SIZE;
        Self {
            data,
            cursor: Cursor::new(data),
            doc_frequency,
            docs_read: 0,
            block_docs: Vec::new(),
            block_freqs: Vec::new(),
            block_positions: Vec::new(),
            block_idx: 0,
            num_full_blocks,
            blocks_processed: 0,
            in_residual: false,
            residual: Vec::new(),
            residual_idx: 0,
            last_doc_id: 0,
            has_positions,
            codec,
        }
    }

    /// Advance to the next posting. Returns `None` when exhausted.
    ///
    /// An inherent cursor method rather than an `Iterator` impl: decoding
    /// borrows the reader's internal buffers, which doesn't fit `Iterator`'s
    /// `Item` lifetime, so `next` stays inherent by design.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<DecodedPosting> {
        if self.docs_read >= self.doc_frequency {
            return None;
        }

        // Need to load the next block?
        if !self.in_residual && self.block_idx >= self.block_docs.len() {
            if self.blocks_processed < self.num_full_blocks {
                self.decode_next_full_block().ok()?;
            } else {
                // Switch to residual
                self.decode_residual().ok()?;
                self.in_residual = true;
            }
        }

        if self.in_residual {
            if self.residual_idx >= self.residual.len() {
                return None;
            }
            let posting = self.residual[self.residual_idx].clone();
            self.residual_idx += 1;
            self.docs_read += 1;
            return Some(posting);
        }

        let doc_id = self.block_docs[self.block_idx];
        let term_freq = self.block_freqs[self.block_idx];
        let positions = self.block_positions[self.block_idx].clone();
        self.block_idx += 1;
        self.docs_read += 1;

        Some(DecodedPosting {
            doc_id,
            term_freq,
            positions,
        })
    }

    fn decode_next_full_block(&mut self) -> io::Result<()> {
        // Read doc_id deltas
        let mut deltas = [0u32; BLOCK_SIZE];
        match self.codec {
            PostCodec::V1 => unpack_u32_block_v1(self.data, &mut self.cursor, &mut deltas)?,
            PostCodec::V2 { .. } => unpack_u32_block_v2(self.data, &mut self.cursor, &mut deltas)?,
        }

        // Reconstruct absolute doc IDs
        let mut doc_ids = vec![0u32; BLOCK_SIZE];
        doc_ids[0] = self.last_doc_id + deltas[0];
        for j in 1..BLOCK_SIZE {
            doc_ids[j] = doc_ids[j - 1] + deltas[j];
        }
        self.last_doc_id = doc_ids[BLOCK_SIZE - 1];

        // A positioned term whose freq stream was elided (ttf == df) or a
        // docs-only field: every frequency is 1 by construction, so the
        // block's freq bytes simply are not there.
        let freq_stored = self.has_positions && !self.freq_omitted();
        let (freqs, positions) = if self.has_positions {
            let freqs = if freq_stored {
                let mut freqs = [0u32; BLOCK_SIZE];
                match self.codec {
                    PostCodec::V1 => unpack_u32_block_v1(self.data, &mut self.cursor, &mut freqs)?,
                    PostCodec::V2 { .. } => {
                        unpack_u32_block_v2(self.data, &mut self.cursor, &mut freqs)?
                    }
                }
                freqs.to_vec()
            } else {
                vec![1u32; BLOCK_SIZE]
            };
            (freqs, self.decode_block_positions()?)
        } else {
            // Docs-only mode: synthesise freq=1 and empty positions.
            let freqs = vec![1u32; BLOCK_SIZE];
            let positions: Vec<Vec<u32>> = vec![Vec::new(); BLOCK_SIZE];
            (freqs, positions)
        };

        self.block_docs = doc_ids.to_vec();
        self.block_freqs = freqs;
        self.block_positions = positions;
        self.block_idx = 0;
        self.blocks_processed += 1;

        Ok(())
    }

    /// `true` when this term's freq stream was never written — either a
    /// docs-only field or (ZPS2) a positioned term with `ttf == df`.
    fn freq_omitted(&self) -> bool {
        match self.codec {
            PostCodec::V1 => false,
            PostCodec::V2 { freq_omitted } => freq_omitted || !self.has_positions,
        }
    }

    /// Decode one block's positions (BLOCK_SIZE documents' worth of
    /// `count + deltas` vbyte groups).
    ///
    /// ZPS1 prefixes the stream with a `u32` byte length and the groups
    /// decode from that slice; ZPS2 dropped the prefix — the group count is
    /// structurally exactly BLOCK_SIZE, so the groups decode straight off
    /// the main cursor and pin its end themselves.
    fn decode_block_positions(&mut self) -> io::Result<Vec<Vec<u32>>> {
        let mut positions: Vec<Vec<u32>> = Vec::with_capacity(BLOCK_SIZE);
        match self.codec {
            PostCodec::V1 => {
                let pos_byte_len = self.cursor.read_u32::<LittleEndian>()? as usize;
                let pos_start = self.cursor.position() as usize;
                let pos_end = pos_start + pos_byte_len;
                if pos_end > self.data.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "postings positions truncated",
                    ));
                }
                let mut pos_cursor = Cursor::new(&self.data[pos_start..pos_end]);
                for _ in 0..BLOCK_SIZE {
                    positions.push(decode_position_group(&mut pos_cursor)?);
                }
                self.cursor.set_position(pos_end as u64);
            }
            PostCodec::V2 { .. } => {
                for _ in 0..BLOCK_SIZE {
                    positions.push(decode_position_group(&mut self.cursor)?);
                }
            }
        }
        Ok(positions)
    }

    fn decode_residual(&mut self) -> io::Result<()> {
        let count = self.cursor.read_u8()? as usize;
        let mut result = Vec::with_capacity(count);
        let mut prev_doc = self.last_doc_id;

        for _ in 0..count {
            let doc_delta = vbyte_decode(&mut self.cursor)?;
            let doc_id = prev_doc + doc_delta;
            prev_doc = doc_id;

            let (term_freq, positions) = if self.has_positions {
                // An elided freq stream (ttf == df) means every posting's
                // frequency is 1 — the residual's freq vbytes were never
                // written either, mirroring `encode_residual`.
                let term_freq = if self.freq_omitted() {
                    1u32
                } else {
                    vbyte_decode(&mut self.cursor)?
                };
                let pos_count = vbyte_decode(&mut self.cursor)? as usize;
                let mut positions = Vec::with_capacity(pos_count);
                let mut prev_pos = 0u32;
                for _ in 0..pos_count {
                    let delta = vbyte_decode(&mut self.cursor)?;
                    prev_pos += delta;
                    positions.push(prev_pos);
                }
                (term_freq, positions)
            } else {
                (1u32, Vec::new())
            };

            result.push(DecodedPosting {
                doc_id,
                term_freq,
                positions,
            });
        }

        self.residual = result;
        self.residual_idx = 0;
        Ok(())
    }

    /// Advance to the first posting whose `doc_id >= target`.
    ///
    /// This is a **linear scan** — it calls [`Self::next`] until a doc_id at or
    /// past `target` is reached (or the list is exhausted).  There is no
    /// skip-list acceleration: the `.post` blob carries no on-disk skip table
    /// (see the module docs), so no block-level seek is possible.  Correct, but
    /// O(n) in the number of postings skipped.
    pub fn advance_to(&mut self, target: u32) -> Option<DecodedPosting> {
        loop {
            match self.next() {
                Some(p) if p.doc_id >= target => return Some(p),
                Some(_) => continue,
                None => return None,
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build_postings(pairs: &[(u32, u32, &[u32])]) -> (Vec<u8>, u32, u64) {
        let mut writer = PostingsWriter::new();
        for &(doc_id, _freq, positions) in pairs {
            for &pos in positions {
                writer.add_occurrence("test", doc_id, pos);
            }
        }
        let mut data = Vec::new();
        writer.encode_term("test", &mut data);
        let doc_freq = pairs.len() as u32;
        let ttf: u64 = pairs.iter().map(|&(_, freq, _)| freq as u64).sum();
        (data, doc_freq, ttf)
    }

    /// The reader a ZPS2 segment constructs for writer output: V2 framing
    /// with the freq-elision predicate the writer itself applied.
    fn v2_reader(data: &[u8], doc_freq: u32, ttf: u64) -> PostingsReader<'_> {
        PostingsReader::new_with_codec(data, doc_freq, true, PostCodec::v2_for(true, ttf, doc_freq))
    }

    #[test]
    fn roundtrip_small_posting_list() {
        let postings: Vec<(u32, u32, &[u32])> =
            vec![(1, 2, &[0, 5]), (3, 1, &[2]), (7, 3, &[0, 1, 2])];
        let (data, doc_freq, ttf) = build_postings(&postings);
        let mut reader = v2_reader(&data, doc_freq, ttf);

        let p = reader.next().unwrap();
        assert_eq!(p.doc_id, 1);
        assert_eq!(p.term_freq, 2);
        assert_eq!(p.positions, vec![0, 5]);

        let p = reader.next().unwrap();
        assert_eq!(p.doc_id, 3);

        let p = reader.next().unwrap();
        assert_eq!(p.doc_id, 7);
        assert_eq!(p.term_freq, 3);

        assert!(reader.next().is_none());
    }

    #[test]
    fn roundtrip_full_block() {
        // Create exactly 128 docs
        let mut writer = PostingsWriter::new();
        for i in 0u32..128 {
            writer.add_occurrence("term", i * 2, i); // even doc ids, one position each
        }
        let mut data = Vec::new();
        writer.encode_term("term", &mut data);

        // Every frequency is 1 => ttf == df => the freq stream is elided and
        // this exercises the synthesised-freq path on a full block.
        let mut reader = v2_reader(&data, 128, 128);
        let mut last_doc = u32::MAX;
        let mut count = 0u32;
        while let Some(p) = reader.next() {
            assert!(
                p.doc_id < last_doc || last_doc == u32::MAX || p.doc_id > last_doc,
                "doc IDs must be monotonically increasing"
            );
            if last_doc != u32::MAX {
                assert!(p.doc_id > last_doc);
            }
            assert_eq!(p.term_freq, 1, "elided freqs decode as 1");
            assert_eq!(p.positions, vec![count], "positions still round-trip");
            last_doc = p.doc_id;
            count += 1;
        }
        assert_eq!(count, 128);
    }

    #[test]
    fn vbyte_roundtrip() {
        for &v in &[0u32, 1, 127, 128, 255, 16383, 16384, u32::MAX / 2] {
            let mut buf = Vec::new();
            vbyte_encode(v, &mut buf);
            let decoded = vbyte_decode(&mut Cursor::new(buf.as_slice())).unwrap();
            assert_eq!(decoded, v, "vbyte failed for {}", v);
        }
    }

    /// `advance_to` is a *linear* scan (there is no on-disk skip table — see
    /// the module docs).  This asserts it still lands on the correct posting:
    /// the first doc_id at or past the target, across a block boundary, and
    /// that it returns `None` once the list is exhausted.
    #[test]
    fn advance_to_linear_scans_correctly() {
        // 200 docs => one full 128-doc block + a 72-doc residual.
        // doc_id = i * 3, so ids are 0, 3, 6, … 597.
        let mut writer = PostingsWriter::new();
        for i in 0u32..200 {
            writer.add_occurrence("term", i * 3, i);
        }
        let mut data = Vec::new();
        writer.encode_term("term", &mut data);

        // Target lands exactly on a doc (150 = 50 * 3), inside the first block.
        let mut reader = v2_reader(&data, 200, 200);
        let hit = reader.advance_to(150).expect("expected a hit at/after 150");
        assert_eq!(hit.doc_id, 150, "must return the exact match when present");

        // Target between two docs (301 is not a multiple of 3) crossing into
        // the residual section => first doc strictly greater is 303.
        let mut reader = v2_reader(&data, 200, 200);
        let hit = reader.advance_to(301).expect("expected a hit at/after 301");
        assert_eq!(hit.doc_id, 303, "must return the first doc_id >= target");

        // Target past the end yields None.
        let mut reader = v2_reader(&data, 200, 200);
        assert!(
            reader.advance_to(600).is_none(),
            "advance past the last doc must exhaust the reader"
        );
    }

    /// The writer always emits the ZPS2 framing now; a hand-built legacy
    /// ZPS1 blob must still decode through `PostCodec::V1`.  The fixture is
    /// one docs-only full block whose lanes are [0, 1, 1, …] — the first
    /// delta is the gap from the previous block's last doc (0), so the
    /// lanes reconstruct docs 0..=127.
    #[test]
    fn zps1_legacy_block_bytes_still_decode() {
        let mut data: Vec<u8> = Vec::new();
        data.push(1u8); // num_bits = 1
        data.extend_from_slice(&16u32.to_le_bytes()); // byte_len = 128 * 1 / 8
                                                      // 1-bit lanes: lane 0 = 0, lanes 1..=127 = 1 => 0xFE then 0xFF × 15.
        data.push(0xFE);
        data.extend(std::iter::repeat_n(0xFFu8, 15));
        // No residual: df is an exact multiple of BLOCK_SIZE.

        let mut reader = PostingsReader::new_with_positions(&data, 128, false);
        let mut expected = 0u32;
        let mut count = 0u32;
        while let Some(p) = reader.next() {
            assert_eq!(
                p.doc_id, expected,
                "contiguous deltas of 1 reconstruct 0..127"
            );
            assert_eq!(p.term_freq, 1, "docs-only synthesises freq 1");
            expected += 1;
            count += 1;
        }
        assert_eq!(count, 128);
    }

    /// A ZPS2 constant block — dense doc IDs, so every delta is 1 — encodes
    /// to `[width 0][vbyte 1]` and nothing else, where ZPS1 paid
    /// 5 header/payload-formality bytes plus a 16-byte payload.
    #[test]
    fn zps2_constant_block_collapses_to_width_zero() {
        let mut writer = PostingsWriter::new();
        for i in 1u32..=128 {
            writer.add_occurrence("term", i, i);
        }
        let mut data = Vec::new();
        writer.encode_term("term", &mut data);

        assert_eq!(&data[..2], &[0u8, 0x81], "width-0 block then vbyte(1)");
        // Positions (128 groups of count=1, delta=i+1) dominate what is left;
        // the doc-id stream itself is over after two bytes.
        assert!(
            data.len() < 4 * 128,
            "whole blob must stay well under the old framing's ~21 B/doc block"
        );

        let mut reader = v2_reader(&data, 128, 128);
        for expected in 1u32..=128 {
            let p = reader.next().expect("128 postings");
            assert_eq!(p.doc_id, expected);
        }
        assert!(reader.next().is_none());
    }

    /// Frame-of-reference packs the block's SPREAD, not its maximum: a freq
    /// stream of 100..=101 needs 7 bits raw, 1 bit after subtracting min.
    #[test]
    fn zps2_for_packs_spread_not_max() {
        let mut writer = PostingsWriter::new();
        for i in 0u32..128 {
            // doc i gets freq alternating 100/101 via that many occurrences
            // of a distinct position count: freq = 100 + (i % 2).
            let freq = 100 + (i % 2);
            for pos in 0u32..freq {
                writer.add_occurrence("term", i * 10, pos);
            }
        }
        let mut data = Vec::new();
        writer.encode_term("term", &mut data);

        // Doc-id deltas: first 10 (0-0), then 10 repeatedly => constant 10
        // after the first lane… all lanes are 10 except lane 0 which is
        // doc 0 => delta 0? No: block[0].doc_id - 0 = 0, then +10 each.
        // spread = 10 => width 4.  Freq lanes: min 100, spread 1 => width 1.
        // Walk: [width][vbyte min][payload] for docids then freqs.
        let mut cursor = Cursor::new(data.as_slice());
        let docid_width = cursor.read_u8().unwrap();
        let docid_min = vbyte_decode(&mut cursor).unwrap();
        assert_eq!((docid_width, docid_min), (4u8, 0u32), "docid lanes 0..=10");
        cursor.set_position(cursor.position() + 128 * 4 / 8);

        let freq_width = cursor.read_u8().unwrap();
        let freq_min = vbyte_decode(&mut cursor).unwrap();
        assert_eq!((freq_width, freq_min), (1u8, 100u32), "freq lanes 100/101");

        let ttf: u64 = (0..128u32).map(|i| (100 + (i % 2)) as u64).sum();
        let df = 128u32;
        let mut reader = v2_reader(&data, df, ttf);
        for i in 0u32..128 {
            let p = reader.next().expect("128 postings");
            assert_eq!(p.doc_id, i * 10);
            assert_eq!(p.term_freq, 100 + (i % 2));
            assert_eq!(p.positions.len(), (100 + (i % 2)) as usize);
        }
        assert!(reader.next().is_none());
    }

    /// A WIDE block whose min and max share a binade gains no width from the
    /// frame subtraction — deltas [1, 100, 100, …] need 7 bits either way —
    /// so the encoder must emit `min = 0` with raw lanes, keeping the
    /// payload bytes identical to what ZPS1 carried (the outer zstd pass
    /// matches them across blocks; FOR-always measurably inflated the
    /// compressed `top_doc` field on the 100k harness).
    #[test]
    fn zps2_skips_for_on_wide_equal_width_blocks() {
        let mut writer = PostingsWriter::new();
        // doc 1 then gaps of 100: lanes [1, 100, 100, …], min 1, max 100 —
        // 7 bits framed (spread 99) and 7 bits raw, and 7 > the narrow cap.
        let mut doc = 1u32;
        for i in 0u32..128 {
            writer.add_occurrence("term", doc, i);
            doc += 100;
        }
        let mut data = Vec::new();
        writer.encode_term("term", &mut data);

        // [width 7][vbyte 0] — NOT [width 7][vbyte 1]: same width either
        // way and too wide for FOR's pattern-normalisation to pay, so the
        // raw lanes are kept for zstd to match.
        assert_eq!(&data[..2], &[7u8, 0x80], "wide equal-width block stays raw");

        let mut reader = v2_reader(&data, 128, 128);
        let mut expected = 1u32;
        for _ in 0..128 {
            let p = reader.next().expect("128 postings");
            assert_eq!(p.doc_id, expected);
            expected += 100;
        }
        assert!(reader.next().is_none());
    }

    /// The NARROW counterpart: an equal-width block at or below the cap
    /// takes the frame even though the width is unchanged, because FOR
    /// normalises level-shifted repeats into identical payloads.  Deltas
    /// [1, 15, 15, …] are 4 bits framed or raw — the frame is chosen.
    #[test]
    fn zps2_frames_narrow_equal_width_blocks() {
        let mut writer = PostingsWriter::new();
        let mut doc = 1u32;
        for i in 0u32..128 {
            writer.add_occurrence("term", doc, i);
            doc += 15;
        }
        let mut data = Vec::new();
        writer.encode_term("term", &mut data);

        assert_eq!(
            &data[..2],
            &[4u8, 0x81],
            "narrow equal-width block takes the frame (min = 1)"
        );

        let mut reader = v2_reader(&data, 128, 128);
        let mut expected = 1u32;
        for _ in 0..128 {
            let p = reader.next().expect("128 postings");
            assert_eq!(p.doc_id, expected);
            expected += 15;
        }
        assert!(reader.next().is_none());
    }

    /// Two terms over the same documents: one all freq 1 (ttf == df), one
    /// with a single freq-2 document.  The elided term's blob must be
    /// strictly smaller and both must round-trip with exact term_freqs.
    #[test]
    fn freq_elision_shrinks_and_roundtrips() {
        const ONE_POSITION: &[u32] = &[0];
        const TWO_POSITIONS: &[u32] = &[0, 1];
        let elided: Vec<(u32, u32, &[u32])> = (0u32..300).map(|i| (i, 1, ONE_POSITION)).collect();
        let mut repeated = elided.clone();
        repeated[5] = (5, 2, TWO_POSITIONS);

        let (data_elided, df_e, ttf_e) = build_postings(&elided);
        let (data_repeated, df_r, ttf_r) = build_postings(&repeated);
        assert_eq!((df_e, ttf_e), (300, 300));
        assert_eq!((df_r, ttf_r), (300, 301));
        assert!(
            data_elided.len() < data_repeated.len(),
            "elided freq stream must save bytes ({} vs {})",
            data_elided.len(),
            data_repeated.len()
        );

        let mut reader = v2_reader(&data_elided, df_e, ttf_e);
        for i in 0u32..300 {
            let p = reader.next().expect("300 postings");
            assert_eq!(p.term_freq, 1, "elided freqs synthesise 1");
            assert_eq!(p.doc_id, i);
        }
        assert!(reader.next().is_none());

        let mut reader = v2_reader(&data_repeated, df_r, ttf_r);
        for i in 0u32..300 {
            let p = reader.next().expect("300 postings");
            assert_eq!(p.term_freq, if i == 5 { 2 } else { 1 });
        }
        assert!(reader.next().is_none());
    }
}
