//! `xerj_common::hub_client_cache` must read what the hub serves (#2760, #3297
//! review): a signed `<slug>-<pin8>.hubpack.tar.zst` as `pack::write_pack`
//! writes it, whose catalogue `pack_sha256` is the digest of that archive, not a
//! private JSON format. The archive here is the real writer's output.

use std::fs;
use std::path::Path;

use xerj_autoindex::harvest::sign::generate_keypair;
use xerj_common::hub::{CatalogueEntry, LicenceClass, PackIndex, PackManifest, PackSource, Sketch};
use xerj_common::hub_client_cache::{
    answer_from_pack, AnswerQuery, CacheLookupOutcome, HubDiskCache, SignedDownload,
};
use xerj_hub::pack::{write_pack, PackFileEntry, MAX_MEMBERS, MAX_UNPACKED_BYTES};
use xerj_hub::{AnswerMode, DefRecord, PACK_FORMAT_VERSION};

const PIN: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";

fn manifest() -> PackManifest {
    PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: "lz4".into(),
        pin_sha: PIN.into(),
        pin_date: "2026-10-02".into(),
        built_at: "2026-10-14T00:00:00Z".into(),
        builder_version: "xerj 1.0.0".into(),
        index_prefix: "xc-lz4-b1".into(),
        indices: vec![PackIndex {
            name: "xc-lz4-b1-docs".into(),
            docs: 10,
            bytes: 2048,
        }],
        snapshot_name: "lz4-0774d055".into(),
        licence_spdx: "BSD-2-Clause".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        sources: vec![PackSource {
            repo: "lz4".into(),
            url: "https://github.com/lz4/lz4".into(),
            sha: PIN.into(),
            licence: "BSD-2-Clause".into(),
        }],
        defs_count: 2,
        files_count: 2,
    }
}

fn defs() -> Vec<DefRecord> {
    vec![
        DefRecord {
            symbol: "LZ4_compress_default".into(),
            kind: "function".into(),
            lang: "c".into(),
            file: "lib/lz4.c".into(),
            line: 700,
            text: "int LZ4_compress_default(const char* src, char* dst, int n, int cap)".into(),
            value: None,
        },
        DefRecord {
            symbol: "LZ4_VERSION_RELEASE".into(),
            kind: "define".into(),
            lang: "c".into(),
            file: "lib/lz4.h".into(),
            line: 133,
            text: "#define LZ4_VERSION_RELEASE 0".into(),
            value: Some("0".into()),
        },
    ]
}

fn files() -> Vec<PackFileEntry> {
    ["lib/lz4.c", "lib/lz4.h"]
        .into_iter()
        .map(|path| PackFileEntry {
            path: path.into(),
            lang: "c".into(),
            bytes: 100_000,
            licence: "BSD-2-Clause".into(),
        })
        .collect()
}

/// A real pack: the opaque snapshot files are packed verbatim under `snapshot/`.
fn real_pack(dir: &Path, seed: &str) -> (CatalogueEntry, SignedDownload) {
    let snapshot = dir.join("snapshot");
    fs::create_dir_all(snapshot.join("idx")).unwrap();
    fs::write(snapshot.join("manifest.json"), br#"{"snapshot":"lz4"}"#).unwrap();
    fs::write(
        snapshot.join("idx/segment_0.bin"),
        b"\x00\x01\x02binary\xff",
    )
    .unwrap();
    let archive_path = dir.join("lz4-0774d055.hubpack.tar.zst");
    let (sha256, bytes) = write_pack(
        &archive_path,
        &manifest(),
        &defs(),
        &files(),
        &snapshot,
        seed,
    )
    .unwrap();
    let download = SignedDownload {
        bytes: fs::read(&archive_path).unwrap(),
        signature_hex: fs::read_to_string(format!("{}.sig", archive_path.display())).unwrap(),
    };
    let entry = CatalogueEntry {
        slug: "lz4".into(),
        domain: "NET".into(),
        topics: vec!["compression".into()],
        ask_for: "fast block compression".into(),
        upstream: "lz4/lz4".into(),
        pin_sha: PIN.into(),
        pin_date: "2026-10-02".into(),
        refresh_cadence_days: 7,
        licence_spdx: "BSD-2-Clause".into(),
        licence_class: LicenceClass::AdaptWithAttribution,
        pack_bytes: bytes,
        pack_url: "https://hub.xerj.org/packs/lz4-0774d055.hubpack.tar.zst".into(),
        pack_sha256: sha256,
        sig_url: "https://hub.xerj.org/packs/lz4-0774d055.hubpack.tar.zst.sig".into(),
        dependency_names: vec!["lz4".into()],
        quality_grade: None,
        term_sketch: Sketch {
            m_bits: 64,
            k_hashes: 3,
            bits_base64: "AAAAAAAAAAA=".into(),
        },
    };
    (entry, download)
}

#[test]
fn the_cache_reads_a_pack_the_hub_writer_produced() {
    let work = tempfile::tempdir().unwrap();
    let (seed, public) = generate_keypair().unwrap();
    let (entry, download) = real_pack(work.path(), &seed);
    assert_eq!(
        &download.bytes[..4],
        &[0x28, 0xb5, 0x2f, 0xfd],
        "zstd frame"
    );

    let cache_dir = tempfile::tempdir().unwrap();
    let mut cache = HubDiskCache::open(cache_dir.path(), 1 << 30, 3600)
        .unwrap()
        .with_pubkey_hex(Some(&public));
    let first = cache
        .get_or_fetch_pack(&entry, 1_000, |_| Ok(download.clone()))
        .unwrap();
    assert_eq!(first.outcome, CacheLookupOutcome::FetchedAndCached);
    assert_eq!(first.value.manifest.slug, "lz4");
    assert_eq!(first.value.defs.len(), 2);
    assert_eq!(first.value.files.len(), 2);

    let by_symbol = answer_from_pack(
        &first.value,
        &entry,
        &AnswerQuery::new("lz4_version_release", AnswerMode::Def),
    )
    .unwrap();
    assert_eq!(by_symbol.hits.len(), 1);
    assert_eq!(by_symbol.hits[0].value.as_deref(), Some("0"));
    assert_eq!(by_symbol.hits[0].file, "lib/lz4.h");

    let again = cache
        .get_or_fetch_pack(&entry, 1_010, |_| panic!("served from disk"))
        .unwrap();
    assert_eq!(again.outcome, CacheLookupOutcome::HitFromDisk);
}

#[test]
fn a_pack_signed_by_a_key_other_than_the_hubs_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let (seed, _public) = generate_keypair().unwrap();
    let (_other_seed, hub_public) = generate_keypair().unwrap();
    let (entry, download) = real_pack(work.path(), &seed);

    let cache_dir = tempfile::tempdir().unwrap();
    let mut cache = HubDiskCache::open(cache_dir.path(), 1 << 30, 3600)
        .unwrap()
        .with_pubkey_hex(Some(&hub_public));
    let err = cache
        .get_or_fetch_pack(&entry, 1_000, |_| Ok(download.clone()))
        .unwrap_err();
    assert!(
        err.to_string().contains("signature verification failed"),
        "{err}"
    );
    assert!(!cache.contains_corpus("lz4"));
}

/// The reader's caps are the extractor's caps: the catalogue never lists a pack
/// one of them accepts and the other refuses.
#[test]
fn the_cache_reader_shares_the_extractors_pack_limits() {
    use xerj_common::hub_client_cache as cache;
    assert_eq!(cache::MAX_UNPACKED_BYTES, MAX_UNPACKED_BYTES);
    assert_eq!(cache::MAX_MEMBERS, MAX_MEMBERS);
}
