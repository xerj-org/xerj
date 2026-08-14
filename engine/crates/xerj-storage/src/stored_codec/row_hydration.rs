use super::*;
use serde::de::Deserializer as _;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};

/// Logical work performed by row-selective hydration.
///
/// These fields are not allocator or RSS measurements. In particular,
/// decompressed buffers are counted by byte length while JSON values are
/// counted as objects, so the fields must never be summed into "memory used".
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StoredV2RowHydrationStats {
    pub encoded_rows_visited: usize,
    pub selected_json_values: usize,
    pub selected_dictionary_entries: usize,
    pub decompressed_buffer_bytes: usize,
    pub output_values_cloned: usize,
}

#[derive(Debug, PartialEq)]
pub struct StoredV2HydratedRow {
    pub ordinal: usize,
    pub id: serde_json::Value,
    pub seq_no: serde_json::Value,
    pub source: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, PartialEq)]
pub enum StoredV2RowHydrationResult {
    NotV2,
    Hydrated {
        rows: Vec<StoredV2HydratedRow>,
        stats: StoredV2RowHydrationStats,
    },
    /// Valid storage whose dependency shape cannot be hydrated selectively.
    /// The caller must use the compatibility path for the whole request.
    UnsupportedDependencyShape {
        column: String,
    },
}

type IdentityColumns = (usize, Vec<serde_json::Value>, Vec<serde_json::Value>);

pub(super) fn decode_v2_identity_columns_controlled(
    bytes: &[u8],
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<StoredDecodeRun<Option<IdentityColumns>>> {
    let directory = parse_v2_directory(&bytes[4..])?;
    let id_index = directory
        .columns
        .iter()
        .position(|column| column.name == "__id")
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 missing __id column")))?;
    let seq_index = directory
        .columns
        .iter()
        .position(|column| column.name == "__seq_no")
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 missing __seq_no column")))?;
    let mut selected = Vec::new();
    selected
        .try_reserve_exact(directory.num_docs)
        .map_err(|error| {
            StorageError::Other(anyhow::anyhow!(
                "cannot reserve {} identity row ordinals: {error}",
                directory.num_docs
            ))
        })?;
    selected.extend(0..directory.num_docs);
    let mut stats = StoredV2RowHydrationStats::default();
    let mut dict_ids = HashMap::new();
    let mut decode =
        |index| -> Result<std::result::Result<Vec<serde_json::Value>, Option<String>>> {
            if decode_cancelled(checkpoint, StoredDecodePhase::BeforeColumn, index, 0) {
                return Ok(Err(None));
            }
            let result = select_column_rows(
                index,
                &directory,
                &selected,
                &mut stats,
                &mut dict_ids,
                checkpoint,
            )?;
            if result.is_ok()
                && decode_cancelled(
                    checkpoint,
                    StoredDecodePhase::AfterColumn,
                    index,
                    directory.num_docs,
                )
            {
                return Ok(Err(None));
            }
            Ok(result)
        };
    let ids = match decode(id_index)? {
        Ok(values) => values,
        Err(None) => return Ok(StoredDecodeRun::Cancelled),
        Err(Some(_)) => return Ok(StoredDecodeRun::Complete(None)),
    };
    let seq_nos = match decode(seq_index)? {
        Ok(values) => values,
        Err(None) => return Ok(StoredDecodeRun::Cancelled),
        Err(Some(_)) => return Ok(StoredDecodeRun::Complete(None)),
    };
    Ok(StoredDecodeRun::Complete(Some((
        directory.num_docs,
        ids,
        seq_nos,
    ))))
}

struct SelectedRowsVisitor<'a> {
    selected: &'a [usize],
    expected_rows: usize,
    column_index: usize,
    checkpoint: &'a mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
    cancelled: &'a Cell<bool>,
}

impl<'de> serde::de::Visitor<'de> for SelectedRowsVisitor<'_> {
    type Value = Vec<serde_json::Value>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON array matching the declared stored row count")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut selected_index = 0usize;
        let mut values = Vec::with_capacity(self.selected.len());
        for row in 0..self.expected_rows {
            if self.selected.get(selected_index).copied() == Some(row) {
                values.push(
                    sequence
                        .next_element::<serde_json::Value>()?
                        .ok_or_else(|| serde::de::Error::custom("stored column ended early"))?,
                );
                selected_index += 1;
            } else {
                sequence
                    .next_element::<serde::de::IgnoredAny>()?
                    .ok_or_else(|| serde::de::Error::custom("stored column ended early"))?;
            }
            let processed = row + 1;
            if processed.is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
                && (self.checkpoint)(StoredDecodeCheckpoint {
                    phase: StoredDecodePhase::Rows,
                    column_index: self.column_index,
                    rows_processed: processed,
                })
                .is_break()
            {
                self.cancelled.set(true);
                return Err(serde::de::Error::custom("stored decode cancelled"));
            }
        }
        if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "stored column exceeds declared row count",
            ));
        }
        Ok(values)
    }
}

fn select_json_rows<R: std::io::Read>(
    reader: R,
    selected: &[usize],
    expected_rows: usize,
    context: &str,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Vec<serde_json::Value>, ()>> {
    let cancelled = Cell::new(false);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let values = match deserializer.deserialize_seq(SelectedRowsVisitor {
        selected,
        expected_rows,
        column_index,
        checkpoint,
        cancelled: &cancelled,
    }) {
        Ok(values) => values,
        Err(_) if cancelled.get() => return Ok(Err(())),
        Err(error) => {
            return Err(StorageError::Other(anyhow::anyhow!("{context}: {error}")));
        }
    };
    deserializer
        .end()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("{context}: {error}")))?;
    Ok(Ok(values))
}

