//! Flat `f32` view of one vector field over one immutable segment (#939).
//!
//! The exact kNN scan used to deep-clone every stored `_source` — vectors
//! included, as `serde_json::Value::Number`s — into its candidate list, clone
//! it a second time to evaluate the filter, clone the passage-vector array a
//! third time through `get_field_value`, and then re-walk those JSON numbers
//! into a fresh `Vec<f32>` per passage. All of that ran per document, per
//! query, and none of it was arithmetic: on 5,183 multi-passage documents it
//! was ~400 ms around ~14 ms of model time.
//!
//! This module holds the vectors once, as plain `f32`s, per segment. Segments
//! are immutable, so the column is derived exactly once and then shared; which
//! documents are *live* is still decided per query, by the caller.
//!
//! # Reference designs (approach adapted, no code copied)
//!
//! * **Flattened multi-vector + `dim`.** qdrant stores a multi-vector as one
//!   contiguous buffer and slices it per sub-vector
//!   (`lib/segment/src/data_types/vectors.rs:271`, max-sim at
//!   `lib/segment/src/vector_storage/query_scorer/mod.rs:77`; Apache-2.0,
//!   74f3e85). Ours differs where XERJ's contract differs: one query vector,
//!   best passage wins, lowest ordinal on an exact tie, and passages may be
//!   skipped without renumbering the ones after them.
//! * **Score an address, hydrate the winners.** qdrant ranks
//!   `ScoredPointOffset { idx, score }` and only then retrieves payloads
//!   (`lib/segment/src/segment/read_view/search.rs:222`); tantivy ranks
//!   `DocAddress { segment_ord, doc_id }` and fetches stored documents by
//!   address (`src/lib.rs:338`, `src/core/searcher.rs:88`; MIT, 3a55bc9).
//!
//! # Exactness contract
//!
//! Scores produced from this column are **bit-identical** to the scan it
//! replaces. That is a property of construction, not of tolerance:
//!
//! * elements are converted with the same expression the scan used
//!   (`as_f64()` then `as f32`, non-numbers dropped);
//! * a vector whose converted length differs from the query's is skipped at
//!   query time, exactly as before, and keeps its original passage ordinal;
//! * a document whose `<field>_chunks` is an array with no usable passage is
//!   skipped — it does **not** fall back to the pooled vector, because the old
//!   scan did not;
//! * the dot product and the norms are the same strictly sequential `f64`
//!   reductions ([`dot_f64`], [`norm_f64`]), shared with
//!   `compute_vector_similarity` so the two cannot drift. Precomputing a
//!   document vector's norm stores the value the scan would have recomputed.

use serde_json::Value;
use std::borrow::Cow;

use crate::index::get_field_value;

/// Which stored vectors a column holds for each document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ColumnMode {
    /// `<field>_chunks` passage vectors when that companion is an array, the
    /// pooled `<field>` vector otherwise. The default exact scan.
    BestPassage,
    /// Always the pooled `<field>` vector. The `scalar8` scan reads only that.
    PooledOnly,
}

impl ColumnMode {
    /// Distinguishes the two shapes in a cache key.
    pub(crate) fn key_tag(self) -> char {
        match self {
            Self::BestPassage => 'c',
            Self::PooledOnly => 'p',
        }
    }
}

/// One stored vector: where it lives in the flat buffer, which passage it was,
/// and its Euclidean norm.
#[derive(Clone, Copy, Debug)]
struct VectorEntry {
    start: usize,
    len: usize,
    ordinal: u32,
    norm: f64,
}

/// The vectors one document contributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocVectors {
    /// Nothing usable: the scan skips the document.
    Absent,
    /// The pooled vector, reported as passage ordinal 0.
    Pooled(usize),
    /// `count` passage vectors starting at entry `first`. `count == 0` is a
    /// passage array with no usable element, which also skips the document.
    Chunks { first: usize, count: usize },
}

/// A borrowed stored vector handed to the scorer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StoredVector<'a> {
    pub(crate) values: &'a [f32],
    pub(crate) ordinal: u32,
    pub(crate) norm: f64,
}

