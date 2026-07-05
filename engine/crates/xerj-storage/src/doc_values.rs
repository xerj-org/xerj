//! On-disk doc-values (column store) — one column per indexed field.
//!
//! Ported from the Lucene 9.0 `Lucene90DocValuesFormat` design but
//! **deliberately simple** in this first cut so it lands fast and is easy
//! to verify against `aggs::run_terms` / `aggs::run_stats`.  Block-level
//! bit-packing and global-ordinal sharing across segments come in M3.
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
//!         u8    kind (0 = numeric, 1 = keyword)
//!         u32   field_name_len
//!         bytes field_name
//!         u64   payload_len
//!         bytes payload  (LZ4-compressed)
//! ```
//!
//! Numeric payload format (uncompressed):
//!
//! ```text
//!     u32   doc_count
//!     bytes null_bitmap_serialized (length-prefixed roaring)
//!     for each doc i in [0..doc_count):
//!         i64   value         // 0 if null
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
        self.sorted[lo_idx..hi_idx].iter().map(|(_, d)| *d).collect()
    }

    /// Count-only variant of `range_doc_ids` — returns just the number of
    /// matching docs without allocating a Vec.
    pub fn range_count(
        &self,
        min: f64,
        max: f64,
        min_inclusive: bool,
        max_inclusive: bool,
    ) -> u64 {
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

    /// V4 M4.7 — drop the redundant `sorted: Vec<(i64, u32)>` tail.
    ///
    /// Pre-M4.7 `encode()` wrote the `data` array (8 B × doc_count) AND
    /// a second copy of every live value as `(i64, u32)` pairs
    /// (12 B × live_count).  On nginx `status`/`bytes` columns that was
    /// 20 B/live-doc per column = 2.66 GB of redundancy on the 66.5 M
    /// workload.  The new encoding stores only the dense `data[]` array
    /// and rebuilds `sorted` from it at `decode()` time using a pdqsort
    /// that's nanoseconds per doc.  Range queries get the same O(log n)
    /// bisect behaviour as before.
    ///
    /// Magic prefix `ZNV1` distinguishes the new layout from the legacy
    /// format (which had no magic).  Legacy readers still work because
    /// the magic bytes are at offset 0 and the legacy format starts with
    /// `doc_count: u32` whose first byte is 0-255 but never matches 'Z'
    /// for a real doc count.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            4 + 4 + 4 + self.data.len() * 8,
        );
        out.extend_from_slice(b"ZNV1");
        out.write_u32::<LittleEndian>(self.doc_count).unwrap();
        let mut bitmap_buf = Vec::new();
        self.null_bitmap.serialize_into(&mut bitmap_buf).unwrap();
        out.write_u32::<LittleEndian>(bitmap_buf.len() as u32).unwrap();
        out.extend_from_slice(&bitmap_buf);
        for &v in &self.data {
            out.write_i64::<LittleEndian>(v).unwrap();
        }
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self> {
        // Detect magic — new format starts with "ZNV1", legacy doesn't.
        let (mut cur, is_new) = if payload.len() >= 4 && &payload[..4] == b"ZNV1" {
            (Cursor::new(&payload[4..]), true)
        } else {
            (Cursor::new(&payload[..]), false)
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
        let mut refs: Vec<&str> = values
            .iter()
            .filter_map(|v| v.as_deref())
            .collect();
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
        self.per_ord_count
            .get(ord as usize)
            .copied()
            .unwrap_or(0)
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
}

/// Encode a `field → Column` map into the binary `Columns` section payload.
/// Header flag bit on the column kind byte marking a zstd-compressed
/// payload (vs the legacy LZ4 path).  The other 7 bits remain the
/// numeric/keyword kind discriminator so old readers still see a valid
/// kind value — they just fail the magic / length check on the outer
/// envelope and fall back to the LZ4 path.
const KIND_FLAG_ZSTD: u8 = 0x80;
const KIND_MASK: u8 = 0x7F;

pub fn encode_columns(columns: &BTreeMap<String, Column>) -> Vec<u8> {
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
        // Zstd level 3 — fast encode (~250 MB/s/core).  Reverted from
        // level 19 because the flush path needs to keep up with sustained
        // ingest (1 M+ docs/s) and the level-19 pass collapsed throughput
        // by 70-100× under continuous load.  See
        // `engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
        // Legacy LZ4-framed column payloads are still auto-detected by
        // the reader via the high bit on the kind byte; level-19-encoded
        // segments from older builds remain readable (zstd decompress is
        // level-independent).
        let zstd_payload = match zstd::encode_all(&payload[..], 3) {
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
        out.write_u64::<LittleEndian>(zstd_payload.len() as u64).unwrap();
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
                col.ord_for(i).map(|o| col.term_for_ord(o).unwrap().to_string()),
                back.ord_for(i).map(|o| back.term_for_ord(o).unwrap().to_string()),
            );
        }
    }

    #[test]
    fn columns_section_roundtrip() {
        let mut cols = BTreeMap::new();
        cols.insert(
            "status".to_string(),
            Column::Numeric(NumericColumn::from_iter(vec![Some(200), Some(404), Some(200)])),
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
}
