//! Stored-section codec.
//!
//! Supports two on-disk formats, selected per-segment:
//!
//! | Magic    | Version | Description                                        |
//! |----------|---------|----------------------------------------------------|
//! | `LZ4\0`  | v1      | LZ4 over a flat JSON array `[{_id,_seq_no,_source}]`|
//! | `ZBS2`   | v2      | Columnar block — per-column codec, cross-col dep, dict+bitpack |
//!
//! The decoder detects the magic and returns a canonical JSON-array payload
//! for both, so callers upstream do not need to change.
//!
//! V2 format exists because v1 LZ4 over flat JSON leaves structural
//! redundancy on the table: every row repeats the schema keys, and numeric
//! columns determined by keyword columns (URL → status, URL → bytes on
//! nginx logs) can be collapsed to mode+exceptions.  `~/cz/src/v7_column_*`
//! demonstrates 397× on this corpus; V2 is the random-access-friendly
//! subset of those techniques — the ones that survive when you still need
//! to materialise an individual document by `doc_ord`.
//!
//! V2 payload layout:
//!
//! ```text
//! "ZBS2"                           4 bytes
//! u32  num_docs
//! u32  num_columns
//! per column (num_columns times):
//!     u16  name_len; name_len bytes of UTF-8 name
//!     u8   codec_id
//!     u32  payload_size
//!     payload_size bytes           // see `col_codec` module below
//! ```
//!
//! Column codecs (u8 id in the header):
//!
//! * 0 — `RAW_JSON`: fallback for complex shapes (objects, arrays).  Payload
//!   is zstd-compressed JSON array of the column values.  Also the codec
//!   used when row count < `V2_MIN_DOCS`.
//! * 1 — `LZ4_JSON`: same as RAW but LZ4 instead of zstd.  Picked when
//!   content is compressible but CPU budget is tight.
//! * 2 — `DICT_BITPACK`: dictionary-encoded column.  Payload =
//!   `u32 dict_count; varint[dict_count] dict_entry_lens; dict_entries; u32 zstd_len; zstd(bitpacked_ids)`.
//!   Rows with `null` use dict id `dict_count` (reserved).
//! * 3 — `CROSS_DEP`: numeric column determined by another dict-encoded
//!   column's dict ids, ≥ 90 % deterministic.  Payload =
//!   `u32 src_col_ix; u32 mode_table_len; i64[mode_table_len] mode_values;
//!   u32 exc_count; (varint delta doc_ord, zigzag_varint target_value)[exc_count]`.
//!   Any source dict id that was never present in the block is represented
//!   by `i64::MIN` in `mode_values` (sentinel).
//! * 4 — `CONSTANT`: a single repeated value.  Payload = zstd(json_of_value).
//!
//! Decoder always returns the canonical v1 JSON-array shape so the rest of
//! the engine is oblivious to which codec was used.

use crate::{Result, StorageError};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

// ── V1: flat LZ4 over JSON (legacy) ───────────────────────────────────────

/// Magic prefix for LZ4-compressed stored sections (v1, pre-columnar).
pub const STORED_LZ4_MAGIC: &[u8; 4] = b"LZ4\0";

/// Compress a stored-section blob using LZ4 and prepend the magic header.
/// Kept for tests and for the slow-path fallback.
pub fn encode_stored_lz4(uncompressed: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::compress_prepend_size(uncompressed);
    let mut out = Vec::with_capacity(4 + compressed.len());
    out.extend_from_slice(STORED_LZ4_MAGIC);
    out.extend_from_slice(&compressed);
    out
}

// ── V2: columnar block with per-column codec ─────────────────────────────

/// Magic prefix for the V2 columnar format.
pub const STORED_V2_MAGIC: &[u8; 4] = b"ZBS2";

/// Minimum document count to bother with the columnar path.  Below this,
/// the per-column header overhead dominates and flat LZ4 wins.
const V2_MIN_DOCS: usize = 128;

/// Determinism threshold for `CROSS_DEP` — a numeric column is stored as
/// cross-dependent on a keyword column iff at least this fraction of rows
/// match the mode value for their source-key value.
const CROSS_DEP_MIN_DETERMINISM: f64 = 0.90;

/// Cardinality cap for `DICT_BITPACK`.  Above this, dict+bitpack loses to
/// zstd over LZ4-JSON.
const DICT_MAX_CARDINALITY: usize = 16_384;