fn unpack_id(packed: &[u8], bit_width: u8, ordinal: usize) -> Option<u32> {
    let width = bit_width as usize;
    if !(1..=32).contains(&width) {
        return None;
    }
    let bit_position = ordinal.checked_mul(width)?;
    let byte_position = bit_position / 8;
    let shift = bit_position % 8;
    let needed = (width + shift).div_ceil(8);
    let bytes = packed.get(byte_position..byte_position.checked_add(needed)?)?;
    let mut word = 0u64;
    for (offset, byte) in bytes.iter().enumerate() {
        word |= (*byte as u64) << (offset * 8);
    }
    Some(((word >> shift) & ((1u64 << width) - 1)) as u32)
}

struct SelectedDict {
    values: Vec<serde_json::Value>,
    ids: Vec<u32>,
    selected_entries: usize,
    decompressed_bytes: usize,
}

fn decode_dict_rows(
    payload: &[u8],
    selected: &[usize],
    num_docs: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<SelectedDict, ()>> {
    let mut cursor = Cursor::new(payload);
    let dict_count = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("dict_count: {error}")))?
        as usize;
    if dict_count > num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict_count {dict_count} exceeds num_docs {num_docs}"
        )));
    }
    let bit_width = cursor
        .read_u8()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("bit_width: {error}")))?;
    if !(1..=32).contains(&bit_width) {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict invalid bit_width {bit_width}"
        )));
    }
    let dict_len = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("dict_zstd_len: {error}")))?
        as usize;
    let dict_start = cursor.position() as usize;
    let dict_end = dict_start
        .checked_add(dict_len)
        .filter(|end| *end <= payload.len())
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict payload truncated")))?;
    cursor.set_position(dict_end as u64);
    let ids_len = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("ids_len: {error}")))?
        as usize;
    if ids_len != num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "dict ids_len {ids_len} != num_docs {num_docs}"
        )));
    }
    let packed_len = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("packed_zstd_len: {error}")))?
        as usize;
    let packed_start = cursor.position() as usize;
    let packed_end = packed_start
        .checked_add(packed_len)
        .filter(|end| *end == payload.len())
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict packed truncated/trailing")))?;
    let expected_packed_len = num_docs
        .checked_mul(bit_width as usize)
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict packed length overflow")))?
        .div_ceil(8);
    let packed = decode_zstd_exact(
        &payload[packed_start..packed_end],
        expected_packed_len,
        "packed zstd",
    )?;
    let used_bits = num_docs * bit_width as usize;
    if !used_bits.is_multiple_of(8) {
        let used_in_last = used_bits % 8;
        let padding_mask = !((1u8 << used_in_last) - 1);
        if packed.last().is_some_and(|byte| byte & padding_mask != 0) {
            return Err(StorageError::Other(anyhow::anyhow!(
                "dict non-zero packed padding"
            )));
        }
    }
    let null_id = dict_count as u32;
    // Validate every encoded id without constructing per-row Values.
    for ordinal in 0..num_docs {
        let id = unpack_id(&packed, bit_width, ordinal)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict id truncated")))?;
        if id > null_id {
            return Err(StorageError::Other(anyhow::anyhow!(
                "dict id {id} outside dictionary"
            )));
        }
        if (ordinal + 1).is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
            && checkpoint(StoredDecodeCheckpoint {
                phase: StoredDecodePhase::Rows,
                column_index,
                rows_processed: ordinal + 1,
            })
            .is_break()
        {
            return Ok(Err(()));
        }
    }
    let ids: Vec<u32> = selected
        .iter()
        .map(|ordinal| {
            unpack_id(&packed, bit_width, *ordinal)
                .ok_or_else(|| StorageError::Other(anyhow::anyhow!("dict selected id truncated")))
        })
        .collect::<Result<_>>()?;
    let mut needed: Vec<usize> = ids
        .iter()
        .copied()
        .filter(|id| *id != null_id)
        .map(|id| id as usize)
        .collect();
    needed.sort_unstable();
    needed.dedup();
    let decoder = zstd::stream::read::Decoder::new(&payload[dict_start..dict_end])
        .map_err(|error| StorageError::Other(anyhow::anyhow!("dict zstd: {error}")))?;
    let entries = match select_json_rows(
        decoder,
        &needed,
        dict_count,
        "dict json",
        column_index,
        checkpoint,
    )? {
        Ok(entries) => entries,
        Err(()) => return Ok(Err(())),
    };
    let entries: HashMap<usize, serde_json::Value> = needed.iter().copied().zip(entries).collect();
    let values = ids
        .iter()
        .map(|id| {
            if *id == null_id {
                Ok(serde_json::Value::Null)
            } else {
                entries.get(&(*id as usize)).cloned().ok_or_else(|| {
                    StorageError::Other(anyhow::anyhow!("selected dict entry missing"))
                })
            }
        })
        .collect::<Result<_>>()?;
    Ok(Ok(SelectedDict {
        values,
        ids,
        selected_entries: needed.len(),
        decompressed_bytes: packed.len(),
    }))
}

