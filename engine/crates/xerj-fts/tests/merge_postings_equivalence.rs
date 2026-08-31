//! #876 — merging postings must produce the segment a re-analysing merge
//! would have produced.
//!
//! The merge path used to walk back to every surviving document's source
//! text and re-run the analyzer chain over it.  `FtsIndexWriter::
//! merge_from_segments` instead replays the source segments' already-built
//! posting lists.  That is only allowed to be faster — never different — so
//! every test here builds the SAME merged segment twice, once each way, and
//! compares the two.
//!
//! The comparison is deliberately at the byte level of the side-car files.
//! Hits and BM25 scores are a pure function of those bytes, so equal files are
//! a strictly stronger claim than equal search results; the search comparison
//! is kept anyway because it is the claim the issue actually makes.
//!
//! `.fst`, `.post` and `.meta` are compared byte for byte.  (`.meta` holds
//! `total_term_frequency`, which a docs-only field DOES diverge on when a
//! document repeats the same value — no fixture document does, so the byte
//! comparison is sound here, and that divergence has its own test:
//! `a_docs_only_field_carries_its_length_but_not_its_term_frequency`.)
//!
//! `.norms` is compared for the content a reader can observe, because it is
//! knowingly NOT byte-identical: byte 0 spells both "field absent" and "field
//! length <= 1", `load_norms` drops byte-0 entries on read, so a replayed
//! merge cannot reproduce a trailing run of them and writes a SHORTER dense
//! array.  The `colour` field in the fixture below is single-token precisely
//! so that case is exercised on every run instead of being accidentally
//! avoided; the exact divergence, and the fact that nothing a query reads
//! moves with it, are pinned by
//! `a_single_token_field_replays_an_equivalent_shorter_norms_array`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use xerj_fts::analyzer::AnalyzerRegistry;
use xerj_fts::index::{
    segment_indexed_field_names, FieldIndexConfig, FieldValues, FtsIndexReader, FtsIndexWriter,
    FtsMergeSource,
};
use xerj_fts::search::{FtsSearcher, PhraseQuery, Query, TermQuery};

const FIELDS: [&str; 4] = ["body", "colour", "tags", "title"];
/// The side-cars a replayed merge must reproduce byte for byte.  `.norms` is
/// excluded on purpose and checked by `assert_norms_equivalent` instead — see
/// the module comment.
const EXTENSIONS: [&str; 3] = ["fst", "post", "meta"];

fn registry() -> Arc<AnalyzerRegistry> {
    Arc::new(AnalyzerRegistry::default())
}

fn configure(writer: &mut FtsIndexWriter) {
    writer.configure_field(
        "body",
        FieldIndexConfig {
            analyzer: "standard".to_owned(),
            store_positions: true,
            store_term_vectors: false,
        },
    );
    writer.configure_field(
        "title",
        FieldIndexConfig {
            analyzer: "standard".to_owned(),
            store_positions: true,
            store_term_vectors: false,
        },
    );
    // Keyword fields are docs-only: their posting lists carry neither a
    // frequency nor a position, which is the shape the merge has to preserve.
    writer.configure_field(
        "tags",
        FieldIndexConfig {
            analyzer: "keyword".to_owned(),
            store_positions: false,
            store_term_vectors: false,
        },
    );
    // A SINGLE-TOKEN keyword field.  `norm_u16_to_u8` maps length 1 to byte 0,
    // the same byte it uses for "field absent", and the reader drops byte-0
    // entries — so every document's norm here is unrecoverable by a replay and
    // this field's `.norms` is the one side-car the merge cannot reproduce byte
    // for byte.  It is in the fixture so that gap is measured, not dodged.
    writer.configure_field(
        "colour",
        FieldIndexConfig {
            analyzer: "keyword".to_owned(),
            store_positions: false,
            store_term_vectors: false,
        },
    );
}