/// Zstd level used for every per-column / dict / cross-dep / fallback
/// payload in V2.
///
/// History: 3 → 9 → 19 → 3.  The level-19 pass (commit 73c6367) was
/// reverted because it ran on the **flush** path, not just merge.
/// Per-shard flush throughput is the back-pressure-critical bound on
/// sustained ingest.  Level-19 zstd at ~25 MB/s/core stacks behind two
/// other level-19 encoders (.dv per-column and .post / .meta in
/// xerj-fts), so a ~50 MB segment flush took 5-10 s instead of
/// ~250 ms — the memtable hit its 3× cap, back-pressure triggered, and
/// 32 parallel ingest workers blew through the retry budget.  Result:
/// 75 % rejected docs at 21 K docs/s sustained vs the previous 1.55 M
/// docs/s peak.
///
/// Level 3 puts ingest back at ~250 MB/s/core encode, restores the
/// 1 M+ docs/s ingest path, and grows steady-state segments by ~5 %
/// (merges are infrequent and dominated by long-term-stored data, so
/// the on-flush overhead is only paid by the freshest tier-0 segments
/// before they merge anyway).  See
/// `engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`.
const STORED_ZSTD_LEVEL: i32 = 3;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
enum ColCodec {
    RawJson = 0,
    Lz4Json = 1,
    DictBitpack = 2,
    CrossDep = 3,
    Constant = 4,
}

impl ColCodec {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::RawJson),
            1 => Some(Self::Lz4Json),
            2 => Some(Self::DictBitpack),
            3 => Some(Self::CrossDep),
            4 => Some(Self::Constant),
            _ => None,
        }
    }
}

/// Encode a stored section written as a JSON array of
/// `{"_id", "_seq_no", "_source"}` objects using the v2 columnar format.
///
/// On tiny segments (< `V2_MIN_DOCS` docs) or on any shape that doesn't
/// fit the column assumptions, the function silently falls back to the
/// v1 LZ4 encoder — the caller never has to care which was used.
pub fn encode_stored_v2(stored_docs_json: &[u8]) -> Vec<u8> {
    // Parse the JSON array of documents.  Failure → v1 fallback.
    let docs: Vec<serde_json::Value> = match serde_json::from_slice(stored_docs_json) {
        Ok(v) => v,
        Err(_) => return encode_stored_lz4(stored_docs_json),
    };
    if docs.len() < V2_MIN_DOCS {
        return encode_stored_lz4(stored_docs_json);
    }

    // Flatten each doc into (id, seq_no, source-fields).  Anything non-JSON
    // at the top level degrades to the RAW column codec.  Keys are
    // collected from _source; the top-level _id and _seq_no become
    // synthetic columns `__id` and `__seq_no` (names starting with `__`
    // are reserved).
    let num_docs = docs.len();

    // First pass: discover the union of _source field names preserving
    // insertion order, plus dtype hints.
    let mut col_order: Vec<String> = vec!["__id".into(), "__seq_no".into()];
    let mut col_seen: HashMap<String, usize> = HashMap::new();
    col_seen.insert("__id".into(), 0);
    col_seen.insert("__seq_no".into(), 1);

    for doc in &docs {
        if let Some(src) = doc.get("_source").and_then(|s| s.as_object()) {
            for key in src.keys() {
                if !col_seen.contains_key(key) {
                    col_seen.insert(key.clone(), col_order.len());
                    col_order.push(key.clone());
                }
            }
        }
    }

    // Second pass: materialise each column as Vec<&serde_json::Value>.
    // `__id` and `__seq_no` are hydrated from the top-level doc object.
    let mut columns: Vec<Vec<serde_json::Value>> =
        vec![Vec::with_capacity(num_docs); col_order.len()];

    let null = serde_json::Value::Null;
    for doc in &docs {
        // __id and __seq_no
        let id = doc.get("_id").cloned().unwrap_or(serde_json::Value::Null);
        let seq = doc.get("_seq_no").cloned().unwrap_or(serde_json::Value::Null);
        columns[0].push(id);
        columns[1].push(seq);

        let src_obj = doc.get("_source").and_then(|s| s.as_object());
        for (cix, cname) in col_order.iter().enumerate().skip(2) {
            let val = src_obj
                .and_then(|m| m.get(cname))
                .cloned()
                .unwrap_or_else(|| null.clone());
            columns[cix].push(val);
        }
    }

    // Build dict-encoded form for each column as a stable side representation.
    // `dict_encode(col)` returns Some((dict_entries, ids)) where ids[i] is the
    // 1-based id (0 reserved for null, we add 1 here).  Returns None when
    // the column has non-scalar values or unique-count exceeds DICT_MAX_CARDINALITY.
    let dict_encoded: Vec<Option<(Vec<serde_json::Value>, Vec<u32>)>> =
        columns.iter().map(|c| dict_encode_column(c)).collect();

    // For each numeric column, try to find a dict-encoded keyword-like
    // source column that determines it.  First match wins.
    let cross_dep_src: Vec<Option<usize>> = columns
        .iter()
        .enumerate()
        .map(|(cix, col)| {
            if !col_is_numeric(col) { return None; }
            best_cross_dep_source(&dict_encoded, cix, col)
        })
        .collect();

    // Pick a codec per column and build its payload.
    let mut col_payloads: Vec<(String, u8, Vec<u8>)> = Vec::with_capacity(col_order.len());
    for (cix, cname) in col_order.iter().enumerate() {
        let col = &columns[cix];

        // CONSTANT → CROSS_DEP → DICT_BITPACK → LZ4_JSON → RAW_JSON fallback.
        if let Some(payload) = try_encode_constant(col) {
            col_payloads.push((cname.clone(), ColCodec::Constant as u8, payload));
            continue;
        }
        if let Some(src_ix) = cross_dep_src[cix] {
            let (payload, ok) = encode_cross_dep(col, src_ix, &dict_encoded);
            if ok {
                col_payloads.push((cname.clone(), ColCodec::CrossDep as u8, payload));
                continue;
            }
        }
        if let Some((entries, ids)) = &dict_encoded[cix] {
            if entries.len() <= DICT_MAX_CARDINALITY && all_scalar_dict_entries(entries) {
                let payload = encode_dict_bitpack(entries, ids);
                col_payloads.push((cname.clone(), ColCodec::DictBitpack as u8, payload));
                continue;
            }
        }
        // Fallback: zstd over JSON-array of the column's values.
        let col_json = serde_json::to_vec(col).unwrap_or_default();
        let zstd_payload = zstd::encode_all(Cursor::new(&col_json), STORED_ZSTD_LEVEL).unwrap_or_else(|_| col_json.clone());
        // Choose RAW_JSON vs LZ4_JSON by size.
        let lz4_payload = lz4_flex::compress_prepend_size(&col_json);
        if lz4_payload.len() + 1 < zstd_payload.len() {
            col_payloads.push((cname.clone(), ColCodec::Lz4Json as u8, lz4_payload));
        } else {
            col_payloads.push((cname.clone(), ColCodec::RawJson as u8, zstd_payload));
        }
    }

    // Assemble the V2 payload.
    let mut out: Vec<u8> = Vec::with_capacity(4 + 8 + col_payloads.iter().map(|(n, _, p)| 2 + n.len() + 5 + p.len()).sum::<usize>());
    out.extend_from_slice(STORED_V2_MAGIC);
    out.write_u32::<LittleEndian>(num_docs as u32).unwrap();
    out.write_u32::<LittleEndian>(col_payloads.len() as u32).unwrap();
    for (name, codec_id, payload) in &col_payloads {
        out.write_u16::<LittleEndian>(name.len() as u16).unwrap();
        out.extend_from_slice(name.as_bytes());
        out.push(*codec_id);
        out.write_u32::<LittleEndian>(payload.len() as u32).unwrap();
        out.extend_from_slice(payload);
    }

    // If v1 LZ4 would have been smaller, use it instead.  This is the
    // "never make things worse" safety net that mirrors cz's per-column
    // best-of-codec picker.
    let v1 = encode_stored_lz4(stored_docs_json);
    if v1.len() < out.len() { v1 } else { out }
}

