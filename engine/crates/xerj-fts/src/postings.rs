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
            if self.store_positions {
                encode_block_freqs(block, output);
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

/// Bit-pack a 128-element `u32` array into the output buffer.
/// Writes: [num_bits: u8][byte_len: u32][compressed bytes...]
fn pack_u32_block(values: &[u32; BLOCK_SIZE], output: &mut Vec<u8>) {
    use bitpacking::BitPacker;
    let max_val = *values.iter().max().unwrap_or(&0);
    let num_bits = if max_val == 0 {
        1u8
    } else {
        (32 - max_val.leading_zeros()) as u8
    };
    let num_bits = num_bits.min(32);

    // bitpacking::BitPacker4x::BLOCK_LEN == 128
    let byte_len = (BLOCK_SIZE * num_bits as usize).div_ceil(8);
    let mut compressed = vec![0u8; byte_len];

    let packer = bitpacking::BitPacker4x::new();
    packer.compress(values, &mut compressed, num_bits);

    output.push(num_bits);
    output.write_u32::<LittleEndian>(byte_len as u32).unwrap();
    output.extend_from_slice(&compressed);
}

/// Decode a 128-element block previously written by `pack_u32_block`.
/// Reads cursor forward past the data and fills `out`.
fn unpack_u32_block(
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

    pack_u32_block(&deltas, output);
}

fn encode_block_freqs(block: &[RawPosting], output: &mut Vec<u8>) {
    debug_assert_eq!(block.len(), BLOCK_SIZE);

    let mut freqs = [0u32; BLOCK_SIZE];
    for (i, p) in block.iter().enumerate() {
        freqs[i] = p.term_freq;
    }

    pack_u32_block(&freqs, output);
}

fn encode_block_positions(block: &[RawPosting], output: &mut Vec<u8>) {
    // Positions: variable-byte encode per-document, delta within document
    let mut pos_buf: Vec<u8> = Vec::new();
    for posting in block {
        vbyte_encode(posting.positions.len() as u32, &mut pos_buf);
        let mut prev = 0u32;
        for &pos in &posting.positions {
            vbyte_encode(pos - prev, &mut pos_buf);
            prev = pos;
        }
    }
    output
        .write_u32::<LittleEndian>(pos_buf.len() as u32)
        .unwrap();
    output.extend_from_slice(&pos_buf);
}

fn encode_residual(
    residual: &[RawPosting],
    output: &mut Vec<u8>,
    prev_last_doc_id: u32,
    store_positions: bool,
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
        // term_freq = 1 when decoding this field.
        if store_positions {
            vbyte_encode(posting.term_freq, output);
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

// ── PostingsReader ────────────────────────────────────────────────────────────

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
    pub fn new_with_positions(data: &'a [u8], doc_frequency: u32, has_positions: bool) -> Self {
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
        unpack_u32_block(self.data, &mut self.cursor, &mut deltas)?;

        // Reconstruct absolute doc IDs
        let mut doc_ids = vec![0u32; BLOCK_SIZE];
        doc_ids[0] = self.last_doc_id + deltas[0];
        for j in 1..BLOCK_SIZE {
            doc_ids[j] = doc_ids[j - 1] + deltas[j];
        }
        self.last_doc_id = doc_ids[BLOCK_SIZE - 1];

        let (freqs, positions) = if self.has_positions {
            // Read freqs
            let mut freqs = [0u32; BLOCK_SIZE];
            unpack_u32_block(self.data, &mut self.cursor, &mut freqs)?;

            // Read positions
            let pos_byte_len = self.cursor.read_u32::<LittleEndian>()? as usize;
            let pos_start = self.cursor.position() as usize;
            let pos_end = pos_start + pos_byte_len;
            let pos_data = &self.data[pos_start..pos_end];
            let mut pos_cursor = Cursor::new(pos_data);
            let mut positions: Vec<Vec<u32>> = Vec::with_capacity(BLOCK_SIZE);
            for _ in 0..BLOCK_SIZE {
                let count = vbyte_decode(&mut pos_cursor)? as usize;
                let mut poss = Vec::with_capacity(count);
                let mut prev = 0u32;
                for _ in 0..count {
                    let delta = vbyte_decode(&mut pos_cursor)?;
                    prev += delta;
                    poss.push(prev);
                }
                positions.push(poss);
            }
            self.cursor.set_position(pos_end as u64);
            (freqs.to_vec(), positions)
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

    fn decode_residual(&mut self) -> io::Result<()> {
        let count = self.cursor.read_u8()? as usize;
        let mut result = Vec::with_capacity(count);
        let mut prev_doc = self.last_doc_id;

        for _ in 0..count {
            let doc_delta = vbyte_decode(&mut self.cursor)?;
            let doc_id = prev_doc + doc_delta;
            prev_doc = doc_id;

            let (term_freq, positions) = if self.has_positions {
                let term_freq = vbyte_decode(&mut self.cursor)?;
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

    fn build_postings(pairs: &[(u32, u32, &[u32])]) -> (Vec<u8>, u32) {
        let mut writer = PostingsWriter::new();
        for &(doc_id, _freq, positions) in pairs {
            for &pos in positions {
                writer.add_occurrence("test", doc_id, pos);
            }
        }
        let mut data = Vec::new();
        writer.encode_term("test", &mut data);
        let doc_freq = pairs.len() as u32;
        (data, doc_freq)
    }

    #[test]
    fn roundtrip_small_posting_list() {
        let postings: Vec<(u32, u32, &[u32])> =
            vec![(1, 2, &[0, 5]), (3, 1, &[2]), (7, 3, &[0, 1, 2])];
        let (data, doc_freq) = build_postings(&postings);
        let mut reader = PostingsReader::new(&data, doc_freq);

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

        let mut reader = PostingsReader::new(&data, 128);
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
        let mut reader = PostingsReader::new(&data, 200);
        let hit = reader.advance_to(150).expect("expected a hit at/after 150");
        assert_eq!(hit.doc_id, 150, "must return the exact match when present");

        // Target between two docs (301 is not a multiple of 3) crossing into
        // the residual section => first doc strictly greater is 303.
        let mut reader = PostingsReader::new(&data, 200);
        let hit = reader.advance_to(301).expect("expected a hit at/after 301");
        assert_eq!(hit.doc_id, 303, "must return the first doc_id >= target");

        // Target past the end yields None.
        let mut reader = PostingsReader::new(&data, 200);
        assert!(
            reader.advance_to(600).is_none(),
            "advance past the last doc must exhaust the reader"
        );
    }
}
