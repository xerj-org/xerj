//! On-disk doc-values (column store) — one column per indexed field.
//!
//! Ported from the Lucene 9.0 `Lucene90DocValuesFormat` design but
//! **deliberately simple** in this first cut so it lands fast and is easy
//! to verify against `aggs::run_terms` / `aggs::run_stats`.  Block-level
//! bit-packing landed with the ZNV2 numeric layout (see `NumericColumn`);
//! global-ordinal sharing across segments is still future.
//!
//! ## What we store
//!
//! Two column kinds:
//!
//! 1. **`NumericColumn`** — a per-doc `Option<i64>` array.  The natural
//!    home for `Long`, `Integer`, `Date`, and `Boolean` (as 0/1).
//!    `Double`/`Float` are bit-cast to `i64` so we don't need a second
//!    code path; readers can `f64::from_bits` if they care.
//!
//! 2. **`KeywordColumn`** — a per-doc `Option<u32>` ordinal pointing into
//!    a sorted terms dictionary.  The dictionary is stored as an `fst::Map`
//!    so lookups are O(log |terms|) and the cardinality footprint is
//!    near-Lucene.
//!
//! Both columns track a `RoaringBitmap` of "doc has no value" so a missing
//! field doesn't waste a slot.
//!
//! ## Why these specific shapes
//!
//! - For `aggs::run_stats(field=bytes)` we just need to walk the numeric
//!   column once: `count`, `sum`, `min`, `max`, `avg`.  No JSON parsing.
//! - For `aggs::run_terms(field=method)` we just need to walk the keyword
//!   ordinals and increment a per-ordinal counter.  No string compares.
//!
//! Both replace the current "deserialise the entire stored section JSON,
//! evaluate `doc_matches_query`, accumulate sources" path that's measured
//! at 1 100 ms / million docs.  Expected post-G2: ~10–50 ms / million.
//!
//! ## Wire format (per-segment `Columns` section)
//!
//! All fields little-endian.
//!
//! ```text
//!     u32   magic = 0x44_56_30_31  ("DV01")
//!     u32   num_columns
//!     for each column:
//!         u8    kind (0 = numeric, 1 = keyword; 0x80 bit = zstd payload,
//!                    clear = legacy LZ4 — see KIND_FLAG_ZSTD)
//!         u32   field_name_len
//!         bytes field_name
//!         u64   payload_len
//!         bytes payload  (zstd when the 0x80 bit is set, else LZ4)
//! ```
//!
//! Numeric payload format (uncompressed, `ZNV2` — full layout on
//! `NumericColumn::encode`; `ZNV1` and the pre-magic legacy layout remain
//! readable):
//!
//! ```text
//!     "ZNV2"
//!     u32   doc_count
//!     bytes null_bitmap_serialized (length-prefixed roaring)
//!     u64   u_min, u64 gcd       // global frame-of-reference + GCD
//!     per 128-doc block: u8 width, varint block_min, packed residuals
//!     // width 0 = constant block, 1..=32 = u32 bit-packed lanes,
//!     // 0xFF = wide (raw u64 lanes) for spreads that exceed 32 bits
//! ```
//!
//! Keyword payload format (uncompressed):
//!
//! ```text
//!     u32   doc_count
//!     u32   num_terms
//!     u32   ord_width (1, 2, or 4 bytes)
//!     bytes null_bitmap_serialized
//!     u32   fst_bytes_len
//!     bytes fst_bytes  // sorted terms → ordinal
//!     for each doc i:
//!         <ord_width> bytes  // the ordinal
//! ```

use crate::{Result, StorageError};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fst::{Map as FstMap, MapBuilder};
use roaring::RoaringBitmap;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

const MAGIC: u32 = 0x44_56_30_31; // "DV01"
const KIND_NUMERIC: u8 = 0;
const KIND_KEYWORD: u8 = 1;

/// Per-doc numeric column.  `None` = no value for that doc.
///
/// Also carries a **1-D BKD-style sorted index** (`sorted`) that pairs each
/// live value with its doc-id, sorted by the raw i64 (which is the f64 bit
/// pattern in M2 — see `index::build_doc_value_columns`).  Range queries
/// bisect this index in O(log n) instead of scanning the full column.
///
/// Also carries pre-computed **column statistics** (`live_count`, `sum`,
/// `min`, `max`) built at `from_iter` / `decode` time so
/// `stats`/`sum`/`avg`/`min`/`max` aggs can return in O(1) per segment.
#[derive(Debug, Clone)]
pub struct NumericColumn {
    pub doc_count: u32,
    pub null_bitmap: RoaringBitmap,
    /// `data[doc_id]` is the value when `null_bitmap` doesn't contain
    /// `doc_id`, otherwise undefined (zero in practice).
    pub data: Vec<i64>,
    /// Sorted `(value, doc_id)` pairs for all live docs.  Sorted by
    /// `f64::from_bits(v as u64)` ascending.  Built lazily at `from_iter`
    /// time and stored on disk so queries pay only O(log n) lookups.
    pub sorted: Vec<(i64, u32)>,
    /// Number of live (non-null) docs. O(1) for value_count.
    pub live_count: u64,
    /// Sum of all live values as f64.  Computed at build/decode time.
    pub live_sum: f64,
    /// Min / max of live values.  Recoverable from `sorted` but kept
    /// explicit for O(1) agg without a vec load.
    pub live_min: f64,
    pub live_max: f64,
}