/// What a document offers the scorer.
pub(crate) enum DocVectorsView<'a> {
    Absent,
    Pooled(StoredVector<'a>),
    Chunks(ChunkIter<'a>),
}

pub(crate) struct ChunkIter<'a> {
    column: &'a SegmentVectorColumn,
    next: usize,
    end: usize,
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = StoredVector<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let vector = self.column.stored(self.next);
        self.next += 1;
        Some(vector)
    }
}

/// Flat vectors for one field over a run of documents, addressed by position.
#[derive(Debug, Default)]
pub(crate) struct SegmentVectorColumn {
    data: Vec<f32>,
    entries: Vec<VectorEntry>,
    docs: Vec<DocVectors>,
    /// Each position's `_id` and `_seq_no`, when it was pushed as a stored
    /// segment document (#1091). These are the only two fields the exact scan
    /// reads from a document before ranking it, so a column that carries them
    /// lets an unfiltered scan choose its candidates without the segment's
    /// parsed stored documents — which, on a passage-chunked corpus, are tens
    /// of kilobytes each and do not fit the hydration budget.
    identities: Vec<Option<DocIdentity>>,
}

/// A stored document's `_id` and `_seq_no`, as the scan reads them.
#[derive(Debug)]
struct DocIdentity {
    id: Box<str>,
    seq_no: Option<u64>,
}

impl SegmentVectorColumn {
    /// Number of document positions covered. A column abandoned part-way (the
    /// request's deadline passed) covers a prefix of its segment.
    pub(crate) fn len(&self) -> usize {
        self.docs.len()
    }

    fn stored(&self, entry: usize) -> StoredVector<'_> {
        let e = &self.entries[entry];
        StoredVector {
            values: &self.data[e.start..e.start + e.len],
            ordinal: e.ordinal,
            norm: e.norm,
        }
    }

    /// The vectors of the document at `position`; `Absent` past the end.
    pub(crate) fn doc(&self, position: usize) -> DocVectorsView<'_> {
        match self.docs.get(position) {
            None | Some(DocVectors::Absent) => DocVectorsView::Absent,
            Some(DocVectors::Pooled(entry)) => DocVectorsView::Pooled(self.stored(*entry)),
            Some(DocVectors::Chunks { first, count }) => DocVectorsView::Chunks(ChunkIter {
                column: self,
                next: *first,
                end: first + count,
            }),
        }
    }

    /// The `_id` and `_seq_no` of the stored document at `position`. `None`
    /// past the end, for a document pushed by source alone (the memtable
    /// scratch), and for a stored document with no string `_id` — the scan
    /// never makes a candidate of that one.
    pub(crate) fn identity(&self, position: usize) -> Option<(&str, Option<u64>)> {
        self.identities
            .get(position)?
            .as_ref()
            .map(|identity| (&*identity.id, identity.seq_no))
    }

    /// Bytes this column keeps alive, for the segment hydration budget.
    pub(crate) fn retained_bytes(&self) -> u64 {
        let id_bytes: usize = self
            .identities
            .iter()
            .flatten()
            .map(|identity| identity.id.len())
            .sum();
        let bytes = self.data.capacity() * std::mem::size_of::<f32>()
            + self.entries.capacity() * std::mem::size_of::<VectorEntry>()
            + self.docs.capacity() * std::mem::size_of::<DocVectors>()
            + self.identities.capacity() * std::mem::size_of::<Option<DocIdentity>>()
            + id_bytes
            + std::mem::size_of::<Self>();
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }
}

/// Builds a [`SegmentVectorColumn`] one document at a time, in position order.
pub(crate) struct ColumnBuilder {
    field: String,
    chunk_field: String,
    mode: ColumnMode,
    column: SegmentVectorColumn,
}

impl ColumnBuilder {
    pub(crate) fn new(field: &str, mode: ColumnMode) -> Self {
        Self {
            field: field.to_string(),
            chunk_field: format!("{field}_chunks"),
            mode,
            column: SegmentVectorColumn::default(),
        }
    }

    /// Forget every document but keep the allocations — the memtable path
    /// reuses one builder as per-document scratch.
    pub(crate) fn clear(&mut self) {
        self.column.data.clear();
        self.column.entries.clear();
        self.column.docs.clear();
        self.column.identities.clear();
    }