/// 300 documents, wide enough that the hottest terms cross the 128-doc PFOR
/// block boundary (so the merged segment exercises the full-block encoder and
/// its cross-block delta chain, not just the vbyte residual).
const DOC_COUNT: usize = 300;

fn document(index: usize) -> HashMap<String, FieldValues> {
    const NOUNS: [&str; 6] = ["fox", "hound", "otter", "falcon", "badger", "heron"];
    const COLOURS: [&str; 4] = ["amber", "cobalt", "russet", "verdant"];

    let mut fields: HashMap<String, FieldValues> = HashMap::new();

    // `quick` and `brown` land in every document (a >128-posting term);
    // `NOUNS[..]` repeats twice in the same value so term frequencies and
    // position lists are both non-trivial.
    let body = format!(
        "quick brown {noun} leapt past a quick {noun} and settled at {index}",
        noun = NOUNS[index % NOUNS.len()],
    );
    if index.is_multiple_of(7) {
        // Multi-valued text: the second value's positions start after
        // POSITION_INCREMENT_GAP, which a merge must carry through untouched.
        fields.insert(
            "body".to_owned(),
            FieldValues::from_values(vec![body, format!("a second value about {index}")]),
        );
    } else {
        fields.insert("body".to_owned(), FieldValues::One(body));
    }

    fields.insert(
        "tags".to_owned(),
        FieldValues::from_values(vec![
            format!("tag-{}", index % 11),
            format!("colour-{}", COLOURS[index % COLOURS.len()]),
        ]),
    );

    // Exactly one keyword token, on EVERY document — so this field's norm is
    // byte 0 for the highest merged ordinal, which is what makes the replayed
    // `.norms` shorter than the re-analysed one.
    fields.insert(
        "colour".to_owned(),
        FieldValues::One(COLOURS[index % COLOURS.len()].to_owned()),
    );

    // Present on only a third of the documents, so the merged segment has a
    // field whose document set is sparse in the merged ordinal space.
    if index.is_multiple_of(3) {
        fields.insert(
            "title".to_owned(),
            FieldValues::One(format!("Report {index} of the survey")),
        );
    }

    fields
}

/// Deterministic pseudo-shuffle standing in for `_seq_no` order: the engine
/// sorts every batch's survivors by sequence number, so documents from
/// different input segments INTERLEAVE in the merged ordinal space.  That is
/// what forces the merge to re-sort each term's posting list.
fn sequence_number(index: usize) -> usize {
    (index * 97) % DOC_COUNT
}

struct Fixture {
    dir: tempfile::TempDir,
    /// Per source segment: the documents it holds, in its own ordinal order.
    segments: Vec<Vec<usize>>,
}

