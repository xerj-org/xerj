//! On-disk zstd format compatibility across zstd crate upgrades.
//!
//! The fixtures under `tests/fixtures/zstd-0.13.3/` were written by this
//! crate's encoders linked against zstd 0.13.3 / zstd-safe 7.2.4 /
//! zstd-sys 2.0.16+zstd.1.5.7 (the versions before the RUSTSEC-2026-0391 /
//! 0398 / 0403 / 0410 bump). Every case checks two things:
//!
//! 1. **Old data still decodes**: the fixture bytes decode, with the current
//!    zstd, to exactly the corpus that produced them — i.e. existing indices
//!    stay readable.
//! 2. **New data is byte-identical**: re-encoding the same corpus with the
//!    current zstd reproduces the fixture byte for byte, so segments written
//!    after the upgrade are indistinguishable from those written before (an
//!    older binary reading a newer index, a mixed-version merge, and the
//!    flush-parity gate all keep working).
//!
//! Covered paths: the stored-field codec at the flush level (3), the merge
//! path at an operator level, the `_forcemerge` long-effort form (level 19 +
//! long-distance matching + explicit window log), and the raw zstd entry
//! points the other crates use for their on-disk formats (`encode_all` /
//! `decode_all` for doc values and hub packs, `bulk::compress` /
//! `bulk::decompress` for FTS postings and `xerj-compress` blocks, and a
//! `bulk::Compressor` with LDM + window log like `compress_long_effort_at`).
//!
//! The fixtures must only ever be regenerated deliberately, and only to pin a
//! new *intended* format: `XERJ_REGEN_ZSTD_COMPAT_FIXTURES=1 cargo test -p
//! xerj-storage --test zstd_format_compat` overwrites them with the current
//! output. A zstd upgrade that makes this test fail on its own is a format
//! change and must not be papered over by regenerating.

use std::path::PathBuf;

use xerj_storage::stored_codec::{
    decode_stored, encode_stored_v2, encode_stored_v2_forcemerge_at_level,
    encode_stored_v2_merge_at_level,
};

const STATUSES: [&str; 4] = ["active", "pending", "closed", "archived"];
const WORDS: [&str; 16] = [
    "index", "segment", "merge", "posting", "column", "vector", "shard", "replica", "query",
    "token", "field", "stored", "window", "frame", "level", "codec",
];