fn decode_cross_dep_rows(
    column: &V2ColumnRef<'_>,
    selected: &[usize],
    source_ids: &[u32],
    num_docs: usize,
    stats: &mut StoredV2RowHydrationStats,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Vec<serde_json::Value>, ()>> {
    let body = decode_cross_dep_body(column.payload, num_docs)?;
    stats.decompressed_buffer_bytes += body.len();
    let mut cursor = Cursor::new(body.as_slice());
    let _source_index = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("cross_dep src_ix: {error}")))?;
    let dict_count = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("cross_dep dict_count: {error}")))?
        as usize;
    if dict_count > num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep dict_count {dict_count} exceeds num_docs {num_docs}"
        )));
    }
    let mut modes = Vec::with_capacity(dict_count);
    for _ in 0..dict_count {
        modes.push(
            cursor
                .read_i64::<LittleEndian>()
                .map_err(|error| StorageError::Other(anyhow::anyhow!("cross_dep mode: {error}")))?,
        );
    }
    let exception_count = cursor
        .read_u32::<LittleEndian>()
        .map_err(|error| StorageError::Other(anyhow::anyhow!("cross_dep exc_count: {error}")))?
        as usize;
    if exception_count > num_docs {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep exc_count {exception_count} exceeds num_docs {num_docs}"
        )));
    }
    let wanted: HashSet<u32> = selected.iter().map(|ordinal| *ordinal as u32).collect();
    let mut exceptions = HashMap::with_capacity(wanted.len().min(exception_count));
    let mut position = cursor.position() as usize;
    let mut previous = 0u32;
    for exception_index in 0..exception_count {
        let delta = u32::try_from(read_varint(&body, &mut position)?)
            .map_err(|_| StorageError::Other(anyhow::anyhow!("cross_dep ordinal overflow")))?;
        let value = read_zigzag_i64(&body, &mut position)?;
        let ordinal = previous
            .checked_add(delta)
            .ok_or_else(|| StorageError::Other(anyhow::anyhow!("cross_dep ordinal overflow")))?;
        if ordinal as usize >= num_docs || (exception_index > 0 && ordinal <= previous) {
            return Err(StorageError::Other(anyhow::anyhow!(
                "cross_dep invalid exception ordinal {ordinal}"
            )));
        }
        if wanted.contains(&ordinal) {
            exceptions.insert(ordinal, value);
        }
        previous = ordinal;
        if (exception_index + 1).is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
            && checkpoint(StoredDecodeCheckpoint {
                phase: StoredDecodePhase::Rows,
                column_index,
                rows_processed: exception_index + 1,
            })
            .is_break()
        {
            return Ok(Err(()));
        }
    }
    if position != body.len() {
        return Err(StorageError::Other(anyhow::anyhow!(
            "cross_dep trailing exception bytes"
        )));
    }
    Ok(Ok(selected
        .iter()
        .zip(source_ids)
        .map(|(ordinal, source_id)| {
            if let Some(value) = exceptions.get(&(*ordinal as u32)).copied() {
                if value == i64::MIN + 1 {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::Number(value.into())
                }
            } else {
                let value = modes.get(*source_id as usize).copied().unwrap_or(i64::MIN);
                if value == i64::MIN {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::Number(value.into())
                }
            }
        })
        .collect()))
}