/// Decode a stored section written by any supported codec version.
///
/// Returns the canonical JSON-array payload that every upstream caller
/// expects (`[{"_id", "_seq_no", "_source": {...}}, ...]`).
pub fn decode_stored(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() >= 4 && &bytes[..4] == STORED_V2_MAGIC {
        return decode_stored_v2(&bytes[4..]);
    }
    if bytes.len() >= 4 && &bytes[..4] == STORED_LZ4_MAGIC {
        return lz4_flex::decompress_size_prepended(&bytes[4..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("LZ4 decompress failed: {e}")));
    }
    Ok(bytes.to_vec())
}

fn decode_stored_v2(body: &[u8]) -> Result<Vec<u8>> {
    let mut cur = Cursor::new(body);
    let num_docs = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 num_docs: {e}")))? as usize;
    let num_cols = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 num_cols: {e}")))? as usize;

    // First pass: decode each column into Vec<Value>.  Store the name
    // alongside so we can re-assemble the docs.
    let mut col_names: Vec<String> = Vec::with_capacity(num_cols);
    let mut col_data: Vec<Vec<serde_json::Value>> = Vec::with_capacity(num_cols);

    for _ in 0..num_cols {
        let name_len = cur.read_u16::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 name_len: {e}")))? as usize;
        let pos = cur.position() as usize;
        if body.len() < pos + name_len {
            return Err(StorageError::Other(anyhow::anyhow!("v2 truncated name")));
        }
        let name = std::str::from_utf8(&body[pos..pos + name_len])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 bad name utf8: {e}")))?
            .to_string();
        cur.set_position((pos + name_len) as u64);

        let codec_id = cur.read_u8()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 codec_id: {e}")))?;
        let codec = ColCodec::from_u8(codec_id)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 unknown codec {}", codec_id)))?;
        let payload_len = cur.read_u32::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 payload_len: {e}")))? as usize;
        let pos = cur.position() as usize;
        if body.len() < pos + payload_len {
            return Err(StorageError::Other(anyhow::anyhow!("v2 truncated payload")));
        }
        let payload = &body[pos..pos + payload_len];
        cur.set_position((pos + payload_len) as u64);

        let values = match codec {
            ColCodec::RawJson => decode_raw_json(payload)?,
            ColCodec::Lz4Json => decode_lz4_json(payload)?,
            ColCodec::Constant => decode_constant(payload, num_docs)?,
            ColCodec::DictBitpack => decode_dict_bitpack(payload, num_docs)?,
            ColCodec::CrossDep => {
                // CROSS_DEP requires the source column to already be
                // decoded.  We decode it in a second pass below — here we
                // emit a placeholder and revisit it after.
                col_names.push(name);
                col_data.push(Vec::new());
                // Stash the raw payload in col_data[i] as a single
                // "deferred" Value.  Second pass recognises and replaces.
                col_data.last_mut().unwrap().push(serde_json::Value::Object({
                    let mut m = serde_json::Map::new();
                    m.insert("__deferred_cross_dep__".into(), serde_json::Value::Array(
                        payload.iter().map(|b| serde_json::Value::Number((*b).into())).collect(),
                    ));
                    m
                }));
                continue;
            }
        };
        col_names.push(name);
        col_data.push(values);
    }

    // Second pass: resolve deferred CROSS_DEP columns now that all
    // source columns are materialised.
    let col_name_to_ix: HashMap<String, usize> = col_names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i))
        .collect();
    for cix in 0..col_data.len() {
        if col_data[cix].len() == 1 {
            if let serde_json::Value::Object(ref m) = col_data[cix][0] {
                if let Some(serde_json::Value::Array(bytes_arr)) = m.get("__deferred_cross_dep__") {
                    let payload: Vec<u8> = bytes_arr
                        .iter()
                        .filter_map(|v| v.as_u64().map(|u| u as u8))
                        .collect();
                    let resolved = decode_cross_dep(&payload, num_docs, &col_data, &col_name_to_ix)?;
                    col_data[cix] = resolved;
                    continue;
                }
            }
        }
    }

    // Re-assemble the JSON-array payload.  col_data[0] = __id, col_data[1] = __seq_no.
    let id_col_ix = col_name_to_ix.get("__id").copied().unwrap_or(0);
    let seq_col_ix = col_name_to_ix.get("__seq_no").copied().unwrap_or(1);

    let mut out_docs: Vec<serde_json::Value> = Vec::with_capacity(num_docs);
    for d in 0..num_docs {
        let mut source_map = serde_json::Map::new();
        for (cix, name) in col_names.iter().enumerate() {
            if cix == id_col_ix || cix == seq_col_ix { continue; }
            let v = col_data[cix].get(d).cloned().unwrap_or(serde_json::Value::Null);
            if !v.is_null() {
                source_map.insert(name.clone(), v);
            }
        }
        let mut doc = serde_json::Map::new();
        doc.insert("_id".into(), col_data[id_col_ix].get(d).cloned().unwrap_or(serde_json::Value::Null));
        doc.insert("_seq_no".into(), col_data[seq_col_ix].get(d).cloned().unwrap_or(serde_json::Value::Null));
        doc.insert("_source".into(), serde_json::Value::Object(source_map));
        out_docs.push(serde_json::Value::Object(doc));
    }

    serde_json::to_vec(&out_docs)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("v2 reassemble: {e}")))
}