    /// The column built so far.
    pub(crate) fn column(&self) -> &SegmentVectorColumn {
        &self.column
    }

    pub(crate) fn finish(mut self) -> SegmentVectorColumn {
        self.column.data.shrink_to_fit();
        self.column.entries.shrink_to_fit();
        self.column.docs.shrink_to_fit();
        self.column.identities.shrink_to_fit();
        self.column
    }

    /// Append a segment's stored document: `{"_id", "_seq_no", "_source"}`,
    /// or a legacy pre-`_source` document whose fields sit at the top level.
    pub(crate) fn push_stored_doc(&mut self, stored: &Value) {
        self.push_vectors(stored_source_view(stored));
        let identity = stored
            .get("_id")
            .and_then(Value::as_str)
            .map(|id| DocIdentity {
                id: id.into(),
                seq_no: stored.get("_seq_no").and_then(Value::as_u64),
            });
        self.column.identities.push(identity);
    }

    /// Append a document given its source object.
    pub(crate) fn push_source(&mut self, source: &Value) {
        self.push_vectors(source);
        self.column.identities.push(None);
    }

    fn push_vectors(&mut self, source: &Value) {
        let doc = match self.mode {
            ColumnMode::PooledOnly => self.push_pooled(source),
            ColumnMode::BestPassage => match field_value_cow(source, &self.chunk_field) {
                Some(chunks) => match chunks.as_ref() {
                    Value::Array(chunks) => self.push_chunks(chunks),
                    _ => self.push_pooled(source),
                },
                None => self.push_pooled(source),
            },
        };
        self.column.docs.push(doc);
    }

    /// Append one row of a segment's typed kNN projection (#1091): the same
    /// document [`Self::push_stored_doc`] would have read, decoded straight to
    /// `f32` by the storage layer instead of through `serde_json::Value`.
    ///
    /// The storage decoder converts each element with the same `as_f64()` then
    /// `as f32` this builder uses, and refuses a column outright — the caller
    /// falls back to the stored documents — whenever a cell holds anything the
    /// `Value` path would have had to drop or skip (a non-number element, a
    /// non-finite value, a passage that is not an array). So every row it does
    /// hand over yields the entries `push_stored_doc` would have pushed:
    /// `chunks` present wins in [`ColumnMode::BestPassage`], a passage keeps
    /// its array index as its ordinal, and `None` is an absent or null cell.
    pub(crate) fn push_projected(
        &mut self,
        id: Option<&str>,
        seq_no: Option<u64>,
        pooled: Option<&[f32]>,
        chunks: Option<&[Vec<f32>]>,
    ) {
        let chunks = match self.mode {
            ColumnMode::BestPassage => chunks,
            ColumnMode::PooledOnly => None,
        };
        let doc = match (chunks, pooled) {
            (Some(chunks), _) => {
                let first = self.column.entries.len();
                for (position, chunk) in chunks.iter().enumerate() {
                    let Ok(ordinal) = u32::try_from(position) else {
                        continue;
                    };
                    self.push_f32_vector(chunk, ordinal);
                }
                DocVectors::Chunks {
                    first,
                    count: self.column.entries.len() - first,
                }
            }
            (None, Some(pooled)) => DocVectors::Pooled(self.push_f32_vector(pooled, 0)),
            (None, None) => DocVectors::Absent,
        };
        self.column.docs.push(doc);
        self.column.identities.push(id.map(|id| DocIdentity {
            id: id.into(),
            seq_no,
        }));
    }

    fn push_f32_vector(&mut self, values: &[f32], ordinal: u32) -> usize {
        let start = self.column.data.len();
        self.column.data.extend_from_slice(values);
        let norm = norm_f64(&self.column.data[start..]);
        self.column.entries.push(VectorEntry {
            start,
            len: values.len(),
            ordinal,
            norm,
        });
        self.column.entries.len() - 1
    }

    fn push_chunks(&mut self, chunks: &[Value]) -> DocVectors {
        let first = self.column.entries.len();
        for (position, chunk) in chunks.iter().enumerate() {
            let Value::Array(elements) = chunk else {
                continue;
            };
            // The ordinal is the passage's position in the ORIGINAL array, so
            // a skipped passage never renumbers the ones after it.
            let Ok(ordinal) = u32::try_from(position) else {
                continue;
            };
            self.push_vector(elements, ordinal);
        }
        DocVectors::Chunks {
            first,
            count: self.column.entries.len() - first,
        }
    }