impl Fixture {
    /// Three source segments, round-robin over the document set.
    fn build(survives: impl Fn(usize) -> bool) -> (Self, Vec<usize>) {
        let dir = tempfile::TempDir::new().unwrap();
        let mut segments: Vec<Vec<usize>> = vec![Vec::new(); 3];
        for index in 0..DOC_COUNT {
            segments[index % 3].push(index);
        }

        for (segment, documents) in segments.iter().enumerate() {
            let mut writer = FtsIndexWriter::new(dir.path(), format!("seg-{segment}"), registry());
            configure(&mut writer);
            let batch: Vec<(String, HashMap<String, FieldValues>, ())> = documents
                .iter()
                .map(|index| (index.to_string(), document(*index), ()))
                .collect();
            writer.add_documents_parallel(&batch);
            writer.finish().unwrap();
        }

        // Survivors, in merged (sequence-number) order.
        let mut survivors: Vec<usize> = (0..DOC_COUNT).filter(|index| survives(*index)).collect();
        survivors.sort_by_key(|index| sequence_number(*index));

        (Fixture { dir, segments }, survivors)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The merged segment a RE-ANALYSING merge writes: every surviving
    /// document's field values, fed through the analyzer again.
    fn reanalysed(&self, survivors: &[usize], segment_id: &str) {
        let mut writer = FtsIndexWriter::new(self.path(), segment_id, registry());
        configure(&mut writer);
        let batch: Vec<(String, HashMap<String, FieldValues>, ())> = survivors
            .iter()
            .map(|index| (index.to_string(), document(*index), ()))
            .collect();
        writer.add_documents_parallel(&batch);
        writer.finish().unwrap();
    }

    /// The merged segment a POSTINGS merge writes: no documents, no analyzer,
    /// just the source segments' side-cars and a doc-id remap.
    fn replayed(&self, survivors: &[usize], segment_id: &str) {
        let merged_ordinal: HashMap<usize, u32> = survivors
            .iter()
            .enumerate()
            .map(|(ordinal, index)| (*index, ordinal as u32))
            .collect();

        let readers: Vec<FtsIndexReader> = (0..self.segments.len())
            .map(|segment| {
                FtsIndexReader::open(self.path(), format!("seg-{segment}"), &FIELDS).unwrap()
            })
            .collect();
        let doc_maps: Vec<Vec<Option<u32>>> = self
            .segments
            .iter()
            .map(|documents| {
                documents
                    .iter()
                    .map(|index| merged_ordinal.get(index).copied())
                    .collect()
            })
            .collect();
        let sources: Vec<FtsMergeSource<'_>> = readers
            .iter()
            .zip(doc_maps.iter())
            .map(|(reader, doc_map)| FtsMergeSource {
                reader,
                doc_map: doc_map.as_slice(),
            })
            .collect();

        let mut writer = FtsIndexWriter::new(self.path(), segment_id, registry());
        configure(&mut writer);
        writer.merge_from_segments(&sources).unwrap();
        writer.finish().unwrap();
    }
}

fn sidecar(dir: &Path, segment_id: &str, field: &str, extension: &str) -> Vec<u8> {
    std::fs::read(dir.join(format!("{segment_id}.{field}.{extension}")))
        .unwrap_or_else(|error| panic!("reading {segment_id}.{field}.{extension}: {error}"))
}

fn assert_sidecars_identical(dir: &Path, expected: &str, actual: &str, extensions: &[&str]) {
    for field in FIELDS {
        for extension in extensions {
            let want = sidecar(dir, expected, field, extension);
            let got = sidecar(dir, actual, field, extension);
            assert_eq!(
                want,
                got,
                "field '{field}' side-car .{extension} differs between the re-analysed \
                 merge and the replayed merge ({} vs {} bytes)",
                want.len(),
                got.len()
            );
        }
    }
}

/// The declared length of a `.norms` dense array, straight out of the ZNM1
/// header (`"ZNM1"`, u8 encoding, u32 dense_len, u32 payload_len).
fn norms_dense_len(dir: &Path, segment_id: &str, field: &str) -> u32 {
    let bytes = sidecar(dir, segment_id, field, "norms");
    assert!(
        bytes.len() >= 13 && &bytes[..4] == b"ZNM1",
        "{segment_id}.{field}.norms is not in the ZNM1 format this test reads"
    );
    u32::from_le_bytes(bytes[5..9].try_into().unwrap())
}

/// `.norms` is the one side-car a replay cannot reproduce byte for byte, so it
/// is compared for what a reader can actually see instead.
///
/// The replayed array is allowed to be SHORTER — byte 0 means both "absent"
/// and "length <= 1", `load_norms` drops those entries, so the replay stops at
/// the last non-zero document — but every norm byte, every dequantised
/// `field_length` and the field statistics behind `avgdl` must match exactly.
/// A shorter array with an identical live table differs only in trailing
/// zeros, which is precisely what no query can observe.
fn assert_norms_equivalent(dir: &Path, expected: &str, actual: &str, doc_count: u32) {
    let want_reader = FtsIndexReader::open(dir, expected, &FIELDS).unwrap();
    let got_reader = FtsIndexReader::open(dir, actual, &FIELDS).unwrap();
    for field in FIELDS {
        let want: Vec<(u32, u8)> = want_reader.field_norm_bytes(field).collect();
        let got: Vec<(u32, u8)> = got_reader.field_norm_bytes(field).collect();
        assert_eq!(
            want, got,
            "field '{field}': the norm bytes a reader loads differ between the              re-analysed merge and the replayed merge"
        );
        for doc_id in 0..doc_count {
            assert_eq!(
                want_reader.field_length(field, doc_id),
                got_reader.field_length(field, doc_id),
                "field '{field}': length of merged doc {doc_id} differs"
            );
        }
        let want_len = norms_dense_len(dir, expected, field);
        let got_len = norms_dense_len(dir, actual, field);
        assert!(
            got_len <= want_len,
            "field '{field}': the replayed .norms dense array ({got_len}) may be              shorter than the re-analysed one ({want_len}) but never longer — a              longer one would mean the replay invented a document"
        );
    }
}

fn hits(dir: &Path, segment_id: &str, query: &Query) -> Vec<(u32, u32)> {
    let reader = Arc::new(FtsIndexReader::open(dir, segment_id, &FIELDS).unwrap());
    let searcher = FtsSearcher::new(reader, registry());
    searcher
        .search(query, 500, false)
        .unwrap()
        .into_iter()
        // Compare the score's exact bit pattern: "same BM25 score" means
        // identical, not close.
        .map(|hit| (hit.doc_id, hit.score.to_bits()))
        .collect()
}

fn queries() -> Vec<Query> {
    vec![
        Query::Term(TermQuery::new("body", "quick")),
        Query::Term(TermQuery::new("body", "otter")),
        Query::Phrase(PhraseQuery::new(
            "body",
            vec!["quick".to_owned(), "brown".to_owned()],
        )),
        // Spans the position-increment gap between two values of the same
        // multi-valued field: it must match NOTHING, before and after.
        Query::Phrase(PhraseQuery::new(
            "body",
            vec!["17".to_owned(), "a".to_owned()],
        )),
        Query::Term(TermQuery::new("tags", "tag-3")),
        Query::Term(TermQuery::new("tags", "colour-cobalt")),
        // The single-token field whose `.norms` the replay cannot reproduce:
        // its hits and its BM25 scores must be identical anyway.
        Query::Term(TermQuery::new("colour", "cobalt")),
        Query::Term(TermQuery::new("title", "survey")),
        Query::MatchAll,
    ]
}

fn assert_searches_identical(dir: &Path, expected: &str, actual: &str) {
    for query in queries() {
        let want = hits(dir, expected, &query);
        let got = hits(dir, actual, &query);
        assert_eq!(
            want, got,
            "hit list / scores differ for {query:?} between the re-analysed merge and \
             the replayed merge"
        );
    }
}

/// `.fst`, `.post` and `.meta` byte for byte; `.norms` for everything a
/// reader can observe (see `assert_norms_equivalent`).
#[test]
fn a_merge_with_no_deletes_reproduces_the_re_analysed_segment() {
    let (fixture, survivors) = Fixture::build(|_| true);
    assert_eq!(survivors.len(), DOC_COUNT);

    fixture.reanalysed(&survivors, "merged-reanalysed");
    fixture.replayed(&survivors, "merged-replayed");

    assert_sidecars_identical(
        fixture.path(),
        "merged-reanalysed",
        "merged-replayed",
        &EXTENSIONS,
    );
    assert_norms_equivalent(
        fixture.path(),
        "merged-reanalysed",
        "merged-replayed",
        survivors.len() as u32,
    );
    assert_searches_identical(fixture.path(), "merged-reanalysed", "merged-replayed");
}

/// Same comparison as above, with a fifth of the documents gone.
#[test]
fn a_merge_that_drops_documents_reproduces_the_re_analysed_segment() {
    // Every fifth document is deleted or superseded, so the merge must skip
    // it, close the ordinal gap it leaves, and reclaim its share of the
    // field-length statistics.
    let (fixture, survivors) = Fixture::build(|index| !index.is_multiple_of(5));
    assert_eq!(survivors.len(), DOC_COUNT - DOC_COUNT / 5);

    fixture.reanalysed(&survivors, "dropped-reanalysed");
    fixture.replayed(&survivors, "dropped-replayed");

    assert_sidecars_identical(
        fixture.path(),
        "dropped-reanalysed",
        "dropped-replayed",
        &EXTENSIONS,
    );
    assert_norms_equivalent(
        fixture.path(),
        "dropped-reanalysed",
        "dropped-replayed",
        survivors.len() as u32,
    );
    assert_searches_identical(fixture.path(), "dropped-reanalysed", "dropped-replayed");
}

#[test]
fn field_statistics_survive_a_merge_that_drops_documents() {
    // Guards the number BM25 length-normalisation actually reads: avgdl =
    // total_field_length / total_docs.  A merge that lost either would still
    // return the same hit SET while silently re-ranking it.
    let (fixture, survivors) = Fixture::build(|index| !index.is_multiple_of(5));
    fixture.reanalysed(&survivors, "stats-reanalysed");
    fixture.replayed(&survivors, "stats-replayed");

    let expected = FtsIndexReader::open(fixture.path(), "stats-reanalysed", &FIELDS).unwrap();
    let actual = FtsIndexReader::open(fixture.path(), "stats-replayed", &FIELDS).unwrap();
    for field in FIELDS {
        let want = expected.field_stats(field).unwrap();
        let got = actual.field_stats(field).unwrap();
        assert_eq!(
            (want.total_docs, want.total_field_length),
            (got.total_docs, got.total_field_length),
            "field '{field}' statistics diverged across the merge"
        );
        for doc_id in 0..survivors.len() as u32 {
            assert_eq!(
                expected.field_length(field, doc_id),
                actual.field_length(field, doc_id),
                "field '{field}' length of merged doc {doc_id} diverged"
            );
        }
    }
}

#[test]
fn merging_needs_no_documents_only_the_segments() {
    // The point of the change, stated as a test: `merge_from_segments` is
    // handed readers and a doc-id map — never a document, never an analyzer
    // input — and still produces a searchable segment.
    let (fixture, survivors) = Fixture::build(|_| true);
    fixture.replayed(&survivors, "sourceless");

    let found = hits(
        fixture.path(),
        "sourceless",
        &Query::Term(TermQuery::new("body", "quick")),
    );
    assert_eq!(
        found.len(),
        DOC_COUNT,
        "every document should still match the term that appears in all of them"
    );
}

#[test]
fn side_car_field_names_read_back_from_the_segment_directory() {
    let (fixture, _survivors) = Fixture::build(|_| true);
    let names = segment_indexed_field_names(fixture.path(), "seg-0", &[]).unwrap();
    let mut expected: Vec<String> = FIELDS.iter().map(|field| (*field).to_owned()).collect();
    expected.sort();
    assert_eq!(names, expected);
}

#[test]
fn an_unresolvable_digest_component_is_refused_not_skipped() {
    // A field name that cannot be a portable filename is stored under a
    // SHA-256 digest.  If the caller cannot tell the merge which name that
    // digest belongs to, enumerating must fail loudly: quietly omitting the
    // field would drop its postings out of the merged segment and stop its
    // terms matching, with nothing in the log.
    let dir = tempfile::TempDir::new().unwrap();
    let unsafe_field = "weird/field:name";
    let mut writer = FtsIndexWriter::new(dir.path(), "seg-encoded", registry());
    writer.configure_field(unsafe_field, FieldIndexConfig::default());
    let batch: Vec<(String, HashMap<String, FieldValues>, ())> = vec![(
        "1".to_owned(),
        HashMap::from([(
            unsafe_field.to_owned(),
            FieldValues::One("hello world".to_owned()),
        )]),
        (),
    )];
    writer.add_documents_parallel(&batch);
    writer.publish_encoded_filename_layout().unwrap();
    writer.finish().unwrap();

    assert!(
        segment_indexed_field_names(dir.path(), "seg-encoded", &[]).is_err(),
        "an unknown digest component must be an error"
    );
    let known = vec![unsafe_field.to_owned()];
    assert_eq!(
        segment_indexed_field_names(dir.path(), "seg-encoded", &known).unwrap(),
        known,
        "a digest component must resolve once the caller supplies the name"
    );
}

#[test]
fn a_docs_only_field_carries_its_length_but_not_its_term_frequency() {
    // The one place a replayed merge is knowingly NOT byte-identical, pinned
    // so it cannot drift further.  A docs-only (`keyword`) posting list never
    // stored a per-document frequency — its reader synthesises 1 — so a
    // document that repeats the same keyword value merges with ttf = df.
    // Everything a query can observe is unchanged: the doc frequency, the
    // posting list bytes, the norm, and the field-length statistics all
    // survive, because `total_field_length` is carried from the source .meta
    // rather than recounted from the postings.
    let dir = tempfile::TempDir::new().unwrap();
    let fields = HashMap::from([(
        "tags".to_owned(),
        FieldValues::from_values(vec!["dup".to_owned(), "dup".to_owned(), "solo".to_owned()]),
    )]);
    let batch: Vec<(String, HashMap<String, FieldValues>, ())> =
        vec![("1".to_owned(), fields.clone(), ())];

    let mut source = FtsIndexWriter::new(dir.path(), "dup-source", registry());
    configure(&mut source);
    source.add_documents_parallel(&batch);
    source.finish().unwrap();

    let mut reference = FtsIndexWriter::new(dir.path(), "dup-reanalysed", registry());
    configure(&mut reference);
    reference.add_documents_parallel(&batch);
    reference.finish().unwrap();

    let reader = FtsIndexReader::open(dir.path(), "dup-source", &FIELDS).unwrap();
    let doc_map = [Some(0u32)];
    let mut merged = FtsIndexWriter::new(dir.path(), "dup-replayed", registry());
    configure(&mut merged);
    merged
        .merge_from_segments(&[FtsMergeSource {
            reader: &reader,
            doc_map: &doc_map,
        }])
        .unwrap();
    merged.finish().unwrap();

    // Query-visible state: identical.
    for extension in ["fst", "post", "norms"] {
        assert_eq!(
            sidecar(dir.path(), "dup-reanalysed", "tags", extension),
            sidecar(dir.path(), "dup-replayed", "tags", extension),
            "docs-only .{extension} must survive a repeated value byte for byte"
        );
    }
    let expected = FtsIndexReader::open(dir.path(), "dup-reanalysed", &FIELDS).unwrap();
    let actual = FtsIndexReader::open(dir.path(), "dup-replayed", &FIELDS).unwrap();
    assert_eq!(
        expected.field_stats("tags").unwrap().total_field_length,
        actual.field_stats("tags").unwrap().total_field_length,
        "the field length behind avgdl must count the repeat"
    );
    assert_eq!(
        expected.lookup_term("tags", "dup").unwrap().doc_frequency,
        actual.lookup_term("tags", "dup").unwrap().doc_frequency,
    );

    // The known, documented divergence.
    assert_eq!(
        expected
            .lookup_term("tags", "dup")
            .unwrap()
            .total_term_frequency,
        2,
        "re-analysis counts the repeated keyword value twice"
    );
    assert_eq!(
        actual
            .lookup_term("tags", "dup")
            .unwrap()
            .total_term_frequency,
        1,
        "a replayed docs-only posting can only report the frequency its \
         on-disk format kept, which is one per document"
    );
}

#[test]
fn a_single_token_field_replays_an_equivalent_shorter_norms_array() {
    // The SECOND place a replayed merge is knowingly not byte-identical,
    // pinned so it cannot drift and so it cannot silently stop being covered.
    //
    // `norm_u16_to_u8` returns byte 0 for length 0 AND for length 1, and
    // `load_norms` treats byte 0 as "no entry".  Re-analysis pushes a norm for
    // every document that CARRIES the field - byte 0 included - and
    // `write_field_static` sizes the dense array from the last such entry, so
    // it writes one byte per document up to the highest one carrying the
    // field.  A replay has no byte-0 entries to push, so its array stops at
    // the last NON-zero document; for a field that is single-token everywhere
    // that is no document at all, and the array collapses to one byte.
    //
    // The difference is trailing zeros.  Both files load to the same (empty)
    // norms table, so `field_length`, `_score` and the hit list are unchanged
    // - which is what `assert_norms_equivalent` and `assert_searches_identical`
    // check on every merge test above.  This test states the mechanism
    // directly: the fixture really is byte-0 throughout, the replayed array
    // really is shorter, and nothing observable moves.
    let (fixture, survivors) = Fixture::build(|_| true);
    fixture.reanalysed(&survivors, "norms-reanalysed");
    fixture.replayed(&survivors, "norms-replayed");

    // Precondition: `colour` really is the byte-0 case.  If a future fixture
    // change gave it two tokens, this assertion fails rather than quietly
    // turning the test into a no-op - which is exactly how this case escaped
    // the original suite.
    let source = FtsIndexReader::open(fixture.path(), "seg-0", &FIELDS).unwrap();
    assert_eq!(
        source.field_norm_bytes("colour").count(),
        0,
        "'colour' must analyse to one token per document (norm byte 0) for this \
         test to be testing anything"
    );
    assert!(
        source.field_stats("colour").unwrap().total_docs > 0,
        "'colour' must actually be indexed in the source segment"
    );

    // The divergence, stated exactly: re-analysis sizes the array from the
    // last document carrying the field, the replay from the last non-zero one.
    let reanalysed_len = norms_dense_len(fixture.path(), "norms-reanalysed", "colour");
    let replayed_len = norms_dense_len(fixture.path(), "norms-replayed", "colour");
    assert_eq!(
        reanalysed_len,
        survivors.len() as u32,
        "re-analysis writes one norm byte per document carrying the field"
    );
    assert_eq!(
        replayed_len, 1,
        "a replay with no non-zero norm has nothing to size the array from"
    );

    // ...and nothing a reader can see moves with it.
    let expected = FtsIndexReader::open(fixture.path(), "norms-reanalysed", &FIELDS).unwrap();
    let actual = FtsIndexReader::open(fixture.path(), "norms-replayed", &FIELDS).unwrap();
    assert_eq!(expected.field_norm_bytes("colour").count(), 0);
    assert_eq!(actual.field_norm_bytes("colour").count(), 0);
    for doc_id in 0..survivors.len() as u32 {
        assert_eq!(
            expected.field_length("colour", doc_id),
            actual.field_length("colour", doc_id),
            "merged doc {doc_id} field length diverged"
        );
    }
    assert_eq!(
        expected.field_stats("colour").unwrap().total_docs,
        actual.field_stats("colour").unwrap().total_docs,
    );
    assert_eq!(
        expected.field_stats("colour").unwrap().total_field_length,
        actual.field_stats("colour").unwrap().total_field_length,
    );
    // The other three side-cars stay byte for byte identical for this field.
    for extension in EXTENSIONS {
        assert_eq!(
            sidecar(fixture.path(), "norms-reanalysed", "colour", extension),
            sidecar(fixture.path(), "norms-replayed", "colour", extension),
            "single-token keyword .{extension} must survive the merge byte for byte"
        );
    }
    assert_eq!(
        hits(
            fixture.path(),
            "norms-reanalysed",
            &Query::Term(TermQuery::new("colour", "cobalt"))
        ),
        hits(
            fixture.path(),
            "norms-replayed",
            &Query::Term(TermQuery::new("colour", "cobalt"))
        ),
        "hits and scores over a single-token field must be identical despite \
         the shorter .norms"
    );
}