// ── Column-level helpers ─────────────────────────────────────────────────

fn col_is_numeric(col: &[serde_json::Value]) -> bool {
    let mut saw_num = false;
    for v in col {
        if v.is_null() { continue; }
        if !v.is_number() { return false; }
        saw_num = true;
    }
    saw_num
}

fn all_scalar_dict_entries(entries: &[serde_json::Value]) -> bool {
    entries.iter().all(|v| v.is_null() || v.is_string() || v.is_number() || v.is_boolean())
}

/// Build a dictionary representation `(entries, ids)` where:
/// * `entries[0..entries.len()]` are unique values in first-seen order
/// * `ids[i] = index_of_value(col[i])`; `null` values get id = `entries.len()` (reserved)
///
/// Returns `None` if the column contains any non-scalar (object/array) value
/// or if the unique-count explodes beyond the cap.
fn dict_encode_column(col: &[serde_json::Value]) -> Option<(Vec<serde_json::Value>, Vec<u32>)> {
    let mut map: HashMap<String, u32> = HashMap::new();
    let mut entries: Vec<serde_json::Value> = Vec::new();
    let mut ids: Vec<u32> = Vec::with_capacity(col.len());
    for v in col {
        if v.is_null() {
            ids.push(u32::MAX); // resolved to "null id" later
            continue;
        }
        if v.is_object() || v.is_array() { return None; }
        // Key values by their serialized form — fast enough, correct for
        // string / number / bool.
        let k = v.to_string();
        if let Some(&id) = map.get(&k) {
            ids.push(id);
        } else {
            let id = entries.len() as u32;
            entries.push(v.clone());
            map.insert(k, id);
            ids.push(id);
        }
        if entries.len() > DICT_MAX_CARDINALITY * 4 {
            return None; // way above the payload-worthy cap
        }
    }
    // Replace u32::MAX with the reserved-null id.
    let null_id = entries.len() as u32;
    for id in ids.iter_mut() {
        if *id == u32::MAX { *id = null_id; }
    }
    Some((entries, ids))
}