impl NumericColumn {
    // Named `from_iter` deliberately; it is not the FromIterator trait method
    // (returns Self by value from an Option-yielding iterator). Renaming would
    // break all callers, so the trait-confusion lint is allowed here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_iter<I: IntoIterator<Item = Option<i64>>>(it: I) -> Self {
        let mut null_bitmap = RoaringBitmap::new();
        let mut data = Vec::new();
        for (i, v) in it.into_iter().enumerate() {
            match v {
                Some(n) => data.push(n),
                None => {
                    null_bitmap.insert(i as u32);
                    data.push(0);
                }
            }
        }
        let sorted = build_sorted_index(&data, &null_bitmap);
        let (live_count, live_sum, live_min, live_max) = compute_stats(&sorted);
        Self {
            doc_count: data.len() as u32,
            null_bitmap,
            data,
            sorted,
            live_count,
            live_sum,
            live_min,
            live_max,
        }
    }

    pub fn get(&self, doc_id: u32) -> Option<i64> {
        if self.null_bitmap.contains(doc_id) {
            return None;
        }
        self.data.get(doc_id as usize).copied()
    }

    /// Return doc-ids whose (f64-interpreted) value falls in `[min, max]`
    /// with the given inclusivity flags.  O(log n) bisect + O(k) scan over
    /// the matching slice, no full-column walk.
    pub fn range_doc_ids(
        &self,
        min: f64,
        max: f64,
        min_inclusive: bool,
        max_inclusive: bool,
    ) -> Vec<u32> {
        if self.sorted.is_empty() {
            return Vec::new();
        }
        // Binary search by interpreting the stored i64 as f64 bits.
        let lo_idx = if min_inclusive {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) < min)
        } else {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) <= min)
        };
        let hi_idx = if max_inclusive {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) <= max)
        } else {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) < max)
        };
        if lo_idx >= hi_idx {
            return Vec::new();
        }
        self.sorted[lo_idx..hi_idx]
            .iter()
            .map(|(_, d)| *d)
            .collect()
    }

    /// Count-only variant of `range_doc_ids` — returns just the number of
    /// matching docs without allocating a Vec.
    pub fn range_count(&self, min: f64, max: f64, min_inclusive: bool, max_inclusive: bool) -> u64 {
        if self.sorted.is_empty() {
            return 0;
        }
        let lo_idx = if min_inclusive {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) < min)
        } else {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) <= min)
        };
        let hi_idx = if max_inclusive {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) <= max)
        } else {
            self.sorted
                .partition_point(|(v, _)| f64::from_bits(*v as u64) < max)
        };
        (hi_idx.saturating_sub(lo_idx)) as u64
    }

    /// ## Wire format history
    ///
    /// - **legacy** (V4 pre-M4.7): no magic; `data[]` raw plus an on-disk
    ///   `sorted` tail.  The tail was dropped after it cost 20 B/live-doc
    ///   per column (2.66 GB of redundancy on the 66.5 M nginx workload) —
    ///   `sorted` is rebuilt at decode time with a pdqsort, nanoseconds
    ///   per doc, and range queries keep the same O(log n) bisect.
    /// - **ZNV1**: `data[]` dense raw (8 B/doc) behind a magic prefix.
    ///   Readers dispatch on it; the legacy format starts with
    ///   `doc_count: u32` whose first byte can never be `'Z'`.
    /// - **ZNV2** (issue #1038, stage 1): frame-of-reference + GCD +
    ///   128-doc block bit-packing — the shape tantivy's columnar uses
    ///   (min-subtract, GCD-divide, then bit-pack; citation and measured
    ///   baseline in `benchmarks/index-size/DESIGN.md`).  Raw 8 B/doc
    ///   hands the outer zstd pass an entropy problem it can only
    ///   partially solve; subtracting the frame first is what actually
    ///   shrinks the column.
    ///
    /// ZNV2 layout, all little-endian:
    ///
    /// ```text
    ///     "ZNV2"
    ///     u32   doc_count
    ///     u32   null_bitmap_len, bytes null_bitmap (as ZNV1)
    ///     u64   u_min   global frame: min over live values of
    ///                  u(v) = (v as u64) ^ (1 << 63); 0 when no live value
    ///     u64   gcd     >= 1; divides every (u - u_min); 1 = no win
    ///     per 128-doc block:
    ///         u8      width   0 = constant block, 1..=32 = bit-packed
    ///                         u32 residuals, 0xFF = wide (raw u64 lanes)
    ///         varint  block_min  quotient the residuals are relative to
    ///         bytes   packed residuals — width × 16 B, or 1024 B when
    ///                 wide; omitted entirely when width == 0
    /// ```
    ///
    /// The sign-flip map `v ↦ (v as u64) ^ (1 << 63)` is the monotone
    /// signed→unsigned bijection, so `u − u_min` never underflows and every
    /// step is integer-exact even though `data[]` holds f64 *bit patterns*
    /// for `Double` columns — the flip is undone before `f64::from_bits`
    /// is ever taken, and `sorted` is rebuilt from the decoded array so
    /// its f64-ordered semantics are untouched.  Null slots contribute
    /// nothing to the frame, the GCD, or the block mins, and decode
    /// re-zeroes them to keep the "nulls are 0" contract of `from_iter`.
    /// Block payload length is derived from `width` — there is no
    /// per-block byte_len to store.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 + self.data.len() + 16);
        out.extend_from_slice(b"ZNV2");
        out.write_u32::<LittleEndian>(self.doc_count).unwrap();
        let mut bitmap_buf = Vec::new();
        self.null_bitmap.serialize_into(&mut bitmap_buf).unwrap();
        out.write_u32::<LittleEndian>(bitmap_buf.len() as u32)
            .unwrap();
        out.extend_from_slice(&bitmap_buf);

        // Global frame over live values only. Pass 1 finds u_min; pass 2
        // accumulates the GCD of (u − u_min), exiting early once it can
        // no longer shrink (gcd can only decrease, and 1 is the floor).
        let mut u_min = u64::MAX;
        let mut any_live = false;
        for (i, &v) in self.data.iter().enumerate() {
            if self.null_bitmap.contains(i as u32) {
                continue;
            }
            let u = znv2_flip(v);
            if !any_live || u < u_min {
                u_min = u;
            }
            any_live = true;
        }
        if !any_live {
            u_min = 0;
        }
        let mut gcd = 0u64;
        for (i, &v) in self.data.iter().enumerate() {
            if self.null_bitmap.contains(i as u32) {
                continue;
            }
            gcd = u64_gcd(gcd, znv2_flip(v) - u_min);
            if gcd == 1 {
                break;
            }
        }
        let gcd = gcd.max(1);
        out.write_u64::<LittleEndian>(u_min).unwrap();
        out.write_u64::<LittleEndian>(gcd).unwrap();

        let mut quot = [0u64; ZNV2_BLOCK];
        for start in (0..self.data.len()).step_by(ZNV2_BLOCK) {
            let end = (start + ZNV2_BLOCK).min(self.data.len());
            // Pass 1 per block: quotients + the block's own frame.
            let mut q_min = u64::MAX;
            let mut q_max = 0u64;
            let mut block_live = false;
            for (j, i) in (start..end).enumerate() {
                if self.null_bitmap.contains(i as u32) {
                    continue;
                }
                let q = (znv2_flip(self.data[i]) - u_min) / gcd;
                quot[j] = q;
                if !block_live || q < q_min {
                    q_min = q;
                }
                if q > q_max {
                    q_max = q;
                }
                block_live = true;
            }
            if !block_live {
                q_min = 0;
                q_max = 0;
            }
            let spread = q_max - q_min;
            let width: u8 = if !block_live || spread == 0 {
                0
            } else if spread > u32::MAX as u64 {
                ZNV2_WIDE
            } else {
                (u32::BITS - (spread as u32).leading_zeros()) as u8
            };
            out.push(width);
            write_u64_varint(&mut out, q_min);
            match width {
                0 => {}
                ZNV2_WIDE => {
                    // Post-FOR spread wider than 32 bits (mixed-sign f64
                    // bit patterns, genuinely wild i64s): store the
                    // residuals raw. Never larger than ZNV1's block plus
                    // the varint, and rare in real columns.
                    for (j, &q) in quot.iter().enumerate() {
                        let r = if j < end - start && !self.null_bitmap.contains((start + j) as u32)
                        {
                            q - q_min
                        } else {
                            0
                        };
                        out.write_u64::<LittleEndian>(r).unwrap();
                    }
                }
                w => {
                    let mut lanes = [0u32; ZNV2_BLOCK];
                    for j in 0..end - start {
                        if !self.null_bitmap.contains((start + j) as u32) {
                            lanes[j] = (quot[j] - q_min) as u32;
                        }
                    }
                    let mut packed = vec![0u8; ZNV2_BLOCK * w as usize / 8];
                    use bitpacking::BitPacker;
                    bitpacking::BitPacker4x::new().compress(&lanes, &mut packed, w);
                    out.extend_from_slice(&packed);
                }
            }
        }
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.len() >= 4 && &payload[..4] == b"ZNV2" {
            return Self::decode_znv2(payload);
        }
        // Detect magic — ZNV1 starts with "ZNV1", legacy doesn't.
        let (mut cur, is_new) = if payload.len() >= 4 && &payload[..4] == b"ZNV1" {
            (Cursor::new(&payload[4..]), true)
        } else {
            (Cursor::new(payload), false)
        };

        let doc_count = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
        let bitmap_len = cur.read_u32::<LittleEndian>().map_err(io_to_storage)? as usize;
        let mut bitmap_bytes = vec![0u8; bitmap_len];
        cur.read_exact(&mut bitmap_bytes).map_err(io_to_storage)?;
        let null_bitmap = RoaringBitmap::deserialize_from(&bitmap_bytes[..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("dv numeric bitmap: {e}")))?;
        let mut data = Vec::with_capacity(doc_count as usize);
        for _ in 0..doc_count {
            data.push(cur.read_i64::<LittleEndian>().map_err(io_to_storage)?);
        }

        // Build the sorted-by-value index in RAM.  Legacy format stored
        // it on disk; new format always rebuilds.  For both paths we use
        // the single canonical `build_sorted_index` so `range_doc_ids`
        // semantics are identical.
        let sorted = if is_new {
            build_sorted_index(&data, &null_bitmap)
        } else {
            // Legacy on-disk sorted tail.  Read if present, otherwise
            // rebuild from `data`.
            match cur.read_u32::<LittleEndian>() {
                Ok(sorted_len) => {
                    let mut s = Vec::with_capacity(sorted_len as usize);
                    for _ in 0..sorted_len {
                        let v = cur.read_i64::<LittleEndian>().map_err(io_to_storage)?;
                        let d = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
                        s.push((v, d));
                    }
                    s
                }
                Err(_) => build_sorted_index(&data, &null_bitmap),
            }
        };
        let (live_count, live_sum, live_min, live_max) = compute_stats(&sorted);

        Ok(Self {
            doc_count,
            null_bitmap,
            data,
            sorted,
            live_count,
            live_sum,
            live_min,
            live_max,
        })
    }

    /// ZNV2 reader — the layout mirror of [`encode`](Self::encode).
    fn decode_znv2(payload: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(&payload[4..]);
        let doc_count = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
        let bitmap_len = cur.read_u32::<LittleEndian>().map_err(io_to_storage)? as usize;
        let mut bitmap_bytes = vec![0u8; bitmap_len];
        cur.read_exact(&mut bitmap_bytes).map_err(io_to_storage)?;
        let null_bitmap = RoaringBitmap::deserialize_from(&bitmap_bytes[..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("dv numeric bitmap: {e}")))?;
        let u_min = cur.read_u64::<LittleEndian>().map_err(io_to_storage)?;
        let gcd = cur.read_u64::<LittleEndian>().map_err(io_to_storage)?;

        let mut data = Vec::with_capacity(doc_count as usize);
        let mut lanes = [0u32; ZNV2_BLOCK];
        let mut packed = Vec::new();
        while (data.len() as u32) < doc_count {
            let width = cur.read_u8().map_err(io_to_storage)?;
            let q_min = read_u64_varint(&mut cur)?;
            let base = data.len();
            let end = (base + ZNV2_BLOCK).min(doc_count as usize);
            match width {
                0 => {
                    // Constant block: every live value is the same
                    // quotient, including the all-null block (q_min 0 —
                    // the zeroing pass below restores the null contract).
                    let v = znv2_unflip(u_min.wrapping_add(gcd.wrapping_mul(q_min)));
                    for _ in base..end {
                        data.push(v);
                    }
                }
                ZNV2_WIDE => {
                    for _ in 0..ZNV2_BLOCK {
                        let r = cur.read_u64::<LittleEndian>().map_err(io_to_storage)?;
                        if data.len() < end {
                            data.push(znv2_unflip(
                                u_min.wrapping_add(gcd.wrapping_mul(q_min.wrapping_add(r))),
                            ));
                        }
                    }
                }
                w => {
                    if w > 32 {
                        return Err(StorageError::Other(anyhow::anyhow!(
                            "znv2 invalid block width {w}"
                        )));
                    }
                    let nbytes = ZNV2_BLOCK * w as usize / 8;
                    packed.clear();
                    packed.resize(nbytes, 0);
                    cur.read_exact(&mut packed).map_err(io_to_storage)?;
                    use bitpacking::BitPacker;
                    bitpacking::BitPacker4x::new().decompress(&packed, &mut lanes, w);
                    for &lane in lanes.iter().take(end - base) {
                        let q = q_min.wrapping_add(u64::from(lane));
                        data.push(znv2_unflip(u_min.wrapping_add(gcd.wrapping_mul(q))));
                    }
                }
            }
        }
        // Null slots: `from_iter` stores 0 there — restore that contract
        // so the decoded array is indistinguishable from a built one.
        for doc_id in &null_bitmap {
            data[doc_id as usize] = 0;
        }

        let sorted = build_sorted_index(&data, &null_bitmap);
        let (live_count, live_sum, live_min, live_max) = compute_stats(&sorted);
        Ok(Self {
            doc_count,
            null_bitmap,
            data,
            sorted,
            live_count,
            live_sum,
            live_min,
            live_max,
        })
    }
}