fn repeat_constant_rows_controlled(
    value: serde_json::Value,
    output_count: usize,
    column_index: usize,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Vec<serde_json::Value>, ()>> {
    let mut values = Vec::new();
    values.try_reserve_exact(output_count).map_err(|error| {
        StorageError::Other(anyhow::anyhow!(
            "cannot reserve {output_count} selected constant values: {error}"
        ))
    })?;
    for output_index in 0..output_count {
        values.push(value.clone());
        let processed = output_index + 1;
        if processed.is_multiple_of(STORED_DECODE_CHECK_INTERVAL)
            && checkpoint(StoredDecodeCheckpoint {
                phase: StoredDecodePhase::Rows,
                column_index,
                rows_processed: processed,
            })
            .is_break()
        {
            return Ok(Err(()));
        }
    }
    Ok(Ok(values))
}

fn select_column_rows(
    column_index: usize,
    directory: &V2Directory<'_>,
    selected: &[usize],
    stats: &mut StoredV2RowHydrationStats,
    dict_ids: &mut HashMap<usize, Vec<u32>>,
    checkpoint: &mut dyn FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
) -> Result<std::result::Result<Vec<serde_json::Value>, Option<String>>> {
    let column = &directory.columns[column_index];
    let values = match column.codec {
        ColCodec::RawJson => {
            stats.encoded_rows_visited += directory.num_docs;
            let decoder = zstd::stream::read::Decoder::new(column.payload)
                .map_err(|error| StorageError::Other(anyhow::anyhow!("raw zstd: {error}")))?;
            match select_json_rows(
                decoder,
                selected,
                directory.num_docs,
                "raw json",
                column_index,
                checkpoint,
            )? {
                Ok(values) => values,
                Err(()) => return Ok(Err(None)),
            }
        }
        ColCodec::Lz4Json => {
            let raw = lz4_flex::decompress_size_prepended(column.payload)
                .map_err(|error| StorageError::Other(anyhow::anyhow!("lz4 decode: {error}")))?;
            stats.decompressed_buffer_bytes += raw.len();
            stats.encoded_rows_visited += directory.num_docs;
            match select_json_rows(
                raw.as_slice(),
                selected,
                directory.num_docs,
                "lz4 json",
                column_index,
                checkpoint,
            )? {
                Ok(values) => values,
                Err(()) => return Ok(Err(None)),
            }
        }
        ColCodec::Constant => {
            let value: serde_json::Value = serde_json::from_slice(column.payload)
                .map_err(|error| StorageError::Other(anyhow::anyhow!("constant: {error}")))?;
            match repeat_constant_rows_controlled(value, selected.len(), column_index, checkpoint)?
            {
                Ok(values) => values,
                Err(()) => return Ok(Err(None)),
            }
        }
        ColCodec::DictBitpack => {
            let decoded = match decode_dict_rows(
                column.payload,
                selected,
                directory.num_docs,
                column_index,
                checkpoint,
            )? {
                Ok(decoded) => decoded,
                Err(()) => return Ok(Err(None)),
            };
            stats.selected_dictionary_entries += decoded.selected_entries;
            stats.decompressed_buffer_bytes += decoded.decompressed_bytes;
            dict_ids.insert(column_index, decoded.ids);
            decoded.values
        }
        ColCodec::CrossDep => {
            let source_index = cross_dep_source_index(column.payload, directory.num_docs)?;
            let Some(source) = directory.columns.get(source_index) else {
                return Err(StorageError::Other(anyhow::anyhow!(
                    "cross_dep source index {source_index} out of range"
                )));
            };
            // Current encoder guarantees CROSS_DEP sources remain DICT. Valid
            // historical/handcrafted chains use the compatibility decoder.
            if source.codec != ColCodec::DictBitpack {
                // Unsupported is not an escape hatch for malformed storage:
                // validate this column's complete body and exception stream
                // before asking the caller to use the compatibility path.
                let placeholder_source_ids = vec![0; selected.len()];
                match decode_cross_dep_rows(
                    column,
                    selected,
                    &placeholder_source_ids,
                    directory.num_docs,
                    stats,
                    column_index,
                    checkpoint,
                )? {
                    Ok(_) => {}
                    Err(()) => return Ok(Err(None)),
                }
                return Ok(Err(Some(column.name.to_string())));
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = dict_ids.entry(source_index) {
                let decoded = match decode_dict_rows(
                    source.payload,
                    selected,
                    directory.num_docs,
                    source_index,
                    checkpoint,
                )? {
                    Ok(decoded) => decoded,
                    Err(()) => return Ok(Err(None)),
                };
                stats.selected_dictionary_entries += decoded.selected_entries;
                stats.decompressed_buffer_bytes += decoded.decompressed_bytes;
                entry.insert(decoded.ids);
            }
            match decode_cross_dep_rows(
                column,
                selected,
                &dict_ids[&source_index],
                directory.num_docs,
                stats,
                column_index,
                checkpoint,
            )? {
                Ok(values) => values,
                Err(()) => return Ok(Err(None)),
            }
        }
    };
    stats.selected_json_values += values.len();
    Ok(Ok(values))
}

/// Reconstruct only selected ZBS2 documents without the canonical full decode.
pub fn decode_stored_v2_rows(
    bytes: &[u8],
    ordinals: &[usize],
) -> Result<StoredV2RowHydrationResult> {
    match decode_stored_v2_rows_projected_controlled(bytes, ordinals, None, |_| {
        std::ops::ControlFlow::Continue(())
    })? {
        StoredDecodeRun::Complete(result) => Ok(result),
        StoredDecodeRun::Cancelled => unreachable!("non-cancelling compatibility wrapper"),
    }
}

/// Reconstruct selected ZBS2 rows and only the requested top-level `_source`
/// fields. Identity and sequence metadata are always returned.
///
/// Missing requested fields remain absent, matching `_source` object
/// semantics. An empty projection returns identity-only rows.
pub fn decode_stored_v2_rows_projected(
    bytes: &[u8],
    ordinals: &[usize],
    source_fields: &[&str],
) -> Result<StoredV2RowHydrationResult> {
    match decode_stored_v2_rows_projected_controlled(bytes, ordinals, Some(source_fields), |_| {
        std::ops::ControlFlow::Continue(())
    })? {
        StoredDecodeRun::Complete(result) => Ok(result),
        StoredDecodeRun::Cancelled => unreachable!("non-cancelling compatibility wrapper"),
    }
}

pub fn decode_stored_v2_rows_controlled<F>(
    bytes: &[u8],
    ordinals: &[usize],
    checkpoint: F,
) -> Result<StoredDecodeRun<StoredV2RowHydrationResult>>
where
    F: FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
{
    decode_stored_v2_rows_projected_controlled(bytes, ordinals, None, checkpoint)
}

pub fn decode_stored_v2_rows_projected_controlled<F>(
    bytes: &[u8],
    ordinals: &[usize],
    source_fields: Option<&[&str]>,
    mut checkpoint: F,
) -> Result<StoredDecodeRun<StoredV2RowHydrationResult>>
where
    F: FnMut(StoredDecodeCheckpoint) -> std::ops::ControlFlow<()>,
{
    if bytes.len() < 4 || &bytes[..4] != STORED_V2_MAGIC {
        return Ok(StoredDecodeRun::Complete(StoredV2RowHydrationResult::NotV2));
    }
    let directory = parse_v2_directory(&bytes[4..])?;
    let id_index = directory
        .columns
        .iter()
        .position(|column| column.name == "__id")
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 missing __id column")))?;
    let seq_index = directory
        .columns
        .iter()
        .position(|column| column.name == "__seq_no")
        .ok_or_else(|| StorageError::Other(anyhow::anyhow!("v2 missing __seq_no column")))?;
    let mut selected = ordinals.to_vec();
    selected.sort_unstable();
    selected.dedup();
    if selected
        .iter()
        .any(|ordinal| *ordinal >= directory.num_docs)
    {
        return Err(StorageError::Other(anyhow::anyhow!(
            "selected row outside stored section"
        )));
    }
    if selected.is_empty() {
        return Ok(StoredDecodeRun::Complete(
            StoredV2RowHydrationResult::Hydrated {
                rows: Vec::new(),
                stats: StoredV2RowHydrationStats::default(),
            },
        ));
    }
    let nulls_index = directory
        .columns
        .iter()
        .position(|column| column.name == V2_EXPLICIT_NULLS_COLUMN);
    let mut stats = StoredV2RowHydrationStats::default();
    let requested: Option<HashSet<&str>> =
        source_fields.map(|fields| fields.iter().copied().collect());
    let mut columns: Vec<Option<Vec<serde_json::Value>>> =
        (0..directory.columns.len()).map(|_| None).collect();
    let mut dict_ids = HashMap::new();
    for index in 0..directory.columns.len() {
        let column = &directory.columns[index];
        // The explicit-null column is decoded like `__id` / `__seq_no` — a
        // projection never names it, and without it a projected field that the
        // document stored as `null` would come back absent.
        let wanted = index == id_index
            || index == seq_index
            || Some(index) == nulls_index
            || requested
                .as_ref()
                .is_none_or(|fields| fields.contains(column.name));
        if !wanted {
            continue;
        }
        if decode_cancelled(&mut checkpoint, StoredDecodePhase::BeforeColumn, index, 0) {
            return Ok(StoredDecodeRun::Cancelled);
        }
        match select_column_rows(
            index,
            &directory,
            &selected,
            &mut stats,
            &mut dict_ids,
            &mut checkpoint,
        )? {
            Ok(values) => columns[index] = Some(values),
            Err(Some(column)) => {
                return Ok(StoredDecodeRun::Complete(
                    StoredV2RowHydrationResult::UnsupportedDependencyShape { column },
                ));
            }
            Err(None) => return Ok(StoredDecodeRun::Cancelled),
        }
        if decode_cancelled(
            &mut checkpoint,
            StoredDecodePhase::AfterColumn,
            index,
            directory.num_docs,
        ) {
            return Ok(StoredDecodeRun::Cancelled);
        }
    }
    let mut rows = Vec::with_capacity(selected.len());
    for (selected_index, ordinal) in selected.into_iter().enumerate() {
        let mut source = serde_json::Map::new();
        let stored_null = nulls_index
            .and_then(|index| columns[index].as_ref())
            .and_then(|values| values[selected_index].as_array());
        for (column_index, column) in directory.columns.iter().enumerate() {
            if column_index == id_index
                || column_index == seq_index
                || Some(column_index) == nulls_index
            {
                continue;
            }
            let Some(values) = columns[column_index].as_ref() else {
                continue;
            };
            let value = values[selected_index].clone();
            if !value.is_null() {
                source.insert(column.name.to_string(), value);
                stats.output_values_cloned += 1;
            } else if column_stored_null(stored_null, column.name) {
                source.insert(column.name.to_string(), serde_json::Value::Null);
                stats.output_values_cloned += 1;
            }
        }
        stats.output_values_cloned += 2;
        rows.push(StoredV2HydratedRow {
            ordinal,
            id: columns[id_index].as_ref().unwrap()[selected_index].clone(),
            seq_no: columns[seq_index].as_ref().unwrap()[selected_index].clone(),
            source,
        });
    }
    Ok(StoredDecodeRun::Complete(
        StoredV2RowHydrationResult::Hydrated { rows, stats },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assemble(num_docs: usize, columns: &[(&str, ColCodec, Vec<u8>)]) -> Vec<u8> {
        let mut out = STORED_V2_MAGIC.to_vec();
        out.write_u32::<LittleEndian>(num_docs as u32).unwrap();
        out.write_u32::<LittleEndian>(columns.len() as u32).unwrap();
        for (name, codec, payload) in columns {
            out.write_u16::<LittleEndian>(name.len() as u16).unwrap();
            out.extend_from_slice(name.as_bytes());
            out.push(*codec as u8);
            out.write_u32::<LittleEndian>(payload.len() as u32).unwrap();
            out.extend_from_slice(payload);
        }
        out
    }

    fn raw(values: &[serde_json::Value]) -> Vec<u8> {
        zstd::encode_all(
            Cursor::new(serde_json::to_vec(values).unwrap()),
            STORED_ZSTD_LEVEL,
        )
        .unwrap()
    }

    fn fixture(n: usize, body_codec: ColCodec) -> Vec<u8> {
        let ids: Vec<_> = (0..n).map(|i| json!(format!("doc-{i}"))).collect();
        let seq: Vec<_> = (0..n).map(|i| json!(i)).collect();
        let body: Vec<_> = (0..n)
            .map(|i| json!({"row": i, "text": format!("payload-{i}")}))
            .collect();
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let body_payload = match body_codec {
            ColCodec::RawJson => zstd::encode_all(Cursor::new(body_bytes), 3).unwrap(),
            ColCodec::Lz4Json => lz4_flex::compress_prepend_size(&body_bytes),
            _ => unreachable!(),
        };
        assemble(
            n,
            &[
                ("__id", ColCodec::RawJson, raw(&ids)),
                ("__seq_no", ColCodec::RawJson, raw(&seq)),
                ("body", body_codec, body_payload),
            ],
        )
    }

    #[test]
    fn raw_and_lz4_select_rows_and_reject_trailing_json() {
        for codec in [ColCodec::RawJson, ColCodec::Lz4Json] {
            let encoded = fixture(128, codec);
            let StoredV2RowHydrationResult::Hydrated { rows, stats } =
                decode_stored_v2_rows(&encoded, &[127, 0, 7, 7]).unwrap()
            else {
                panic!("expected hydration")
            };
            assert_eq!(
                rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
                [0, 7, 127]
            );
            assert_eq!(rows[1].source["body"]["row"], 7);
            assert_eq!(stats.output_values_cloned, 9);
            if codec == ColCodec::Lz4Json {
                assert!(stats.decompressed_buffer_bytes > 0);
            }
        }

        let mut bad_values = serde_json::to_vec(&vec![json!("x"); 4]).unwrap();
        bad_values.extend_from_slice(b"[]");
        let bad = assemble(
            4,
            &[
                ("__id", ColCodec::RawJson, raw(&vec![json!("x"); 4])),
                ("__seq_no", ColCodec::RawJson, raw(&vec![json!(0); 4])),
                (
                    "bad",
                    ColCodec::Lz4Json,
                    lz4_flex::compress_prepend_size(&bad_values),
                ),
            ],
        );
        assert!(decode_stored_v2_rows(&bad, &[0]).is_err());
    }

    #[test]
    fn dictionary_nulls_boundaries_and_corruption_are_validated() {
        let values = vec![json!("a"), json!("b"), json!("c")];
        let ids = vec![0, 1, 3, 2, 0, 3, 1, 2];
        let encoded = assemble(
            ids.len(),
            &[
                (
                    "__id",
                    ColCodec::DictBitpack,
                    encode_dict_bitpack(&values, &ids, STORED_ZSTD_LEVEL),
                ),
                (
                    "__seq_no",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!(9)).unwrap(),
                ),
            ],
        );
        let StoredV2RowHydrationResult::Hydrated { rows, stats } =
            decode_stored_v2_rows(&encoded, &[0, 2, 7]).unwrap()
        else {
            panic!("expected hydration")
        };
        assert_eq!(rows[0].id, "a");
        assert!(rows[1].id.is_null());
        assert_eq!(rows[2].id, "c");
        assert_eq!(stats.selected_dictionary_entries, 2);

        for width in 1..=32 {
            let max = if width == 32 {
                u32::MAX
            } else {
                (1u32 << width) - 1
            };
            let packed = bitpack_u32(&[0, max, max / 2], width);
            assert_eq!(unpack_id(&packed, width, 1), Some(max));
            assert_eq!(unpack_id(&packed, width, 2), Some(max / 2));
            assert_eq!(unpack_id(&packed[..packed.len() - 1], width, 2), None);
        }

        let mut corrupt = encode_dict_bitpack(&[json!("only")], &[0, 0, 0], STORED_ZSTD_LEVEL);
        // Rebuild the packed stream with id=3, which is outside {entry,null}.
        let dict_len = u32::from_le_bytes(corrupt[5..9].try_into().unwrap()) as usize;
        let packed_offset = 9 + dict_len + 8;
        let bad_packed = zstd::encode_all(Cursor::new(bitpack_u32(&[0, 3, 0], 2)), 3).unwrap();
        corrupt.truncate(packed_offset);
        corrupt.extend_from_slice(&bad_packed);
        corrupt[4] = 2;
        let length_offset = 9 + dict_len + 4;
        corrupt[length_offset..length_offset + 4]
            .copy_from_slice(&(bad_packed.len() as u32).to_le_bytes());
        let bad = assemble(
            3,
            &[
                ("__id", ColCodec::DictBitpack, corrupt),
                (
                    "__seq_no",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!(0)).unwrap(),
                ),
            ],
        );
        assert!(decode_stored_v2_rows(&bad, &[0]).is_err());

        let mut padding = encode_dict_bitpack(&[json!("only")], &[0, 0, 0], STORED_ZSTD_LEVEL);
        let dict_len = u32::from_le_bytes(padding[5..9].try_into().unwrap()) as usize;
        let packed_length_offset = 9 + dict_len + 4;
        let packed_offset = packed_length_offset + 4;
        let mut unpacked = zstd::decode_all(&padding[packed_offset..]).unwrap();
        unpacked[0] |= 0b1000_0000;
        let repacked = zstd::encode_all(Cursor::new(unpacked), 3).unwrap();
        padding.truncate(packed_offset);
        padding.extend_from_slice(&repacked);
        padding[packed_length_offset..packed_offset]
            .copy_from_slice(&(repacked.len() as u32).to_le_bytes());
        let bad_padding = assemble(
            3,
            &[
                ("__id", ColCodec::DictBitpack, padding),
                (
                    "__seq_no",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!(0)).unwrap(),
                ),
            ],
        );
        assert!(decode_stored_v2_rows(&bad_padding, &[0]).is_err());
    }

    #[test]
    fn selection_validation_empty_fast_path_and_fixed_r_work() {
        assert!(matches!(
            decode_stored_v2_rows(b"plain", &[0]).unwrap(),
            StoredV2RowHydrationResult::NotV2
        ));
        let encoded = fixture(128, ColCodec::RawJson);
        assert!(decode_stored_v2_rows(&encoded, &[128]).is_err());
        let StoredV2RowHydrationResult::Hydrated { rows, stats } =
            decode_stored_v2_rows(&encoded, &[]).unwrap()
        else {
            panic!("expected hydration")
        };
        assert!(rows.is_empty());
        assert_eq!(stats, StoredV2RowHydrationStats::default());

        let small = decode_stored_v2_rows(&fixture(128, ColCodec::RawJson), &[1, 9, 17]).unwrap();
        let large = decode_stored_v2_rows(&fixture(1024, ColCodec::RawJson), &[1, 9, 17]).unwrap();
        let selected = |result| match result {
            StoredV2RowHydrationResult::Hydrated { rows, stats } => (
                rows.len(),
                stats.selected_json_values,
                stats.output_values_cloned,
            ),
            _ => panic!("expected hydration"),
        };
        assert_eq!(selected(small), selected(large));
    }

    #[test]
    fn controlled_hydration_has_deterministic_order_and_distinct_cancellation() {
        let encoded = fixture(256, ColCodec::RawJson);
        let mut observed = Vec::new();
        let run = decode_stored_v2_rows_controlled(&encoded, &[0, 7], |point| {
            observed.push(point);
            if point.phase == StoredDecodePhase::Rows && point.rows_processed == 128 {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        })
        .unwrap();
        assert_eq!(run, StoredDecodeRun::Cancelled);
        assert_eq!(
            observed[0],
            StoredDecodeCheckpoint {
                phase: StoredDecodePhase::BeforeColumn,
                column_index: 0,
                rows_processed: 0,
            }
        );
        assert_eq!(
            observed[1],
            StoredDecodeCheckpoint {
                phase: StoredDecodePhase::Rows,
                column_index: 0,
                rows_processed: 128,
            }
        );

        let complete = decode_stored_v2_rows_controlled(&encoded, &[0, 7], |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(
            complete,
            StoredDecodeRun::Complete(decode_stored_v2_rows(&encoded, &[0, 7]).unwrap())
        );
    }

    #[test]
    fn controlled_constant_hydration_is_fallible_cancellable_and_preserves_parity() {
        let encoded = assemble(
            256,
            &[
                (
                    "__id",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!("same-id")).unwrap(),
                ),
                (
                    "__seq_no",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!(7)).unwrap(),
                ),
                (
                    "body",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!({"kind": "constant"})).unwrap(),
                ),
            ],
        );
        let selected: Vec<_> = (0..256).collect();
        let mut observed = Vec::new();
        let cancelled = decode_stored_v2_rows_controlled(&encoded, &selected, |point| {
            observed.push(point);
            if point.phase == StoredDecodePhase::Rows && point.rows_processed == 128 {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        })
        .unwrap();
        assert_eq!(cancelled, StoredDecodeRun::Cancelled);
        assert_eq!(
            observed[1],
            StoredDecodeCheckpoint {
                phase: StoredDecodePhase::Rows,
                column_index: 0,
                rows_processed: 128,
            }
        );

        let complete = decode_stored_v2_rows_controlled(&encoded, &[255, 0, 7, 7], |_| {
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(
            complete,
            StoredDecodeRun::Complete(decode_stored_v2_rows(&encoded, &[255, 0, 7, 7]).unwrap())
        );
        let StoredDecodeRun::Complete(StoredV2RowHydrationResult::Hydrated { rows, .. }) = complete
        else {
            panic!("expected completed constant hydration")
        };
        assert_eq!(
            rows.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
            [0, 7, 255]
        );
        assert!(rows
            .iter()
            .all(|row| row.source["body"] == json!({"kind": "constant"})));

        let reserve_error =
            repeat_constant_rows_controlled(json!(null), usize::MAX, 0, &mut |_| {
                std::ops::ControlFlow::Continue(())
            })
            .unwrap_err();
        let expected = format!("cannot reserve {} selected constant values", usize::MAX);
        assert!(
            reserve_error.to_string().contains(&expected),
            "allocation failure must identify the requested output count: {reserve_error}"
        );
    }

    #[test]
    fn controlled_hydration_cancellation_precedes_unvisited_corruption() {
        let mut encoded = fixture(256, ColCodec::RawJson);
        let (offset, payload_len) = {
            let directory = parse_v2_directory(&encoded[4..]).unwrap();
            let last = directory.columns.last().unwrap();
            (
                last.payload.as_ptr() as usize - encoded.as_ptr() as usize,
                last.payload.len(),
            )
        };
        encoded[offset + payload_len / 2] ^= 0xff;

        assert_eq!(
            decode_stored_v2_rows_controlled(&encoded, &[0], |_| {
                std::ops::ControlFlow::Break(())
            })
            .unwrap(),
            StoredDecodeRun::Cancelled
        );
        assert!(decode_stored_v2_rows_controlled(&encoded, &[0], |_| {
            std::ops::ControlFlow::Continue(())
        })
        .is_err());
    }

    #[test]
    fn controlled_hydration_stops_before_malformed_row_129() {
        let mut json = String::from("[");
        for row in 0..128 {
            if row != 0 {
                json.push(',');
            }
            json.push_str(&row.to_string());
        }
        json.push_str(",row_129_is_invalid]");
        let malformed = zstd::encode_all(Cursor::new(json.as_bytes()), STORED_ZSTD_LEVEL).unwrap();
        let encoded = assemble(
            256,
            &[
                (
                    "__id",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!("id")).unwrap(),
                ),
                (
                    "__seq_no",
                    ColCodec::Constant,
                    serde_json::to_vec(&json!(1)).unwrap(),
                ),
                ("bad", ColCodec::RawJson, malformed),
            ],
        );
        assert_eq!(
            decode_stored_v2_rows_controlled(&encoded, &[0], |point| {
                if point.column_index == 2
                    && point.phase == StoredDecodePhase::Rows
                    && point.rows_processed == 128
                {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            })
            .unwrap(),
            StoredDecodeRun::Cancelled
        );
        assert!(
            decode_stored_v2_rows_controlled(&encoded, &[0], |_| {
                std::ops::ControlFlow::Continue(())
            })
            .is_err(),
            "without cancellation the parser must consume malformed row 129"
        );
    }

    #[test]
    fn malformed_framing_and_unsupported_dependency_are_explicit() {
        let encoded = fixture(4, ColCodec::RawJson);
        assert!(decode_stored_v2_rows(&encoded[..encoded.len() - 1], &[0]).is_err());

        let valid_cross_dep = |source_index: u32| {
            let mut bytes = vec![0];
            bytes.extend_from_slice(&source_index.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes
        };
        let unsupported = assemble(
            1,
            &[
                ("__id", ColCodec::RawJson, raw(&[json!("a")])),
                ("__seq_no", ColCodec::RawJson, raw(&[json!(0)])),
                ("dep", ColCodec::CrossDep, valid_cross_dep(0)),
            ],
        );
        assert!(matches!(
            decode_stored_v2_rows(&unsupported, &[0]).unwrap(),
            StoredV2RowHydrationResult::UnsupportedDependencyShape { .. }
        ));

        let malformed = assemble(
            1,
            &[
                ("__id", ColCodec::RawJson, raw(&[json!("a")])),
                ("__seq_no", ColCodec::RawJson, raw(&[json!(0)])),
                ("dep", ColCodec::CrossDep, vec![0, 0, 0, 0, 0]),
            ],
        );
        assert!(decode_stored_v2_rows(&malformed, &[0]).is_err());
    }

    #[test]
    fn selected_cross_dep_exceptions_and_nulls_match_full_semantics() {
        let mut dependent = vec![0];
        dependent.extend_from_slice(&0u32.to_le_bytes()); // source column
        dependent.extend_from_slice(&2u32.to_le_bytes());
        dependent.extend_from_slice(&10i64.to_le_bytes());
        dependent.extend_from_slice(&20i64.to_le_bytes());
        dependent.extend_from_slice(&2u32.to_le_bytes());
        write_varint(&mut dependent, 1);
        write_zigzag_i64(&mut dependent, i64::MIN + 1);
        write_varint(&mut dependent, 2);
        write_zigzag_i64(&mut dependent, 99);
        let encoded = assemble(
            4,
            &[
                (
                    "__id",
                    ColCodec::DictBitpack,
                    encode_dict_bitpack(
                        &[json!("a"), json!("b")],
                        &[0, 1, 0, 1],
                        STORED_ZSTD_LEVEL,
                    ),
                ),
                ("__seq_no", ColCodec::CrossDep, dependent),
            ],
        );
        let StoredV2RowHydrationResult::Hydrated { rows, .. } =
            decode_stored_v2_rows(&encoded, &[0, 1, 2, 3]).unwrap()
        else {
            panic!("expected hydration")
        };
        let canonical: Vec<serde_json::Value> =
            serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        for (row, full) in rows.iter().zip(&canonical) {
            assert_eq!(row.id, full["_id"]);
            assert_eq!(row.seq_no, full["_seq_no"]);
            assert_eq!(
                serde_json::Value::Object(row.source.clone()),
                full["_source"]
            );
        }
        assert_eq!(rows[0].seq_no, 10);
        assert!(rows[1].seq_no.is_null());
        assert_eq!(rows[2].seq_no, 10);
        assert_eq!(rows[3].seq_no, 99);
    }

    #[test]
    fn adaptive_encoder_rows_match_canonical_decode_across_source_shapes() {
        let sources: Vec<_> = (0..256)
            .map(|i| {
                json!({
                    "company": if i % 3 == 0 { "Acme" } else { "Globex" },
                    "quarter": (i % 4) + 1,
                    "constant": true,
                    "nested": {"amount": i as f64 / 7.0, "tags": ["finance", i]},
                    "nullable": if i % 11 == 0 { serde_json::Value::Null } else { json!(i % 5) }
                })
            })
            .collect();
        let ids: Vec<_> = (0..256).map(|i| format!("doc-{i}")).collect();
        let docs: Vec<_> = (0..256)
            .map(|i| (ids[i].as_str(), i as u64, &sources[i]))
            .collect();
        let encoded = encode_stored_v2_from_values_nojson(&docs);
        assert_eq!(&encoded[..4], STORED_V2_MAGIC);
        let full: Vec<serde_json::Value> =
            serde_json::from_slice(&decode_stored(&encoded).unwrap()).unwrap();
        let selected = [0, 7, 42, 128, 255];
        let StoredV2RowHydrationResult::Hydrated { rows, .. } =
            decode_stored_v2_rows(&encoded, &selected).unwrap()
        else {
            panic!("current encoder must have a selectively supported dependency shape")
        };
        for (row, ordinal) in rows.iter().zip(selected) {
            assert_eq!(row.id, full[ordinal]["_id"]);
            assert_eq!(row.seq_no, full[ordinal]["_seq_no"]);
            assert_eq!(
                serde_json::Value::Object(row.source.clone()),
                full[ordinal]["_source"]
            );
        }
    }

    #[test]
    fn row_projection_preserves_sparse_parity_and_skips_unrequested_payloads() {
        let count = 128usize;
        let ids: Vec<_> = (0..count).map(|row| json!(format!("id-{row}"))).collect();
        let seqs: Vec<_> = (0..count).map(|row| json!(row)).collect();
        let wanted: Vec<_> = (0..count)
            .map(|row| {
                if row % 3 == 0 {
                    json!(format!("quarter-{row}"))
                } else {
                    serde_json::Value::Null
                }
            })
            .collect();
        let encoded = assemble(
            count,
            &[
                ("__id", ColCodec::RawJson, raw(&ids)),
                ("__seq_no", ColCodec::RawJson, raw(&seqs)),
                ("wanted", ColCodec::RawJson, raw(&wanted)),
                // Valid framing, deliberately corrupt payload. A projected
                // read must not touch an unrelated column.
                ("unrelated", ColCodec::RawJson, b"not-zstd".to_vec()),
            ],
        );
        let selected = [0, 1, 42, 127];
        let StoredV2RowHydrationResult::Hydrated { rows, stats } =
            decode_stored_v2_rows_projected(&encoded, &selected, &["wanted"]).unwrap()
        else {
            panic!("expected projected hydration")
        };
        assert_eq!(rows.len(), selected.len());
        for (row, ordinal) in rows.iter().zip(selected) {
            assert_eq!(row.id, ids[ordinal]);
            assert_eq!(row.seq_no, seqs[ordinal]);
            if wanted[ordinal].is_null() {
                assert!(!row.source.contains_key("wanted"));
            } else {
                assert_eq!(row.source["wanted"], wanted[ordinal]);
            }
            assert!(!row.source.contains_key("unrelated"));
        }
        assert_eq!(stats.selected_json_values, selected.len() * 3);
        assert!(decode_stored_v2_rows(&encoded, &selected).is_err());

        let StoredV2RowHydrationResult::Hydrated { rows, .. } =
            decode_stored_v2_rows_projected(&encoded, &[7], &[]).unwrap()
        else {
            panic!("expected identity-only hydration")
        };
        assert!(rows[0].source.is_empty());
    }

    /// #367: selective hydration reassembles `_source` from column cells too,
    /// so it needs the explicit-null column even though no projection ever
    /// names it — otherwise a projected field the document stored as `null`
    /// comes back absent while the full decode returns it.
    #[test]
    fn row_hydration_restores_stored_nulls_for_full_and_projected_reads() {
        let count = 128usize;
        let docs: Vec<_> = (0..count)
            .map(|row| {
                json!({
                    "_id": format!("id-{row}"),
                    "_seq_no": row,
                    "_source": {
                        "kept": format!("value-{row}"),
                        "reason": serde_json::Value::Null,
                    }
                })
            })
            .collect();
        let encoded = encode_stored_v2(&serde_json::to_vec(&docs).unwrap());
        let selected = [0usize, 63, 127];

        let StoredV2RowHydrationResult::Hydrated { rows, .. } =
            decode_stored_v2_rows(&encoded, &selected).unwrap()
        else {
            panic!("expected full hydration")
        };
        for (row, ordinal) in rows.iter().zip(selected) {
            assert_eq!(
                serde_json::Value::Object(row.source.clone()),
                docs[ordinal]["_source"],
                "row {ordinal} must hydrate the stored null"
            );
        }

        let StoredV2RowHydrationResult::Hydrated { rows, .. } =
            decode_stored_v2_rows_projected(&encoded, &selected, &["reason"]).unwrap()
        else {
            panic!("expected projected hydration")
        };
        for row in &rows {
            assert!(
                row.source
                    .get("reason")
                    .is_some_and(serde_json::Value::is_null),
                "a projected stored null must survive: {:?}",
                row.source
            );
            assert!(!row.source.contains_key("kept"));
        }
    }
}