/// For a numeric target column, find the index of an earlier dict-encoded
/// column (if any) whose dict ids deterministically predict the target at
/// ≥ `CROSS_DEP_MIN_DETERMINISM` fraction of rows.
fn best_cross_dep_source(
    dict_encoded: &[Option<(Vec<serde_json::Value>, Vec<u32>)>],
    target_ix: usize,
    target_col: &[serde_json::Value],
) -> Option<usize> {
    for (src_ix, de) in dict_encoded.iter().enumerate() {
        if src_ix == target_ix { continue; }
        let (entries, ids) = match de { Some(e) => e, None => continue };
        if !all_scalar_dict_entries(entries) { continue; }
        if entries.len() < 2 { continue; } // constant source: useless
        if entries.len() > DICT_MAX_CARDINALITY { continue; }

        // Build mode table: mode_values[src_id] = most frequent target int
        let mut mode_tally: HashMap<u32, HashMap<i64, usize>> = HashMap::new();
        for (row, t_val) in target_col.iter().enumerate() {
            let Some(t) = t_val.as_i64().or_else(|| t_val.as_f64().map(|f| f as i64)) else { continue };
            let sid = ids[row];
            *mode_tally.entry(sid).or_default().entry(t).or_insert(0) += 1;
        }
        // Count deterministic rows: row matches mode[src_id].
        let mut mode_pick: HashMap<u32, i64> = HashMap::new();
        for (sid, tally) in &mode_tally {
            if let Some((v, _)) = tally.iter().max_by_key(|(_, c)| *c) {
                mode_pick.insert(*sid, *v);
            }
        }
        let mut hits = 0usize;
        let mut total_numeric = 0usize;
        for (row, t_val) in target_col.iter().enumerate() {
            let Some(t) = t_val.as_i64().or_else(|| t_val.as_f64().map(|f| f as i64)) else { continue };
            total_numeric += 1;
            if mode_pick.get(&ids[row]) == Some(&t) { hits += 1; }
        }
        if total_numeric == 0 { continue; }
        let det = hits as f64 / total_numeric as f64;
        if det >= CROSS_DEP_MIN_DETERMINISM {
            return Some(src_ix);
        }
    }
    None
}

fn try_encode_constant(col: &[serde_json::Value]) -> Option<Vec<u8>> {
    let first = col.iter().find(|v| !v.is_null())?;
    if col.iter().all(|v| v == first || v.is_null()) && !col.iter().any(|v| v.is_null()) {
        // Only encode as constant when there are no nulls (keep the codec simple).
        return Some(serde_json::to_vec(first).ok()?);
    }
    None
}

fn decode_constant(payload: &[u8], num_docs: usize) -> Result<Vec<serde_json::Value>> {
    let v: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("constant decode: {e}")))?;
    Ok(vec![v; num_docs])
}

fn encode_dict_bitpack(entries: &[serde_json::Value], ids: &[u32]) -> Vec<u8> {
    let dict_count = entries.len();
    // Reserve one extra id for null → dict_count (which might be past the
    // packed range, so we allocate `bit_width` large enough for dict_count
    // inclusive).
    let max_id = dict_count as u32;
    let bit_width = if max_id == 0 { 1 } else { 32 - max_id.leading_zeros() as u8 };

    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(dict_count as u32).unwrap();
    out.push(bit_width);

    // Dict entries as zstd(json array).
    let dict_json = serde_json::to_vec(entries).unwrap_or_default();
    let dict_zstd = zstd::encode_all(Cursor::new(&dict_json), STORED_ZSTD_LEVEL).unwrap_or(dict_json.clone());
    out.write_u32::<LittleEndian>(dict_zstd.len() as u32).unwrap();
    out.extend_from_slice(&dict_zstd);

    // Bit-packed ids.
    let packed = bitpack_u32(ids, bit_width);
    // zstd over the bit-packed stream — gives another 20-40 % on log
    // data because repeated ids cluster.
    let packed_zstd = zstd::encode_all(Cursor::new(&packed), STORED_ZSTD_LEVEL).unwrap_or(packed.clone());
    out.write_u32::<LittleEndian>(ids.len() as u32).unwrap();
    out.write_u32::<LittleEndian>(packed_zstd.len() as u32).unwrap();
    out.extend_from_slice(&packed_zstd);
    out
}

