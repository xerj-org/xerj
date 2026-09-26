# XERJ vs Lucene 10.3.1

This page is a source map for humans and AI agents comparing XERJ with Lucene.
It answers six storage and search questions a Lucene, Solr, or Elasticsearch
engineer is likely to ask, then links each answer to its implementation or
official documentation. XERJ is a from-scratch Rust engine whose primary
workflow is `autoindex <folder> -> autoindex map -> query`; Lucene 10.3.1 is an
embeddable Java search library. Elasticsearch-wire compatibility is an adoption
bridge, not evidence that the two engines share an implementation.

This comparison is current as of **2026-08-17**, except section 5's
`_update_by_query` note, which is updated to **2026-09-26** and pinned to that
change's merge commit. XERJ claims and source links
are pinned to commit
[`24711999dd866ceec2a6e7c91d934c2c27d7066c`](https://github.com/xerj-org/xerj/tree/24711999dd866ceec2a6e7c91d934c2c27d7066c);
Lucene links point to official Apache Lucene 10.3.1 documentation. The XERJ
source wins if a nearby overview page says something different.

The default XERJ embedding fallback is lexical feature-hashing, not neural
semantic understanding ([`Embedder`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-ai/src/embedder.rs#L7-L23)).
The built-in neural BERT backend is opt-in via `--embed-mode neural` and
downloads its model on first use ([model loader](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-ai/src/neural.rs#L225-L275)).

| Axis | Lucene 10.3.1 | XERJ at `24711999` |
|---|---|---|
| 1. Segment and merge model | `IndexWriter`, immutable per-segment lifecycle, merge policy and scheduler | WAL, sharded buffers, immutable `.seg` containers and sidecars |
| 2. Postings layout | BlockTree terms, packed blocks, two-level impact skips | FST/metadata terms, 128-doc PFOR blocks, no persisted skip table |
| 3. Values and ranges | DocValues for columns; BKD points for numeric and geo ranges | Numeric/keyword `.dv` columns with a sorted range index |
| 4. Analysis chain | Broad analysis modules and composable `CustomAnalyzer` | Curated registry and shape-specific query projection |
| 5. Update and delete semantics | Delete/add documents plus restricted DocValues updates | Source overlay, logical replacement, versions and tombstones |
| 6. Vector index placement | Per-segment HNSW, filter-aware search and quantized codecs | Index-level HNSW for eligible plain queries; exact fallback elsewhere |

## 1. Segment and merge model

Lucene buffers indexing work in `IndexWriter`, flushes immutable segments, and
lets a `MergePolicy` select groups of segments for replacement by a merged
segment. Its default merge scheduler can run those merges in background
threads. See the [Lucene 10.3.1 `IndexWriter`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/index/IndexWriter.html),
[merge policy](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/index/MergePolicy.html),
and [concurrent merge scheduler](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/index/ConcurrentMergeScheduler.html).

XERJ has a different publication boundary. A WAL records mutations; production
[`Index::flush`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L18012-L18147)
fans out one
[`do_flush_shard`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L24433-L24920)
task per FTS shard. Each shard drains and sequence-orders its entries, builds
FTS and `.dv` sidecars, then calls
[`finalize_flush_with_publisher`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/index_store.rs#L1901-L1935)
to write an immutable `.seg` and atomically publish an `ArcSwap` snapshot.

The live ingest buffer is a configurable
[`ShardedFtsMemtable`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/memtable.rs#L670-L709),
not one process-wide FTS lock. Its shards drain and re-sort by WAL sequence. The
background
[`run_merge_once`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L8468-L8495)
writes a replacement segment from surviving documents, and
[`apply_merge_with_repoints`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/index_store.rs#L3697-L3707)
publishes it before
[`retire_segment_files`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/index_store.rs#L1553-L1570)
retires the inputs. A XERJ segment is a checksummed container whose
[`SegmentWriter::finish`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/segment.rs#L400-L475)
writes named sections
([`SectionType`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/segment.rs#L85-L113));
FTS and doc values are separate sidecars, while stored source is a segment
section.

## 2. Postings layout

Lucene 10.3.1's postings format uses a BlockTree term dictionary and packed
integer blocks (128 integers in the packed path), with skip data interleaved at
two levels. The skip data carries competitive impact metadata that lets scorers
safely skip score calculation for uncompetitive documents rather than merely
advancing through every posting. See
[`Lucene103PostingsFormat`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/lucene103/Lucene103PostingsFormat.html)
and its
[`Lucene103PostingsWriter`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/lucene103/Lucene103PostingsWriter.html).

XERJ's
[`FtsIndexWriter`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/index.rs#L557-L620)
writes one sidecar family per indexed field: an FST term dictionary, a
compressed `.post` blob, binary `.meta` term statistics, and encoded `.norms`.
The FST maps a term to a fixed metadata record; it does not point directly to a
Lucene segment file. The format and reader are documented in
[`FtsIndexReader`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/index.rs#L1157-L1228).

The current encoding is defined by the
[`postings` module format notes](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/postings.rs#L1-L40),
[`PostingsWriter::encode_term`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/postings.rs#L260-L331),
and its
[`packed-block helpers`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/postings.rs#L333-L421).
Doc IDs are delta-encoded in 128-doc PFOR blocks; positioned fields add packed
term frequencies and variable-byte positions, while exact fields can omit
both. Residual postings use variable-byte encoding. The shared block size is
not Lucene codec compatibility. XERJ's `SkipEntry` is currently in-memory
scaffolding and is not serialized, so
[`advance_to`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/postings.rs#L705-L712)
scans linearly; it does not provide Lucene's persisted impact-skip behavior.

## 3. Doc-values equivalent

Lucene exposes typed, column-oriented DocValues alongside postings; its codec
chooses on-disk representations for numeric, sorted, sorted-set, and related
types. The 10.3.1 reference describes `.dvd` and `.dvm` files in
[`Lucene90DocValuesFormat`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/lucene90/Lucene90DocValuesFormat.html),
while
[`SortedSetDocValuesField`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/document/SortedSetDocValuesField.html)
is useful for a multi-valued sorted field.

DocValues are not Lucene's primary numeric-range index. Numeric and geo points
such as [`LongPoint`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/document/LongPoint.html)
are queried through [`PointRangeQuery`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/search/PointRangeQuery.html)
and stored by a BKD points format such as
[`Lucene90PointsFormat`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/lucene90/Lucene90PointsFormat.html),
which can prune ranges of the point tree. XERJ does not claim a byte-compatible
DocValues codec or a BKD counterpart.

XERJ's closest DocValues equivalent is a per-segment `{segment_id}.dv` sidecar.
The
[`Column` enum](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/doc_values.rs#L560-L590)
currently has numeric and keyword columns, each with a missing-value bitmap;
keyword columns use sorted terms plus ordinals, and numeric columns retain a
sorted `(value, doc_id)` index for range work. The binary envelope is produced
by
[`encode_columns`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/doc_values.rs#L637-L735)
and loaded through the engine's `.dv` cache.

The columns feed XERJ sorting, filters, ranges, and aggregation fast paths
without reparsing `_source`. They are an analogous column store, not a drop-in
Lucene DocValues codec: field eligibility, array handling, compression, and the
public mapping contract are XERJ behavior. The segment's stored source remains
the fallback and hydration authority.

## 4. Analysis chain

Lucene's
[`Analyzer`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/analysis/Analyzer.html)
creates a reusable token stream for a field. Lucene also ships broad
[`analysis-common`](https://lucene.apache.org/core/10_3_1/analysis/common/overview-summary.html)
language and token-processing support, and
[`CustomAnalyzer`](https://lucene.apache.org/core/10_3_1/analysis/common/org/apache/lucene/analysis/custom/CustomAnalyzer.html)
composes char filters, a tokenizer, and token filters. Analyzer output is part
of the index contract: changing analysis changes the terms in postings.

Lucene's no-argument
[`StandardAnalyzer`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/analysis/standard/StandardAnalyzer.html)
uses an empty stop-word set, lowercases terms, and does not stem. That default
is roughly equivalent to XERJ's `standard` preset; the material difference is
Lucene's wider module set and custom composition.

XERJ models the same conceptual stages in
[`AnalyzerPipeline::analyze`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/analyzer.rs#L69-L126):
char filters, one tokenizer, then ordered token filters.
[`AnalyzerRegistry`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-fts/src/analyzer.rs#L937-L1100)
starts with built-ins; supported custom settings are validated and installed at
index creation
([create-time gate](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L5922-L5940)).
The FTS mapping builder chooses `standard` for `text` and `keyword` for other
mapped field types
([`build_fts_field_configs`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L19383-L19407));
its `standard` preset is Unicode word splitting plus lowercase, with no
stop-word removal or stemming.

Query analysis is shape-specific, not general index/query custom-analyzer
symmetry: `match` uses the requested built-in analyzer or `standard` when
omitted; unknown or custom names decline to stored evaluation in this default
registry
([`match` projection](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L37926-L37975)).
Keyword matches use whole-value terms. On keyword or exact fields,
`match_phrase` uses the whole-value term path. On analyzed text fields, its FTS
phrase path requires `standard` and zero slop; other analyzed shapes fall back
to stored evaluation
([`match_phrase` implementation](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L38447-L38507)).

## 5. Update and delete semantics

Lucene documents are immutable once in a segment. In Lucene's own API,
`updateDocument` is expressed as delete-then-add; buffered deletes and live
document state are reconciled as segments are flushed and merged. The core
lifecycle is stable, but concrete segment, live-docs, and DocValues files are
codec/package details that can vary by Lucene commit; see the official
[10.3.1 index package](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/index/package-summary.html)
and [`IndexWriter`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/index/IndexWriter.html),
not universal filenames. Live-doc encoding is defined by the
[`LiveDocsFormat` contract](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/LiveDocsFormat.html).
Lucene core accepts `IndexableField` objects, not JSON. Its
`updateNumericDocValue` and `updateBinaryDocValue` APIs only update existing
fields that were indexed with the corresponding DocValues type and DocValues
only; they cannot add arbitrary fields and are not a general JSON partial-update
API.

XERJ also does not edit an already-published segment in place. A document
update appends a new WAL/index version under the same `_id`; the
[`VersionMap`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/version_map.rs#L86-L118)
tracks the newest sequence number and whether the current version is deleted.
Old physical copies are ghosts until merge filtering removes them. Deletes are
WAL tombstones and can be carried in a segment's sequence-aware tombstone
section; the latest-version checks in merge and recovery keep an older copy
from reappearing.

The ES-shaped partial-update surface is real:
[`update_document`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L13144-L13210)
reads the current source, overlays top-level fields, and reindexes the merged
document. The API exposes `/_update/{id}` in
[`update_doc`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-api/src/es_compat.rs#L17446-L17540)
and `/_update_by_query` in
[`update_by_query`](https://github.com/xerj-org/xerj/blob/b63debbbbc1f64d93d3a8eebe8cbc780fa42f1a9/engine/crates/xerj-api/src/es_compat.rs#L26585-L26795).
Since PR [#1023](https://github.com/xerj-org/xerj/pull/1023) (merged 2026-09-26)
that handler pages the match set: an `_id`-keyset `search_after` cursor in
`scroll_size` batches (default 1000), a single-pass `matching_ids_sorted` arm
for `match_all`/`ids` selectors, ES `max_docs`/`scroll_size` honored, and a
10,000,000 `max_total` backstop. One request is now a full-corpus traversal;
at the 2026-08-17 pin above it hard-capped its fetch at 10,000 hits
(`size: 10000`). Updates are logical replacement
plus versioning, not byte-level mutation. A delete also tombstones the old HNSW
node when one exists, so a vector result cannot outlive the document
([`delete_document_versioned`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L12947-L13140)).

## 6. Vector index placement

Lucene treats a vector field as a first-class per-segment vector value and
delegates nearest-neighbor search to a `KnnVectorsFormat` reader. A
[`KnnFloatVectorField`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/document/KnnFloatVectorField.html)
creates that field, while
[`KnnFloatVectorQuery`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/search/KnnFloatVectorQuery.html)
executes it; the codec boundary is
[`KnnVectorsFormat`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/KnnVectorsFormat.html).
For filtered kNN, Lucene can traverse HNSW subject to the filter and
automatically falls back to exact search when the filter is cheaper or graph
search reaches its visit limit; see
[`FilteredHnswGraphSearcher`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/util/hnsw/FilteredHnswGraphSearcher.html).

Lucene 10.3.1 also provides
[`Lucene99HnswScalarQuantizedVectorsFormat`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/lucene99/Lucene99HnswScalarQuantizedVectorsFormat.html)
for 4- or 7-bit scalar quantization and
[`Lucene102HnswBinaryQuantizedVectorsFormat`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/codecs/lucene102/Lucene102HnswBinaryQuantizedVectorsFormat.html)
for binary quantization. These are material representation and footprint
choices, not just different graph placement.

XERJ keeps vectors out of the lexical FTS term dictionary. A mapped
`dense_vector` is stored in `_source` for hydration and maintained by
[`index_vectors`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L9939-L10070)
in an index-level persisted HNSW artifact (`hnsw/`;
[`save_hnsw_to_disk`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L12309-L12360)).
Graph serialization is
[`HnswIndex::save_to`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-vector/src/hnsw.rs#L971-L1008),
and the doc-ID maps are persisted alongside it. That graph is not a postings
list and is not an active FTS field sidecar.

Unfiltered top-level cosine kNN may use HNSW only after field-identity,
coverage, freshness, and size checks; candidates are fetched and exact
rescored. Exact rescoring does not make candidate selection exact: HNSW remains
approximate, so candidate-set recall can differ from brute force. Filters,
nested or passage shapes, unsupported metrics, small or stale graphs, and any
failed eligibility check use XERJ's exact brute-force path. The
[`kNN dispatch`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L13844-L13934)
and
[`run_knn_hnsw`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L10487-L10530)
make those dispatch and fallback rules explicit.

Non-claims: this page does not claim byte-compatible indexes, identical merge
policies, Lucene codec compatibility, complete analyzer parity, identical
vector recall or latency, or that an Elasticsearch API name implies Lucene
internals. It also does not compare Lucene's
[`facets`](https://lucene.apache.org/core/10_3_1/facet/org/apache/lucene/facet/package-summary.html),
near-real-time reader management through
[`DirectoryReader`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/index/DirectoryReader.html)
and [`SearcherManager`](https://lucene.apache.org/core/10_3_1/core/org/apache/lucene/search/SearcherManager.html),
or its
[`backward-codecs`](https://lucene.apache.org/core/10_3_1/backward-codecs/overview-summary.html)
compatibility surface. This is an orientation guide pinned to the source
snapshot above, not a complete feature-parity claim.