/// ZNV2 block size — matches `bitpacking::BitPacker4x::BLOCK_LEN` (128),
/// the same lane count the `.post` codec packs.
const ZNV2_BLOCK: usize = 128;

/// ZNV2 width byte for blocks whose post-FOR spread exceeds 32 bits —
/// residuals stored raw as u64 lanes instead of bit-packed u32s.
const ZNV2_WIDE: u8 = 0xFF;

/// Sign-flip map: the monotone signed→unsigned bijection. Guarantees
/// `u − u_min` never underflows for live values, whatever the numbers
/// mean (true i64s or f64 bit patterns).
fn znv2_flip(v: i64) -> u64 {
    (v as u64) ^ (1u64 << 63)
}

fn znv2_unflip(u: u64) -> i64 {
    (u ^ (1u64 << 63)) as i64
}

fn u64_gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn write_u64_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn read_u64_varint(cur: &mut Cursor<&[u8]>) -> Result<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        if shift >= 64 {
            return Err(StorageError::Other(anyhow::anyhow!(
                "znv2 varint longer than 64 bits"
            )));
        }
        let b = cur.read_u8().map_err(io_to_storage)?;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
    }
}

/// Walk the sorted index once and produce (count, sum, min, max).
/// Called at build / decode time so per-query stats aggs are O(1).
fn compute_stats(sorted: &[(i64, u32)]) -> (u64, f64, f64, f64) {
    // Pull min/max via pattern-match on `split_first` so the
    // empty-input branch and the value-extracting branch live
    // in the same `match`. Was: `if is_empty() return; ...
    // sorted.first().unwrap()` — safe but vulnerable to the
    // fix-the-guard, forget-the-unwrap class of regression.
    let (Some((first, _)), Some((last, _))) = (sorted.first(), sorted.last()) else {
        return (0, 0.0, f64::NAN, f64::NAN);
    };
    let mut sum = 0.0;
    for (v, _) in sorted {
        sum += f64::from_bits(*v as u64);
    }
    let min = f64::from_bits(*first as u64);
    let max = f64::from_bits(*last as u64);
    (sorted.len() as u64, sum, min, max)
}