fn decode_dict_bitpack(payload: &[u8], num_docs: usize) -> Result<Vec<serde_json::Value>> {
    let mut cur = Cursor::new(payload);
    let dict_count = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict_count: {e}")))? as usize;
    let bit_width = cur.read_u8()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("bit_width: {e}")))?;

    let dict_zstd_len = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict_zstd_len: {e}")))? as usize;
    let pos = cur.position() as usize;
    if payload.len() < pos + dict_zstd_len {
        return Err(StorageError::Other(anyhow::anyhow!("dict bitpack truncated")));
    }
    let dict_json = zstd::decode_all(&payload[pos..pos + dict_zstd_len])
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict zstd decode: {e}")))?;
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&dict_json)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("dict json decode: {e}")))?;
    cur.set_position((pos + dict_zstd_len) as u64);

    let ids_len = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("ids_len: {e}")))? as usize;
    if ids_len != num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict bitpack ids_len {} != num_docs {}", ids_len, num_docs
        )));
    }
    let packed_zstd_len = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("packed_zstd_len: {e}")))? as usize;
    let pos = cur.position() as usize;
    if payload.len() < pos + packed_zstd_len {
        return Err(StorageError::Other(anyhow::anyhow!("dict bitpack packed truncated")));
    }
    let packed = zstd::decode_all(&payload[pos..pos + packed_zstd_len])
        .map_err(|e| StorageError::Other(anyhow::anyhow!("packed zstd decode: {e}")))?;

    let ids = bitunpack_u32(&packed, bit_width, num_docs);
    let null_id = dict_count as u32;
    let values: Vec<serde_json::Value> = ids
        .into_iter()
        .map(|id| {
            if id == null_id { serde_json::Value::Null }
            else { entries.get(id as usize).cloned().unwrap_or(serde_json::Value::Null) }
        })
        .collect();
    Ok(values)
}

fn encode_cross_dep(
    target_col: &[serde_json::Value],
    src_ix: usize,
    dict_encoded: &[Option<(Vec<serde_json::Value>, Vec<u32>)>],
) -> (Vec<u8>, bool) {
    let (_src_entries, src_ids) = match &dict_encoded[src_ix] {
        Some(e) => e,
        None => return (Vec::new(), false),
    };

    // Recompute the mode table for just the subset.  We need:
    //   mode[src_id] = most frequent target i64
    //   exceptions  = list of (doc_ord, target_i64) where row deviates
    let dict_count = match &dict_encoded[src_ix] {
        Some((e, _)) => e.len(),
        None => return (Vec::new(), false),
    };
    let mut tally: Vec<HashMap<i64, u32>> = vec![HashMap::new(); dict_count + 1];
    for (row, t) in target_col.iter().enumerate() {
        let Some(tv) = t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)) else { continue };
        let sid = src_ids[row] as usize;
        *tally[sid.min(dict_count)].entry(tv).or_insert(0) += 1;
    }
    let mut mode_values: Vec<i64> = vec![i64::MIN; dict_count];
    for (sid, t) in tally.iter().enumerate().take(dict_count) {
        if let Some((v, _)) = t.iter().max_by_key(|(_, c)| *c) {
            mode_values[sid] = *v;
        }
    }

    let mut exceptions: Vec<(u32, i64)> = Vec::new();
    for (row, t) in target_col.iter().enumerate() {
        let Some(tv) = t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)) else {
            // Null / non-numeric — emit as exception with sentinel i64::MIN+1 ? No,
            // simpler: mark with i64::MIN via sentinel tuple, but we cannot
            // distinguish, so encode using i64::MIN+1 meaning "null" in the decoder.
            exceptions.push((row as u32, i64::MIN + 1));
            continue;
        };
        let sid = src_ids[row] as usize;
        let expected = if sid < dict_count { mode_values[sid] } else { i64::MIN };
        if expected != tv {
            exceptions.push((row as u32, tv));
        }
    }

    // Serialise.
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(src_ix as u32).unwrap();
    out.write_u32::<LittleEndian>(dict_count as u32).unwrap();
    for &v in &mode_values { out.write_i64::<LittleEndian>(v).unwrap(); }
    out.write_u32::<LittleEndian>(exceptions.len() as u32).unwrap();
    // delta-encode doc_ords
    let mut prev_ord = 0u32;
    for (ord, val) in &exceptions {
        let d = ord.wrapping_sub(prev_ord);
        write_varint(&mut out, d as u64);
        write_zigzag_i64(&mut out, *val);
        prev_ord = *ord;
    }
    // zstd the whole thing.
    let compressed = zstd::encode_all(Cursor::new(&out), STORED_ZSTD_LEVEL).unwrap_or(out.clone());
    // Save the smaller of raw vs zstd.
    let mut final_out = Vec::with_capacity(1 + compressed.len().max(out.len()));
    if compressed.len() + 1 < out.len() {
        final_out.push(1);
        final_out.extend_from_slice(&compressed);
    } else {
        final_out.push(0);
        final_out.extend_from_slice(&out);
    }
    (final_out, true)
}