/// Deterministic xorshift so the corpus never depends on a RNG crate.
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// A stored section in the canonical `[{"_id","_seq_no","_source"}]` shape,
/// mixing low-cardinality strings, integers, floats, explicit nulls, nested
/// objects and long free text with repeats far apart (what LDM targets).
fn stored_corpus() -> Vec<u8> {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let sentences: Vec<String> = (0..24)
        .map(|_| {
            (0..12)
                .map(|_| WORDS[(rng.next() % WORDS.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let docs: Vec<serde_json::Value> = (0..200u64)
        .map(|i| {
            let body: Vec<&str> = (0..6)
                .map(|_| sentences[(rng.next() % sentences.len() as u64) as usize].as_str())
                .collect();
            let mut source = serde_json::json!({
                "status": STATUSES[(i % 4) as usize],
                "count": rng.next() % 10_000,
                "score": (rng.next() % 1_000_000) as f64 / 1000.0,
                "body": body.join(". "),
                "meta": { "host": format!("node-{}", i % 7), "tags": [i % 3, i % 5] },
            });
            if i % 11 == 0 {
                source["optional"] = serde_json::Value::Null;
            }
            serde_json::json!({ "_id": format!("doc-{i:05}"), "_seq_no": i, "_source": source })
        })
        .collect();
    serde_json::to_vec(&docs).unwrap()
}

/// Free text with long-range repeats for the raw-frame cases.
fn raw_corpus() -> Vec<u8> {
    let mut rng = XorShift(0xD1B5_4A32_D192_ED03);
    let mut out = Vec::new();
    while out.len() < 40 * 1024 {
        let w = WORDS[(rng.next() % WORDS.len() as u64) as usize];
        out.extend_from_slice(w.as_bytes());
        out.push(if rng.next().is_multiple_of(9) {
            b'\n'
        } else {
            b' '
        });
    }
    let head = out[..16 * 1024].to_vec();
    out.extend_from_slice(&head);
    out
}

fn ldm_compress(input: &[u8], level: i32, window_log: u32) -> Vec<u8> {
    use zstd::zstd_safe::CParameter;
    let mut c = zstd::bulk::Compressor::new(level).unwrap();
    c.set_parameter(CParameter::WindowLog(window_log)).unwrap();
    c.set_parameter(CParameter::EnableLongDistanceMatching(true))
        .unwrap();
    c.compress(input).unwrap()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/zstd-0.13.3")
        .join(format!("{name}.hex"))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2), "odd-length hex fixture");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Compare (or, in regen mode, write) one fixture. Returns the fixture bytes.
fn check(name: &str, current: &[u8]) -> Vec<u8> {
    let path = fixture_path(name);
    if std::env::var_os("XERJ_REGEN_ZSTD_COMPAT_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, to_hex(current) + "\n").unwrap();
        return current.to_vec();
    }
    let fixture = from_hex(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing fixture {}: {e}", path.display());
    }));
    assert!(
        fixture == current,
        "{name}: zstd output changed ({} B fixture vs {} B now) — this is an \
         on-disk format change, not a dependency bump",
        fixture.len(),
        current.len()
    );
    fixture
}

fn assert_stored_roundtrip(name: &str, fixture: &[u8], corpus: &[u8]) {
    let decoded = decode_stored(fixture).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
    let got: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    let want: serde_json::Value = serde_json::from_slice(corpus).unwrap();
    assert_eq!(got, want, "{name}: decoded docs differ from the corpus");
}

#[test]
fn stored_codec_flush_level_is_format_stable() {
    let corpus = stored_corpus();
    let fixture = check("stored_v2_flush_l3", &encode_stored_v2(&corpus));
    assert_stored_roundtrip("stored_v2_flush_l3", &fixture, &corpus);
}

#[test]
fn stored_codec_merge_levels_are_format_stable() {
    let corpus = stored_corpus();
    for level in [1, 9] {
        let name = format!("stored_v2_merge_l{level}");
        let fixture = check(&name, &encode_stored_v2_merge_at_level(&corpus, level));
        assert_stored_roundtrip(&name, &fixture, &corpus);
    }
}

#[test]
fn stored_codec_forcemerge_long_effort_is_format_stable() {
    let corpus = stored_corpus();
    let fixture = check(
        "stored_v2_forcemerge_l3",
        &encode_stored_v2_forcemerge_at_level(&corpus, 3),
    );
    assert_stored_roundtrip("stored_v2_forcemerge_l3", &fixture, &corpus);
    // The long-effort candidate (level 19 + LDM + window log) must have won
    // at least one column, otherwise this case would not cover that path.
    assert_ne!(
        fixture,
        encode_stored_v2(&corpus),
        "forcemerge fixture should carry a form-1 (long-effort) column"
    );
}

#[test]
fn raw_zstd_frames_are_format_stable() {
    let corpus = raw_corpus();

    for level in [1, 3, 19] {
        let name = format!("raw_encode_all_l{level}");
        let fixture = check(&name, &zstd::encode_all(corpus.as_slice(), level).unwrap());
        assert_eq!(
            zstd::decode_all(fixture.as_slice()).unwrap(),
            corpus,
            "{name}"
        );
        let mut streamed = Vec::new();
        std::io::Read::read_to_end(
            &mut zstd::stream::read::Decoder::new(fixture.as_slice()).unwrap(),
            &mut streamed,
        )
        .unwrap();
        assert_eq!(streamed, corpus, "{name} (stream decoder)");
    }

    for level in [1, 3, 19] {
        let name = format!("raw_bulk_l{level}");
        let fixture = check(&name, &zstd::bulk::compress(&corpus, level).unwrap());
        assert_eq!(
            zstd::bulk::decompress(&fixture, corpus.len()).unwrap(),
            corpus,
            "{name}"
        );
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&fixture).unwrap(),
            Some(corpus.len() as u64),
            "{name}: frame content size"
        );
        assert_eq!(
            zstd::zstd_safe::find_frame_compressed_size(&fixture).unwrap(),
            fixture.len(),
            "{name}: frame compressed size"
        );
    }

    let fixture = check("raw_ldm_l19_wlog18", &ldm_compress(&corpus, 19, 18));
    assert_eq!(zstd::decode_all(fixture.as_slice()).unwrap(), corpus);
}