fn build_sorted_index(data: &[i64], null_bitmap: &RoaringBitmap) -> Vec<(i64, u32)> {
    let mut out: Vec<(i64, u32)> = Vec::with_capacity(data.len());
    for (i, &v) in data.iter().enumerate() {
        let doc_id = i as u32;
        if !null_bitmap.contains(doc_id) {
            out.push((v, doc_id));
        }
    }
    // Sort by the f64 interpretation so ranges compare correctly for
    // negative and fractional values.  NaN is unlikely in doc-values (we
    // skip nulls) but we fall back to bit comparison as a stable tiebreak.
    out.sort_by(|a, b| {
        let fa = f64::from_bits(a.0 as u64);
        let fb = f64::from_bits(b.0 as u64);
        fa.partial_cmp(&fb)
            .unwrap_or_else(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    out
}

/// Per-doc keyword column with sorted terms FST + ordinal-per-doc array.
#[derive(Debug, Clone)]
pub struct KeywordColumn {
    pub doc_count: u32,
    /// Sorted unique terms.  `term_at(ord)` recovers the string.
    pub terms: Vec<String>,
    /// FST mapping term bytes → ordinal in `terms`.
    pub fst_bytes: Vec<u8>,
    pub null_bitmap: RoaringBitmap,
    /// Per-doc ordinals, indexed by doc_id.  `None` if `null_bitmap`
    /// contains the doc_id.
    pub ords: Vec<u32>,
    /// Per-ordinal doc count (built at decode time, not serialised).
    /// `per_ord_count[ord]` is the number of live docs whose value
    /// matches that ordinal.  Makes `doc_freq(term)` an O(log n FST +
    /// single-index read) instead of O(doc_count).
    pub per_ord_count: Vec<u32>,
}

impl KeywordColumn {
    // Named `from_iter` deliberately; it is not the FromIterator trait method
    // (it is fallible, returning Result<Self>). Renaming would break all
    // callers, so the trait-confusion lint is allowed here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_iter<I: IntoIterator<Item = Option<String>>>(it: I) -> Result<Self> {
        let values: Vec<Option<String>> = it.into_iter().collect();
        let doc_count = values.len() as u32;

        // Collect unique non-null terms.  We assign ordinals in
        // **lexicographic** order so the on-disk ordinal space is stable
        // and the FST insert (which requires keys in sorted order) just
        // walks the same iteration order as the assignment loop.
        //
        // sort+dedup over borrowed &str instead of the previous
        // `BTreeMap<String, ()>` build, which CLONED every non-null cell
        // (hundreds of thousands of String allocs per 31k-doc flush
        // segment) — only unique terms are cloned now.
        let mut refs: Vec<&str> = values.iter().filter_map(|v| v.as_deref()).collect();
        refs.sort_unstable();
        refs.dedup();
        let terms: Vec<String> = refs.iter().map(|s| (*s).to_string()).collect();
        let term_to_ord: std::collections::HashMap<&str, u32> = refs
            .iter()
            .enumerate()
            .map(|(i, s)| (*s, i as u32))
            .collect();

        // Build FST term → ord (keys must be sorted ascending — they are
        // because we iterate `terms` which is the sorted BTreeMap key list).
        let mut fst_buf: Vec<u8> = Vec::new();
        {
            let mut builder = MapBuilder::new(&mut fst_buf)
                .map_err(|e| StorageError::Other(anyhow::anyhow!("dv kw fst builder: {e}")))?;
            for (i, term) in terms.iter().enumerate() {
                builder
                    .insert(term.as_bytes(), i as u64)
                    .map_err(|e| StorageError::Other(anyhow::anyhow!("dv kw fst insert: {e}")))?;
            }
            builder
                .finish()
                .map_err(|e| StorageError::Other(anyhow::anyhow!("dv kw fst finish: {e}")))?;
        }

        let mut null_bitmap = RoaringBitmap::new();
        let mut ords = Vec::with_capacity(values.len());
        let mut per_ord_count: Vec<u32> = vec![0; terms.len()];
        for (i, v) in values.iter().enumerate() {
            match v {
                Some(s) => {
                    let ord = *term_to_ord.get(s.as_str()).unwrap();
                    ords.push(ord);
                    per_ord_count[ord as usize] += 1;
                }
                None => {
                    null_bitmap.insert(i as u32);
                    ords.push(0);
                }
            }
        }

        Ok(Self {
            doc_count,
            terms,
            fst_bytes: fst_buf,
            null_bitmap,
            ords,
            per_ord_count,
        })
    }

    pub fn ord_for(&self, doc_id: u32) -> Option<u32> {
        if self.null_bitmap.contains(doc_id) {
            return None;
        }
        self.ords.get(doc_id as usize).copied()
    }

    pub fn term_for_ord(&self, ord: u32) -> Option<&str> {
        self.terms.get(ord as usize).map(|s| s.as_str())
    }

    /// O(log |terms|) term lookup via the FST.
    pub fn ord_for_term(&self, term: &str) -> Option<u32> {
        let map = FstMap::new(&self.fst_bytes[..]).ok()?;
        map.get(term.as_bytes()).map(|v| v as u32)
    }

    /// Per-segment `doc_freq` for a term — O(log n FST lookup + one
    /// `Vec` read).  The ord histogram is built at decode / `from_iter`
    /// time and stored in `per_ord_count`, so the term-query shortcut
    /// count path pays zero extra work per segment.
    pub fn doc_freq(&self, term: &str) -> u32 {
        let Some(ord) = self.ord_for_term(term) else {
            return 0;
        };
        self.per_ord_count.get(ord as usize).copied().unwrap_or(0)
    }

    fn ord_width(num_terms: u32) -> u8 {
        if num_terms <= u8::MAX as u32 {
            1
        } else if num_terms <= u16::MAX as u32 {
            2
        } else {
            4
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let num_terms = self.terms.len() as u32;
        let ord_width = Self::ord_width(num_terms);
        let mut out =
            Vec::with_capacity(16 + self.fst_bytes.len() + self.ords.len() * ord_width as usize);
        out.write_u32::<LittleEndian>(self.doc_count).unwrap();
        out.write_u32::<LittleEndian>(num_terms).unwrap();
        out.write_u8(ord_width).unwrap();
        let mut bitmap_buf = Vec::new();
        self.null_bitmap.serialize_into(&mut bitmap_buf).unwrap();
        out.write_u32::<LittleEndian>(bitmap_buf.len() as u32)
            .unwrap();
        out.extend_from_slice(&bitmap_buf);
        out.write_u32::<LittleEndian>(self.fst_bytes.len() as u32)
            .unwrap();
        out.extend_from_slice(&self.fst_bytes);
        for &ord in &self.ords {
            match ord_width {
                1 => out.write_u8(ord as u8).unwrap(),
                2 => out.write_u16::<LittleEndian>(ord as u16).unwrap(),
                4 => out.write_u32::<LittleEndian>(ord).unwrap(),
                _ => unreachable!(),
            }
        }
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(payload);
        let doc_count = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
        let num_terms = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
        let ord_width = cur.read_u8().map_err(io_to_storage)?;
        let bitmap_len = cur.read_u32::<LittleEndian>().map_err(io_to_storage)? as usize;
        let mut bitmap_bytes = vec![0u8; bitmap_len];
        cur.read_exact(&mut bitmap_bytes).map_err(io_to_storage)?;
        let null_bitmap = RoaringBitmap::deserialize_from(&bitmap_bytes[..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("dv kw bitmap: {e}")))?;
        let fst_len = cur.read_u32::<LittleEndian>().map_err(io_to_storage)? as usize;
        let mut fst_bytes = vec![0u8; fst_len];
        cur.read_exact(&mut fst_bytes).map_err(io_to_storage)?;

        // Recover the sorted terms list by walking the FST in lex order.
        let map = FstMap::new(&fst_bytes[..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("dv kw fst parse: {e}")))?;
        let mut terms_vec: Vec<(u32, String)> = Vec::with_capacity(num_terms as usize);
        {
            use fst::Streamer;
            let mut stream = map.stream();
            while let Some((k, v)) = stream.next() {
                if let Ok(s) = std::str::from_utf8(k) {
                    terms_vec.push((v as u32, s.to_string()));
                }
            }
        }
        terms_vec.sort_by_key(|(ord, _)| *ord);
        let terms: Vec<String> = terms_vec.into_iter().map(|(_, s)| s).collect();

        let mut ords = Vec::with_capacity(doc_count as usize);
        for _ in 0..doc_count {
            let ord = match ord_width {
                1 => cur.read_u8().map_err(io_to_storage)? as u32,
                2 => cur.read_u16::<LittleEndian>().map_err(io_to_storage)? as u32,
                4 => cur.read_u32::<LittleEndian>().map_err(io_to_storage)?,
                _ => return Err(StorageError::Other(anyhow::anyhow!("bad ord_width"))),
            };
            ords.push(ord);
        }

        // Rebuild the ord → doc count histogram for fast `doc_freq`
        // lookups.  This is not serialised — each segment pays O(doc_count)
        // once at decode time and benefits every term query afterwards.
        let mut per_ord_count: Vec<u32> = vec![0; terms.len()];
        for (i, &ord) in ords.iter().enumerate() {
            if !null_bitmap.contains(i as u32) && (ord as usize) < per_ord_count.len() {
                per_ord_count[ord as usize] += 1;
            }
        }

        Ok(Self {
            doc_count,
            terms,
            fst_bytes,
            null_bitmap,
            ords,
            per_ord_count,
        })
    }
}

/// One column of any kind.
#[derive(Debug, Clone)]
pub enum Column {
    Numeric(NumericColumn),
    Keyword(KeywordColumn),
}

impl Column {
    pub fn doc_count(&self) -> u32 {
        match self {
            Column::Numeric(n) => n.doc_count,
            Column::Keyword(k) => k.doc_count,
        }
    }

    /// Conservative deterministic estimate of heap allocations retained by
    /// this decoded column. This is capacity accounting, not allocator/RSS
    /// identity. The inline `Column` enum itself is owned by the caller's
    /// map and is intentionally not counted here.
    pub fn estimated_retained_bytes(&self) -> u64 {
        match self {
            Column::Numeric(column) => capacity_bytes::<i64>(column.data.capacity())
                .saturating_add(capacity_bytes::<(i64, u32)>(column.sorted.capacity()))
                .saturating_add(roaring_retained_bytes(&column.null_bitmap)),
            Column::Keyword(column) => {
                let term_storage = capacity_bytes::<String>(column.terms.capacity())
                    .saturating_add(column.terms.iter().fold(0_u64, |total, term| {
                        total.saturating_add(usize_to_u64(term.capacity()))
                    }));
                term_storage
                    .saturating_add(usize_to_u64(column.fst_bytes.capacity()))
                    .saturating_add(capacity_bytes::<u32>(column.ords.capacity()))
                    .saturating_add(capacity_bytes::<u32>(column.per_ord_count.capacity()))
                    .saturating_add(roaring_retained_bytes(&column.null_bitmap))
            }
        }
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn capacity_bytes<T>(capacity: usize) -> u64 {
    usize_to_u64(capacity).saturating_mul(usize_to_u64(std::mem::size_of::<T>()))
}

fn roaring_retained_bytes(bitmap: &RoaringBitmap) -> u64 {
    let statistics = bitmap.statistics();
    statistics
        .n_bytes_array_containers
        .saturating_add(statistics.n_bytes_bitset_containers)
        .saturating_add(statistics.n_bytes_run_containers)
        // roaring 0.10 does not expose the container Vec capacity. One
        // conservative three-word allowance per live container covers the
        // key/store slot without pretending serialized size is heap size.
        .saturating_add(
            u64::from(statistics.n_containers)
                .saturating_mul(usize_to_u64(3 * std::mem::size_of::<usize>())),
        )
}

/// Encode a `field → Column` map into the binary `Columns` section payload.
/// Header flag bit on the column kind byte marking a zstd-compressed
/// payload (vs the legacy LZ4 path).  The other 7 bits remain the
/// numeric/keyword kind discriminator so old readers still see a valid
/// kind value — they just fail the magic / length check on the outer
/// envelope and fall back to the LZ4 path.
const KIND_FLAG_ZSTD: u8 = 0x80;
const KIND_MASK: u8 = 0x7F;

/// Zstd level the flush path encodes `.dv` columns at. Pinned for the reason
/// on `stored_codec::STORED_ZSTD_LEVEL`: flush is the back-pressure-critical
/// bound on sustained ingest, and this used to be one of three stacked
/// level-19 encoders in a single flush.
pub const DV_ZSTD_LEVEL: i32 = 3;

/// Encode at [`DV_ZSTD_LEVEL`]; merge calls [`encode_columns_at_level`] to
/// honour `compression.level` (#318).
pub fn encode_columns(columns: &BTreeMap<String, Column>) -> Vec<u8> {
    encode_columns_at_level(columns, DV_ZSTD_LEVEL)
}

/// [`encode_columns`] at a caller-chosen zstd effort level.
///
/// Level is a write-side choice only: the reader dispatches on the
/// `KIND_FLAG_ZSTD` bit and zstd decode is level-independent, so `.dv` files
/// written at different levels coexist in one index with no format change.
pub fn encode_columns_at_level(columns: &BTreeMap<String, Column>, level: i32) -> Vec<u8> {
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(MAGIC).unwrap();
    out.write_u32::<LittleEndian>(columns.len() as u32).unwrap();
    for (name, col) in columns {
        let kind = match col {
            Column::Numeric(_) => KIND_NUMERIC,
            Column::Keyword(_) => KIND_KEYWORD,
        };
        let payload = match col {
            Column::Numeric(n) => n.encode(),
            Column::Keyword(k) => k.encode(),
        };
        // `level` is DV_ZSTD_LEVEL (3) on flush — fast encode
        // (~250 MB/s/core), reverted from level 19 because the flush path
        // needs to keep up with sustained ingest (1 M+ docs/s) and the
        // level-19 pass collapsed throughput by 70-100× under continuous
        // load.  See
        // `engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
        // Merge passes the operator's `compression.level` instead, which is
        // off the ingest critical path.
        // Legacy LZ4-framed column payloads are still auto-detected by
        // the reader via the high bit on the kind byte; level-19-encoded
        // segments from older builds remain readable (zstd decompress is
        // level-independent).
        let zstd_payload = match zstd::encode_all(&payload[..], level) {
            Ok(c) if c.len() < payload.len() => c,
            // If compression doesn't help (rare tiny columns), keep the
            // zstd path anyway — decompression still works and avoids a
            // second reader branch.
            Ok(c) => c,
            Err(_) => {
                // Fallback: raw zstd of an empty compressed level-1 pass.
                // Extremely unlikely — `encode_all` only errors on OOM.
                zstd::encode_all(&payload[..], 1).unwrap_or_else(|_| payload.clone())
            }
        };
        out.write_u8(kind | KIND_FLAG_ZSTD).unwrap();
        out.write_u32::<LittleEndian>(name.len() as u32).unwrap();
        out.extend_from_slice(name.as_bytes());
        out.write_u64::<LittleEndian>(zstd_payload.len() as u64)
            .unwrap();
        out.extend_from_slice(&zstd_payload);
    }
    out
}

pub fn decode_columns(bytes: &[u8]) -> Result<BTreeMap<String, Column>> {
    let mut cur = Cursor::new(bytes);
    let magic = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
    if magic != MAGIC {
        return Err(StorageError::Other(anyhow::anyhow!(
            "doc-values magic mismatch: {:#x}",
            magic
        )));
    }
    let num_columns = cur.read_u32::<LittleEndian>().map_err(io_to_storage)?;
    let mut out = BTreeMap::new();
    for _ in 0..num_columns {
        let kind_byte = cur.read_u8().map_err(io_to_storage)?;
        let is_zstd = (kind_byte & KIND_FLAG_ZSTD) != 0;
        let kind = kind_byte & KIND_MASK;
        let name_len = cur.read_u32::<LittleEndian>().map_err(io_to_storage)? as usize;
        let mut name_bytes = vec![0u8; name_len];
        cur.read_exact(&mut name_bytes).map_err(io_to_storage)?;
        let name = String::from_utf8(name_bytes)
            .map_err(|e| StorageError::Other(anyhow::anyhow!("dv field name utf8: {e}")))?;
        let payload_len = cur.read_u64::<LittleEndian>().map_err(io_to_storage)? as usize;
        let mut compressed = vec![0u8; payload_len];
        cur.read_exact(&mut compressed).map_err(io_to_storage)?;
        let payload = if is_zstd {
            zstd::decode_all(&compressed[..])
                .map_err(|e| StorageError::Other(anyhow::anyhow!("dv zstd decompress: {e}")))?
        } else {
            lz4_flex::decompress_size_prepended(&compressed)
                .map_err(|e| StorageError::Other(anyhow::anyhow!("dv lz4 decompress: {e}")))?
        };
        let column = match kind {
            KIND_NUMERIC => Column::Numeric(NumericColumn::decode(&payload)?),
            KIND_KEYWORD => Column::Keyword(KeywordColumn::decode(&payload)?),
            _ => {
                return Err(StorageError::Other(anyhow::anyhow!(
                    "unknown doc-values kind {kind}"
                )))
            }
        };
        out.insert(name, column);
    }
    Ok(out)
}

fn io_to_storage(e: std::io::Error) -> StorageError {
    StorageError::Other(anyhow::anyhow!("doc-values io: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_roundtrip() {
        let col = NumericColumn::from_iter(vec![Some(1), Some(2), None, Some(4), Some(5)]);
        assert_eq!(col.get(0), Some(1));
        assert_eq!(col.get(2), None);
        let bytes = col.encode();
        let back = NumericColumn::decode(&bytes).unwrap();
        for i in 0..5 {
            assert_eq!(col.get(i), back.get(i));
        }
        // Sorted index should have 4 entries (one null in doc 2).
        assert_eq!(back.sorted.len(), 4);
    }

    #[test]
    fn numeric_range_query() {
        // Store f64 bit patterns so the range query sees natural float
        // ordering, matching how `build_doc_value_columns` writes numerics.
        let col = NumericColumn::from_iter(vec![
            Some((10.0_f64).to_bits() as i64),
            Some((20.0_f64).to_bits() as i64),
            Some((30.0_f64).to_bits() as i64),
            Some((40.0_f64).to_bits() as i64),
            None,
            Some((50.0_f64).to_bits() as i64),
        ]);
        // Round-trip the encoded form so we test the decoded sorted index.
        let back = NumericColumn::decode(&col.encode()).unwrap();

        let mut r = back.range_doc_ids(15.0, 45.0, true, true);
        r.sort();
        assert_eq!(r, vec![1, 2, 3]);

        assert_eq!(back.range_count(15.0, 45.0, true, true), 3);
        assert_eq!(back.range_count(20.0, 40.0, false, false), 1); // just 30
        assert_eq!(back.range_count(20.0, 40.0, true, true), 3);
        assert_eq!(back.range_count(0.0, 1.0, true, true), 0);
        assert_eq!(back.range_count(50.0, 100.0, true, true), 1);
    }

    #[test]
    fn znv2_monotone_timestamps_roundtrip_and_shrink() {
        // Date-column shape: 41-bit globals whose GCD (37) and per-block
        // frame collapse to a few bits per doc. Raw ZNV1 stored 8 B/doc.
        let n = 5_000;
        let col =
            NumericColumn::from_iter((0..n).map(|i| Some(1_700_000_000_000_i64 + i as i64 * 37)));
        let bytes = col.encode();
        assert!(&bytes[..4] == b"ZNV2");
        assert!(
            bytes.len() < n * 8,
            "znv2 {} B vs raw {} B",
            bytes.len(),
            n * 8
        );
        let back = NumericColumn::decode(&bytes).unwrap();
        assert_eq!(col.data, back.data);
        assert_eq!(col.sorted, back.sorted);
        assert_eq!(col.live_count, back.live_count);
        assert_eq!(col.live_min, back.live_min);
    }

    #[test]
    fn znv2_gcd_collapses_scaled_columns() {
        // Every value is a multiple of 1000: the GCD divide shrinks the
        // quotients a thousandfold even though the raw spread is ~1e6.
        let n = 1_000;
        let col = NumericColumn::from_iter((0..n).map(|i| Some(50_000_i64 + i as i64 * 1_000)));
        let bytes = col.encode();
        assert!(bytes.len() < n * 2, "{} B for {} values", bytes.len(), n);
        let back = NumericColumn::decode(&bytes).unwrap();
        assert_eq!(col.data, back.data);
    }

    #[test]
    fn znv2_constant_and_boolean_columns() {
        // Every value identical: width-0 constant blocks, no packed bytes.
        let col = NumericColumn::from_iter((0..300).map(|_| Some(42_i64)));
        let bytes = col.encode();
        assert!(bytes.len() < 64, "{} B", bytes.len());
        assert_eq!(NumericColumn::decode(&bytes).unwrap().data, col.data);

        // Boolean column (f64 bit patterns of 0.0 / 1.0): two distinct
        // values whose difference becomes the GCD → 1-bit lanes instead
        // of 8 raw bytes per doc.
        let col = NumericColumn::from_iter((0..1_000).map(|i| {
            Some(if i % 3 == 0 {
                0.0_f64.to_bits() as i64
            } else {
                1.0_f64.to_bits() as i64
            })
        }));
        let bytes = col.encode();
        assert!(bytes.len() < 1_000, "{} B", bytes.len());
        let back = NumericColumn::decode(&bytes).unwrap();
        assert_eq!(col.data, back.data);
    }

    #[test]
    fn znv2_nulls_and_empty_roundtrip() {
        // All-null: no live values; decode restores the all-zero array.
        let col = NumericColumn::from_iter((0..257).map(|_| None::<i64>));
        let back = NumericColumn::decode(&col.encode()).unwrap();
        assert_eq!(back.data, vec![0_i64; 257]);
        assert!(back.sorted.is_empty());
        assert_eq!(back.live_count, 0);

        // Empty column.
        let empty = NumericColumn::from_iter(Vec::<Option<i64>>::new());
        let back = NumericColumn::decode(&empty.encode()).unwrap();
        assert_eq!(back.doc_count, 0);
        assert!(back.data.is_empty());

        // Sparse nulls inside a monotone column: live data intact, null
        // slots re-zeroed, per-doc reads identical.
        let col = NumericColumn::from_iter((0..500).map(|i| {
            if i % 17 == 5 {
                None
            } else {
                Some(1_700_000_000_000 + i as i64 * 37)
            }
        }));
        let back = NumericColumn::decode(&col.encode()).unwrap();
        assert_eq!(col.data, back.data);
        assert_eq!(col.sorted, back.sorted);
        for i in 0..500_u32 {
            assert_eq!(col.get(i), back.get(i));
        }
    }

    #[test]
    fn znv2_block_boundaries_roundtrip() {
        // Tail blocks of every shape: 1, sub-128, exact, +1, multiple+1.
        // Values include negatives (sign-flip path) and a 1017 stride.
        for n in [1_usize, 127, 128, 129, 255, 256, 257, 1_000] {
            let col = NumericColumn::from_iter((0..n).map(|i| Some((i as i64 - 300) * 1_017)));
            let bytes = col.encode();
            let back = NumericColumn::decode(&bytes).unwrap();
            assert_eq!(col.data, back.data, "n={n}");
            assert_eq!(col.sorted, back.sorted, "n={n}");
        }
    }

    #[test]
    fn znv2_wide_blocks_roundtrip_and_stay_queryable() {
        // Mixed-sign f64 bit patterns: the post-flip spread can exceed 32
        // bits, exercising the 0xFF raw-lane fallback. The decoded column
        // must still answer f64 range queries exactly.
        let col = NumericColumn::from_iter((0..600).map(|i| {
            Some(if i % 2 == 0 {
                (-1.0_f64 - i as f64).to_bits() as i64
            } else {
                (i as f64 * 0.25).to_bits() as i64
            })
        }));
        let bytes = col.encode();
        assert!(&bytes[..4] == b"ZNV2");
        let back = NumericColumn::decode(&bytes).unwrap();
        assert_eq!(col.data, back.data);
        assert_eq!(col.sorted, back.sorted);
        // evens i∈{0,2,..,8} give -1..-9; odds give 0.25 steps, only 0.25
        // fits the closed range.
        assert_eq!(back.range_count(-10.5, 0.25, true, true), 6);
    }

    #[test]
    fn znv1_payload_still_decodes() {
        // Hand-written ZNV1 bytes (the pre-ZNV2 writer): index dirs from
        // older builds remain readable through the magic dispatch.
        let col = NumericColumn::from_iter(vec![Some(10_i64), None, Some(-3), Some(10)]);
        let mut b = Vec::new();
        b.extend_from_slice(b"ZNV1");
        b.write_u32::<LittleEndian>(col.doc_count).unwrap();
        let mut bm = Vec::new();
        col.null_bitmap.serialize_into(&mut bm).unwrap();
        b.write_u32::<LittleEndian>(bm.len() as u32).unwrap();
        b.extend_from_slice(&bm);
        for &v in &col.data {
            b.write_i64::<LittleEndian>(v).unwrap();
        }
        let back = NumericColumn::decode(&b).unwrap();
        assert_eq!(back.data, col.data);
        assert_eq!(back.get(1), None);
        assert_eq!(back.get(2), Some(-3));
        assert_eq!(back.sorted.len(), 3);
    }

    #[test]
    fn keyword_roundtrip() {
        let col = KeywordColumn::from_iter(vec![
            Some("GET".to_string()),
            Some("POST".to_string()),
            None,
            Some("GET".to_string()),
            Some("DELETE".to_string()),
        ])
        .unwrap();
        assert_eq!(col.ord_for(0), col.ord_for(3));
        assert_ne!(col.ord_for(0), col.ord_for(1));
        assert_eq!(col.ord_for(2), None);
        assert_eq!(col.term_for_ord(col.ord_for(0).unwrap()), Some("GET"));
        assert_eq!(col.doc_freq("GET"), 2);
        assert_eq!(col.doc_freq("PUT"), 0);

        let bytes = col.encode();
        let back = KeywordColumn::decode(&bytes).unwrap();
        for i in 0..5 {
            assert_eq!(
                col.ord_for(i)
                    .map(|o| col.term_for_ord(o).unwrap().to_string()),
                back.ord_for(i)
                    .map(|o| back.term_for_ord(o).unwrap().to_string()),
            );
        }
    }

    #[test]
    fn columns_section_roundtrip() {
        let mut cols = BTreeMap::new();
        cols.insert(
            "status".to_string(),
            Column::Numeric(NumericColumn::from_iter(vec![
                Some(200),
                Some(404),
                Some(200),
            ])),
        );
        cols.insert(
            "method".to_string(),
            Column::Keyword(
                KeywordColumn::from_iter(vec![
                    Some("GET".to_string()),
                    Some("POST".to_string()),
                    Some("GET".to_string()),
                ])
                .unwrap(),
            ),
        );
        let bytes = encode_columns(&cols);
        let back = decode_columns(&bytes).unwrap();
        assert_eq!(cols.len(), back.len());
        if let (Column::Numeric(a), Column::Numeric(b)) = (&cols["status"], &back["status"]) {
            assert_eq!(a.get(0), b.get(0));
            assert_eq!(a.get(1), b.get(1));
        } else {
            panic!("expected numeric");
        }
    }

    #[test]
    fn retained_estimates_count_numeric_keyword_capacity_and_roaring() {
        let mut numeric = NumericColumn::from_iter(vec![Some(1), None, Some(3)]);
        numeric.data.reserve(128);
        numeric.sorted.reserve(64);
        let numeric = Column::Numeric(numeric);
        let numeric_estimate = numeric.estimated_retained_bytes();
        let Column::Numeric(numeric_column) = &numeric else {
            unreachable!()
        };
        assert!(
            numeric_estimate
                >= (numeric_column.data.capacity() * std::mem::size_of::<i64>()
                    + numeric_column.sorted.capacity() * std::mem::size_of::<(i64, u32)>())
                    as u64
        );

        let mut keyword = KeywordColumn::from_iter(vec![
            Some(String::from("alpha")),
            None,
            Some(String::from("beta")),
        ])
        .unwrap();
        keyword.terms.reserve(32);
        keyword.fst_bytes.reserve(256);
        keyword.ords.reserve(128);
        keyword.per_ord_count.reserve(64);
        let keyword = Column::Keyword(keyword);
        let keyword_estimate = keyword.estimated_retained_bytes();
        let Column::Keyword(keyword_column) = &keyword else {
            unreachable!()
        };
        let vector_floor = keyword_column.terms.capacity() * std::mem::size_of::<String>()
            + keyword_column.fst_bytes.capacity()
            + keyword_column.ords.capacity() * std::mem::size_of::<u32>()
            + keyword_column.per_ord_count.capacity() * std::mem::size_of::<u32>();
        assert!(keyword_estimate >= vector_floor as u64);
    }
}