fn decode_cross_dep(
    payload: &[u8],
    num_docs: usize,
    col_data: &[Vec<serde_json::Value>],
    col_name_to_ix: &HashMap<String, usize>,
) -> Result<Vec<serde_json::Value>> {
    if payload.is_empty() {
        return Err(StorageError::Other(anyhow::anyhow!("cross_dep empty")));
    }
    let flag = payload[0];
    let body = if flag == 1 {
        zstd::decode_all(&payload[1..])
            .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep zstd decode: {e}")))?
    } else {
        payload[1..].to_vec()
    };
    let mut cur = Cursor::new(&body[..]);
    let src_ix = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep src_ix: {e}")))? as usize;
    let _ = col_name_to_ix; // not strictly needed yet
    let dict_count = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep dict_count: {e}")))? as usize;
    let mut mode_values: Vec<i64> = Vec::with_capacity(dict_count);
    for _ in 0..dict_count {
        mode_values.push(cur.read_i64::<LittleEndian>()
            .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep mode: {e}")))?);
    }
    let exc_count = cur.read_u32::<LittleEndian>()
        .map_err(|e| StorageError::Other(anyhow::anyhow!("cross_dep exc_count: {e}")))? as usize;

    // Rebuild the source column's dict ids by re-running `dict_encode_column`
    // on the already-decoded source column.
    let src_col = col_data.get(src_ix)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep src missing")))?;
    if src_col.len() != num_docs {
        // Source column was itself a CROSS_DEP and not yet resolved.  Caller
        // should have resolved RHS first.  Bail with a clear error.
        return Err(StorageError::Other(anyhow::anyhow!("cross_dep src not resolved")));
    }
    let (_src_entries, src_ids) = dict_encode_column(src_col)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep re-dict src failed")))?;

    // Exceptions.
    let mut exc: Vec<(u32, i64)> = Vec::with_capacity(exc_count);
    let mut pos = cur.position() as usize;
    let mut prev_ord = 0u32;
    for _ in 0..exc_count {
        let delta = read_varint(&body, &mut pos) as u32;
        let val = read_zigzag_i64(&body, &mut pos);
        let ord = prev_ord.wrapping_add(delta);
        exc.push((ord, val));
        prev_ord = ord;
    }

    // Materialise.
    let mut result: Vec<serde_json::Value> = Vec::with_capacity(num_docs);
    let mut exc_ix = 0;
    for row in 0..num_docs {
        if exc_ix < exc.len() && exc[exc_ix].0 as usize == row {
            let v = exc[exc_ix].1;
            exc_ix += 1;
            if v == i64::MIN + 1 {
                result.push(serde_json::Value::Null);
            } else {
                result.push(serde_json::Value::Number(v.into()));
            }
            continue;
        }
        let sid = src_ids.get(row).copied().unwrap_or(0) as usize;
        let v = if sid < dict_count { mode_values[sid] } else { i64::MIN };
        if v == i64::MIN {
            result.push(serde_json::Value::Null);
        } else {
            result.push(serde_json::Value::Number(v.into()));
        }
    }
    Ok(result)
}

fn decode_raw_json(payload: &[u8]) -> Result<Vec<serde_json::Value>> {
    let raw = zstd::decode_all(payload)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("raw zstd decode: {e}")))?;
    serde_json::from_slice(&raw)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("raw json decode: {e}")))
}

fn decode_lz4_json(payload: &[u8]) -> Result<Vec<serde_json::Value>> {
    let raw = lz4_flex::decompress_size_prepended(payload)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("lz4 decode: {e}")))?;
    serde_json::from_slice(&raw)
        .map_err(|e| StorageError::Other(anyhow::anyhow!("lz4 json decode: {e}")))
}

// ── Bit-pack helpers ──────────────────────────────────────────────────────

fn bitpack_u32(ids: &[u32], bit_width: u8) -> Vec<u8> {
    if bit_width == 0 {
        return Vec::new();
    }
    let bw = bit_width as usize;
    let total_bits = ids.len() * bw;
    let total_bytes = (total_bits + 7) / 8;
    let mut out = vec![0u8; total_bytes];
    let mut bit_pos = 0usize;
    for &id in ids {
        let val = id as u64 & ((1u64 << bw) - 1);
        let byte_ix = bit_pos / 8;
        let shift = bit_pos % 8;
        let combined = val << shift;
        // Write up to 64 bits starting at byte_ix.
        let n_bytes = ((bw + shift + 7) / 8).min(total_bytes - byte_ix);
        for i in 0..n_bytes {
            out[byte_ix + i] |= ((combined >> (i * 8)) & 0xFF) as u8;
        }
        bit_pos += bw;
    }
    out
}