    fn push_pooled(&mut self, source: &Value) -> DocVectors {
        match field_value_cow(source, &self.field).as_deref() {
            Some(Value::Array(elements)) => DocVectors::Pooled(self.push_vector(elements, 0)),
            _ => DocVectors::Absent,
        }
    }

    fn push_vector(&mut self, elements: &[Value], ordinal: u32) -> usize {
        let start = self.column.data.len();
        self.column
            .data
            .extend(elements.iter().filter_map(|v| v.as_f64().map(|f| f as f32)));
        let len = self.column.data.len() - start;
        let norm = norm_f64(&self.column.data[start..]);
        self.column.entries.push(VectorEntry {
            start,
            len,
            ordinal,
            norm,
        });
        self.column.entries.len() - 1
    }
}

/// The source object of a segment's stored document, without cloning it.
///
/// Reassembled segment documents are `{"_id", "_seq_no", "_source": {...}}`;
/// legacy pre-M7 segments keep the fields at the top level, beside `_id`.
pub(crate) fn stored_source_view(stored: &Value) -> &Value {
    stored.get("_source").unwrap_or(stored)
}

/// [`get_field_value`] without the clone when the field is a literal key at
/// the source root — which is where a vector field and its generated
/// companions live. Anything else (a dotted path, an array of objects, the
/// multi-field fallback) is answered by `get_field_value` itself, so the two
/// resolve identically by construction.
fn field_value_cow<'a>(source: &'a Value, field: &str) -> Option<Cow<'a, Value>> {
    if let Value::Object(map) = source {
        if let Some(value) = map.get(field) {
            return Some(Cow::Borrowed(value));
        }
    }
    get_field_value(source, field).map(Cow::Owned)
}

/// Strictly sequential `f64` dot product. Shared with
/// `compute_vector_similarity`; do not reorder or widen this reduction — a
/// different summation order changes the low bits of every score.
#[inline]
pub(crate) fn dot_f64(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum()
}