fn bitunpack_u32(packed: &[u8], bit_width: u8, count: usize) -> Vec<u32> {
    if bit_width == 0 {
        return vec![0; count];
    }
    let bw = bit_width as usize;
    let mask = (1u64 << bw) - 1;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let bit_pos = i * bw;
        let byte_ix = bit_pos / 8;
        let shift = bit_pos % 8;
        let mut combined: u64 = 0;
        let n_bytes = ((bw + shift + 7) / 8).min(packed.len() - byte_ix);
        for j in 0..n_bytes {
            combined |= (packed[byte_ix + j] as u64) << (j * 8);
        }
        let val = (combined >> shift) & mask;
        out.push(val as u32);
    }
    out
}

// ── Varint / zigzag helpers ───────────────────────────────────────────────

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 { out.push(b); break; }
        out.push(b | 0x80);
    }
}

fn read_varint(data: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0u32;
    while *pos < data.len() {
        let b = data[*pos];
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 { return v; }
        shift += 7;
    }
    v
}

fn write_zigzag_i64(out: &mut Vec<u8>, v: i64) {
    let z = ((v << 1) ^ (v >> 63)) as u64;
    write_varint(out, z);
}

fn read_zigzag_i64(data: &[u8], pos: &mut usize) -> i64 {
    let z = read_varint(data, pos);
    ((z >> 1) as i64) ^ -((z & 1) as i64)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lz4_roundtrip() {
        let raw = br#"[{"_id":"a","_source":{"msg":"hello world"}}]"#;
        let encoded = encode_stored_lz4(raw);
        assert_eq!(&encoded[..4], STORED_LZ4_MAGIC);
        let decoded = decode_stored(&encoded).unwrap();
        assert_eq!(&decoded, raw);
    }

    #[test]
    fn uncompressed_passthrough() {
        let raw = br#"[{"_id":"a"}]"#;
        let decoded = decode_stored(raw).unwrap();
        assert_eq!(&decoded, raw);
    }

    #[test]
    fn v2_roundtrip_nginx_like() {
        // Synthesise 256 nginx-like docs with strong determinism: URL → status, URL → bytes.
        let mut docs = Vec::new();
        let urls = ["/a.png", "/b.png", "/c.js", "/d.html", "/api/x"];
        let statuses = [200, 200, 200, 404, 500];
        let sizes = [1024, 2048, 4096, 0, 12345];
        for i in 0..256 {
            let u = i % urls.len();
            docs.push(json!({
                "_id": format!("doc-{}", i),
                "_seq_no": i as u64,
                "_source": {
                    "path": urls[u],
                    "status": statuses[u],
                    "bytes": sizes[u],
                    "method": if i % 7 == 0 { "POST" } else { "GET" },
                    "ip": format!("10.0.{}.{}", i / 256, i % 256),
                    "ua": "Mozilla/5.0 Chrome/120.0",
                    "@timestamp": 1_700_000_000u64 + i as u64,
                }
            }));
        }
        let raw = serde_json::to_vec(&docs).unwrap();
        let encoded = encode_stored_v2(&raw);
        // V2 should have meaningfully reduced the payload.
        assert!(encoded.len() < raw.len() / 2,
                "v2 encoded {} not < half of raw {}", encoded.len(), raw.len());

        let decoded = decode_stored(&encoded).unwrap();
        let round: Vec<serde_json::Value> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(round.len(), docs.len());
        // Spot-check a few docs.
        for i in [0, 1, 5, 42, 128, 255] {
            assert_eq!(round[i]["_id"], docs[i]["_id"], "id mismatch at {i}");
            assert_eq!(round[i]["_source"]["status"], docs[i]["_source"]["status"],
                       "status mismatch at {i}");
            assert_eq!(round[i]["_source"]["path"], docs[i]["_source"]["path"],
                       "path mismatch at {i}");
            assert_eq!(round[i]["_source"]["bytes"], docs[i]["_source"]["bytes"],
                       "bytes mismatch at {i}");
        }
    }

    #[test]
    fn v2_falls_back_on_tiny_input() {
        let raw = br#"[{"_id":"a","_seq_no":1,"_source":{"m":"x"}}]"#;
        let encoded = encode_stored_v2(raw);
        // Small input: should fall back to v1 LZ4 magic.
        assert_eq!(&encoded[..4], STORED_LZ4_MAGIC);
    }

    #[test]
    fn bitpack_roundtrip() {
        let ids = vec![0u32, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3];
        let packed = bitpack_u32(&ids, 3);
        let unpacked = bitunpack_u32(&packed, 3, ids.len());
        assert_eq!(unpacked, ids);
    }
}