/// Strictly sequential `f64` Euclidean norm. Same warning as [`dot_f64`].
#[inline]
pub(crate) fn norm_f64(v: &[f32]) -> f64 {
    v.iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn views(column: &SegmentVectorColumn, position: usize) -> Vec<(u32, Vec<f32>)> {
        match column.doc(position) {
            DocVectorsView::Absent => Vec::new(),
            DocVectorsView::Pooled(v) => vec![(v.ordinal, v.values.to_vec())],
            DocVectorsView::Chunks(iter) => iter.map(|v| (v.ordinal, v.values.to_vec())).collect(),
        }
    }

    #[test]
    fn best_passage_prefers_the_chunk_companion_over_the_pooled_vector() {
        let mut b = ColumnBuilder::new("body_vector", ColumnMode::BestPassage);
        b.push_stored_doc(&json!({"_id": "a", "_source": {
            "body_vector": [9.0, 9.0],
            "body_vector_chunks": [[1.0, 0.0], [0.0, 1.0]],
        }}));
        let column = b.finish();
        assert_eq!(
            views(&column, 0),
            vec![(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])]
        );
    }

    #[test]
    fn skipped_passages_keep_the_ordinals_of_the_ones_after_them() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"v_chunks": [[1.0, 0.0], "not a vector", [0.0, 1.0]]}));
        let column = b.finish();
        assert_eq!(
            views(&column, 0),
            vec![(0, vec![1.0, 0.0]), (2, vec![0.0, 1.0])]
        );
    }

    #[test]
    fn an_empty_passage_array_skips_the_document_instead_of_falling_back() {
        // The scan this replaces `continue`d here; falling back to the pooled
        // vector would surface a document it never returned.
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"v": [1.0, 0.0], "v_chunks": ["x", 3]}));
        b.push_source(&json!({"v": [1.0, 0.0], "v_chunks": []}));
        let column = b.finish();
        assert!(views(&column, 0).is_empty());
        assert!(views(&column, 1).is_empty());
    }

    #[test]
    fn a_non_array_chunk_companion_falls_back_to_the_pooled_vector() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"v": [0.5, 0.25], "v_chunks": "nope"}));
        let column = b.finish();
        assert_eq!(views(&column, 0), vec![(0, vec![0.5, 0.25])]);
    }

    #[test]
    fn pooled_only_ignores_the_chunk_companion() {
        let mut b = ColumnBuilder::new("v", ColumnMode::PooledOnly);
        b.push_source(&json!({"v": [0.5, 0.25], "v_chunks": [[1.0, 0.0]]}));
        let column = b.finish();
        assert_eq!(views(&column, 0), vec![(0, vec![0.5, 0.25])]);
    }

    #[test]
    fn non_numeric_elements_are_dropped_like_the_scan_dropped_them() {
        // `filter_map(as_f64)` shortens the vector; the length check against
        // the query then rejects it at query time, as it always did.
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"v": [1.0, "x", 2.0]}));
        let column = b.finish();
        assert_eq!(views(&column, 0), vec![(0, vec![1.0, 2.0])]);
    }

    #[test]
    fn missing_and_non_array_vectors_are_absent_and_positions_stay_aligned() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"other": 1}));
        b.push_source(&json!({"v": "text"}));
        b.push_source(&json!({"v": [3.0]}));
        let column = b.finish();
        assert_eq!(column.len(), 3);
        assert!(views(&column, 0).is_empty());
        assert!(views(&column, 1).is_empty());
        assert_eq!(views(&column, 2), vec![(0, vec![3.0])]);
        assert!(views(&column, 99).is_empty(), "past the end is absent");
    }

    #[test]
    fn legacy_documents_without_source_are_read_at_the_top_level() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_stored_doc(&json!({"_id": "legacy", "v": [1.0, 2.0]}));
        let column = b.finish();
        assert_eq!(views(&column, 0), vec![(0, vec![1.0, 2.0])]);
    }

    #[test]
    fn stored_documents_carry_their_identity_and_sources_do_not() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_stored_doc(&json!({"_id": "a", "_seq_no": 7, "_source": {"v": [1.0]}}));
        b.push_stored_doc(&json!({"_id": "legacy", "v": [2.0]}));
        b.push_stored_doc(&json!({"_id": 3, "_source": {"v": [3.0]}}));
        b.push_source(&json!({"v": [4.0]}));
        let column = b.finish();
        assert_eq!(column.identity(0), Some(("a", Some(7))));
        assert_eq!(column.identity(1), Some(("legacy", None)));
        assert_eq!(column.identity(2), None, "a non-string _id is no candidate");
        assert_eq!(column.identity(3), None, "a bare source has no identity");
        assert_eq!(column.identity(99), None);
        let mut scratch = ColumnBuilder::new("v", ColumnMode::BestPassage);
        scratch.push_stored_doc(&json!({"_id": "a", "_source": {"v": [1.0]}}));
        scratch.clear();
        scratch.push_source(&json!({"v": [1.0]}));
        assert_eq!(
            scratch.column().identity(0),
            None,
            "clear forgets identities"
        );
    }

    #[test]
    fn dotted_vector_fields_resolve_through_get_field_value() {
        let mut b = ColumnBuilder::new("emb.v", ColumnMode::BestPassage);
        b.push_source(&json!({"emb": {"v": [1.0, 2.0]}}));
        let column = b.finish();
        assert_eq!(views(&column, 0), vec![(0, vec![1.0, 2.0])]);
    }

    #[test]
    fn stored_norm_is_the_value_the_scan_would_recompute() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"v": [0.1, -0.7, 0.33, 1e-3]}));
        let column = b.finish();
        let DocVectorsView::Pooled(v) = column.doc(0) else {
            panic!("expected a pooled vector");
        };
        assert_eq!(v.norm.to_bits(), norm_f64(v.values).to_bits());
    }

    #[test]
    fn clear_reuses_the_builder_as_scratch() {
        let mut b = ColumnBuilder::new("v", ColumnMode::BestPassage);
        b.push_source(&json!({"v": [1.0]}));
        b.clear();
        b.push_source(&json!({"v": [2.0]}));
        assert_eq!(b.column().len(), 1);
        assert_eq!(views(b.column(), 0), vec![(0, vec![2.0])]);
    }
}
