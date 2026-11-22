use super::*;
use crate::hub::{PackIndex, PackSource, Sketch};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::cell::Cell;

const NOW: u64 = 1_700_000_000;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A throwaway hub signing key. `sign` makes the detached signature the hub's
/// `.sig` files carry: the raw ed25519 signature of the exact bytes, as hex.
struct TestHub {
    pair: Ed25519KeyPair,
    pubkey_hex: String,
}

impl TestHub {
    fn new(seed: u8) -> Self {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap();
        let pubkey_hex = hex(pair.public_key().as_ref());
        Self { pair, pubkey_hex }
    }

    fn sign(&self, bytes: &[u8]) -> String {
        hex(self.pair.sign(bytes).as_ref())
    }

    fn download(&self, bytes: &[u8]) -> SignedDownload {
        SignedDownload {
            bytes: bytes.to_vec(),
            signature_hex: self.sign(bytes),
        }
    }

    fn catalogue_download(&self, entries: &[CatalogueEntry]) -> SignedDownload {
        self.download(&serde_json::to_vec(&catalogue(entries.to_vec())).unwrap())
    }

    fn cache(&self, dir: &Path, max_bytes: u64, ttl_secs: u64) -> HubDiskCache {
        HubDiskCache::open(dir, max_bytes, ttl_secs)
            .unwrap()
            .with_pubkey_hex(Some(&self.pubkey_hex))
    }
}

fn pin(n: u8) -> String {
    std::iter::repeat_n(char::from_digit(u32::from(n) % 16, 16).unwrap(), 40).collect()
}

fn def(symbol: &str, file: &str, line: u32, value: Option<&str>) -> DefRecord {
    DefRecord {
        symbol: symbol.to_string(),
        kind: "function".to_string(),
        lang: "c".to_string(),
        file: file.to_string(),
        line,
        text: format!("int {symbol}(void);"),
        value: value.map(str::to_string),
    }
}

fn file(path: &str, licence: &str) -> PackFileEntry {
    PackFileEntry {
        path: path.to_string(),
        lang: "c".to_string(),
        bytes: 100,
        licence: licence.to_string(),
    }
}

fn manifest(slug: &str, pin_sha: &str, class: LicenceClass) -> PackManifest {
    PackManifest {
        format_version: PACK_FORMAT_VERSION,
        slug: slug.to_string(),
        pin_sha: pin_sha.to_string(),
        pin_date: "2026-11-15".to_string(),
        built_at: "2026-11-15T10:00:00Z".to_string(),
        builder_version: "1.0.0-rc.104".to_string(),
        index_prefix: format!("xc-{slug}-b1"),
        indices: vec![PackIndex {
            name: format!("xc-{slug}-b1-defs"),
            docs: 2,
            bytes: 512,
        }],
        snapshot_name: format!("snap-{slug}"),
        licence_spdx: "BSD-2-Clause".to_string(),
        licence_class: class,
        sources: vec![PackSource {
            repo: format!("{slug}/{slug}"),
            url: format!("https://github.com/{slug}/{slug}"),
            sha: pin_sha.to_string(),
            licence: "BSD-2-Clause".to_string(),
        }],
        defs_count: 1,
        files_count: 1,
    }
}

fn jsonl<T: Serialize>(items: &[T]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.extend_from_slice(&serde_json::to_vec(item).unwrap());
        out.push(b'\n');
    }
    out
}

/// tar + zstd of exactly these members, in this order.
fn raw_archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in members {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, *data).unwrap();
    }
    zstd::encode_all(builder.into_inner().unwrap().as_slice(), 3).unwrap()
}

/// Incompressible filler, so a pack's size follows `n`.
fn filler(n: usize) -> Vec<u8> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

struct Spec {
    slug: String,
    pin: String,
    class: LicenceClass,
    defs: Vec<DefRecord>,
    files: Vec<PackFileEntry>,
    snapshot_bytes: usize,
}

impl Spec {
    fn new(slug: &str, pin_sha: &str) -> Self {
        Self {
            slug: slug.to_string(),
            pin: pin_sha.to_string(),
            class: LicenceClass::AdaptWithAttribution,
            defs: vec![def("LZ4_compress_default", "lib/lz4.h", 142, Some("142"))],
            files: vec![
                file("lib/lz4.c", "BSD-2-Clause"),
                file("lib/lz4.h", "BSD-2-Clause"),
            ],
            snapshot_bytes: 64,
        }
    }

    /// The archive the hub serves: the members and `SHA256SUMS` of
    /// `xerj-hub`'s `pack::write_pack`, snapshot payload included.
    fn archive(&self) -> Vec<u8> {
        let pack_json =
            serde_json::to_vec_pretty(&manifest(&self.slug, &self.pin, self.class)).unwrap();
        let mut members: Vec<(String, Vec<u8>)> = vec![
            ("pack.json".to_string(), pack_json),
            ("defs.jsonl".to_string(), jsonl(&self.defs)),
            ("files.jsonl".to_string(), jsonl(&self.files)),
            (
                "snapshot/seg-0.bin".to_string(),
                filler(self.snapshot_bytes),
            ),
        ];
        members.sort_by(|a, b| a.0.cmp(&b.0));
        let sums: String = members
            .iter()
            .map(|(path, data)| format!("{}  {path}\n", sha256_hex(data)))
            .collect();
        let mut all: Vec<(&str, &[u8])> = vec![("SHA256SUMS", sums.as_bytes())];
        all.extend(members.iter().map(|(p, d)| (p.as_str(), d.as_slice())));
        raw_archive(&all)
    }
}

fn entry_for(spec: &Spec, archive: &[u8]) -> CatalogueEntry {
    CatalogueEntry {
        slug: spec.slug.clone(),
        domain: "NET".to_string(),
        topics: vec!["compression".to_string()],
        ask_for: "fast block compression".to_string(),
        upstream: format!("{0}/{0}", spec.slug),
        pin_sha: spec.pin.clone(),
        pin_date: "2026-11-15".to_string(),
        refresh_cadence_days: 7,
        licence_spdx: "BSD-2-Clause".to_string(),
        licence_class: spec.class,
        pack_bytes: archive.len() as u64,
        pack_url: format!("https://hub.xerj.org/packs/{}.hubpack.tar.zst", spec.slug),
        pack_sha256: sha256_hex(archive),
        sig_url: format!(
            "https://hub.xerj.org/packs/{}.hubpack.tar.zst.sig",
            spec.slug
        ),
        dependency_names: vec![spec.slug.clone()],
        quality_grade: Some("G7 5/5".to_string()),
        term_sketch: Sketch {
            m_bits: 64,
            k_hashes: 3,
            bits_base64: "AAAAAAAAAAA=".to_string(),
        },
    }
}

fn catalogue(entries: Vec<CatalogueEntry>) -> Catalogue {
    Catalogue {
        format_version: CATALOGUE_FORMAT_VERSION,
        generated_at: "2026-11-15T12:00:00Z".to_string(),
        corpora: entries,
    }
}

fn fetch_failure() -> HubModeError {
    HubModeError::HubFetch {
        target: "https://hub.xerj.org".to_string(),
        reason: "connection refused".to_string(),
    }
}

fn hub_only() -> McpRuntimeMode {
    McpRuntimeMode::detect("http://127.0.0.1:9200", DEFAULT_HUB_URL, 50, |_, _| None)
}

fn pack_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(PACK_FILE_PREFIX))
        .collect();
    names.sort();
    names
}

#[test]
fn no_node_startup_detects_hub_only_fallback_and_lists_pack_tools() {
    let hub = TestHub::new(1);
    let mode = McpRuntimeMode::detect(
        "http://127.0.0.1:9200",
        DEFAULT_HUB_URL,
        DEFAULT_PROBE_BUDGET_MS,
        |_url, _budget| None,
    );
    assert!(!mode.has_local_node());
    assert!(mode
        .fallback_notice()
        .unwrap()
        .contains("running in hub-only mode"));

    let dir = tempfile::tempdir().unwrap();
    let router = HubFirstRouter::new(mode, hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60));
    let tools = router.list_tools();
    for hub_tool in ["xerj_hub_find", "xerj_hub_def", "xerj_hub_ask"] {
        let t = tools.iter().find(|t| t.name == hub_tool).unwrap();
        assert!(t.available, "{hub_tool} must be available without a node");
        assert_eq!(t.routed_via, "hub_client_cache");
    }
    for node_tool in ["xerj_search", "xerj_def", "xerj_map"] {
        let t = tools.iter().find(|t| t.name == node_tool).unwrap();
        assert!(!t.available, "{node_tool} requires a local node");
        assert!(t
            .note
            .as_deref()
            .unwrap()
            .contains("no local xerj node reachable"));
    }
}

#[test]
fn probe_exceeding_startup_budget_falls_back_to_hub_only() {
    let mode = McpRuntimeMode::detect("http://localhost:9200", DEFAULT_HUB_URL, 100, |_, _| {
        Some(250)
    });
    assert!(!mode.has_local_node());
    assert!(mode.fallback_notice().unwrap().contains("within 100ms"));
}

#[test]
fn local_node_reachable_routes_private_index_tools_to_node_and_keeps_hub_tools() {
    let hub = TestHub::new(1);
    let mode = McpRuntimeMode::detect(
        "http://127.0.0.1:9200/",
        DEFAULT_HUB_URL,
        DEFAULT_PROBE_BUDGET_MS,
        |_url, _budget| Some(12),
    );
    assert!(mode.has_local_node());
    assert!(mode.fallback_notice().is_none());

    let dir = tempfile::tempdir().unwrap();
    let router = HubFirstRouter::new(mode, hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60));
    for node_tool in ["xerj_search", "xerj_def", "xerj_map", "xerj_status"] {
        assert_eq!(
            router.route_tool(node_tool).unwrap(),
            RouteTarget::LocalNode {
                base_url: "http://127.0.0.1:9200".to_string()
            }
        );
    }
    assert_eq!(
        router.route_tool("xerj_hub_ask").unwrap(),
        RouteTarget::HubClient {
            hub_url: DEFAULT_HUB_URL.to_string(),
            fallback_notice: None,
        }
    );

    let other = tempfile::tempdir().unwrap();
    let fallback = HubFirstRouter::new(
        hub_only(),
        hub.cache(other.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60),
    );
    let err = fallback.route_tool("xerj_search").unwrap_err();
    assert!(matches!(err, HubModeError::LocalNodeRequired { tool, .. } if tool == "xerj_search"));
}

/// `xerj mcp` serves the `xerj_hub_*` tools only when a hub key is configured.
#[test]
fn hub_tools_are_refused_without_a_hub_key() {
    let dir = tempfile::tempdir().unwrap();
    let cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60)
        .unwrap()
        .with_pubkey_hex(None);
    assert!(!cache.can_verify());
    let router = HubFirstRouter::new(hub_only(), cache);

    for tool in ["xerj_hub_ask", "xerj_hub_def", "xerj_hub_find"] {
        let err = router.route_tool(tool).unwrap_err();
        assert!(matches!(err, HubModeError::NoHubKey), "{tool}: {err}");
        assert!(err.to_string().contains("no hub signing key"), "{err}");
    }
    // These two are served by `xerj mcp` whatever the key.
    for tool in ["xerj_corpus_feedback", "xerj_field_report"] {
        assert!(matches!(
            router.route_tool(tool),
            Ok(RouteTarget::HubClient { .. })
        ));
    }
    let tools = router.list_tools();
    for tool in ["xerj_hub_ask", "xerj_hub_def", "xerj_hub_find"] {
        let t = tools.iter().find(|t| t.name == tool).unwrap();
        assert!(!t.available, "{tool}");
        assert!(t.note.as_deref().unwrap().contains("no hub signing key"));
    }
    assert!(
        tools
            .iter()
            .find(|t| t.name == "xerj_field_report")
            .unwrap()
            .available
    );
}

#[test]
fn a_cache_without_a_hub_key_refuses_before_fetching_anything() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = HubDiskCache::open(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60)
        .unwrap()
        .with_pubkey_hex(None);
    let cat = hub.catalogue_download(std::slice::from_ref(&entry));
    assert!(matches!(
        cache.get_or_fetch_catalogue(NOW, || panic!("no fetch without a key")),
        Err(HubModeError::NoHubKey)
    ));
    assert!(matches!(
        cache.put_catalogue(&cat, NOW),
        Err(HubModeError::NoHubKey)
    ));
    assert!(matches!(
        cache.get_or_fetch_pack(&entry, NOW, |_| panic!("no fetch without a key")),
        Err(HubModeError::NoHubKey)
    ));
}

#[test]
fn defaults_to_the_hub_disk_budget_and_a_24_hour_ttl() {
    assert_eq!(DEFAULT_MCP_CACHE_MAX_BYTES, DISK_BUDGET_BYTES);
    assert_eq!(DEFAULT_CACHE_TTL_SECS, 86_400);
    assert_eq!(DEFAULT_MAX_HITS, 50);
}

/// The class of failure #3297 was reviewed for: real hub bytes. The archive is
/// zstd + tar with the pack's own members, signed over the archive bytes, and
/// the catalogue's `pack_sha256` is the digest of that archive.
#[test]
fn a_real_hubpack_verifies_answers_and_the_second_call_is_served_from_disk() {
    let hub = TestHub::new(7);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    assert_eq!(&archive[..4], &[0x28, 0xb5, 0x2f, 0xfd], "zstd frame");
    let entry = entry_for(&spec, &archive);
    let cat = hub.catalogue_download(&[entry]);
    let pack = hub.download(&archive);

    let dir = tempfile::tempdir().unwrap();
    let mut router = HubFirstRouter::new(
        hub_only(),
        hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600),
    );
    let cat_fetches = Cell::new(0);
    let pack_fetches = Cell::new(0);

    let first = router
        .query_corpus(
            HubQueryRequest::new("lz4", "LZ4_compress_default", AnswerMode::Def, 2, NOW),
            || {
                cat_fetches.set(cat_fetches.get() + 1);
                Ok(cat.clone())
            },
            |_| {
                pack_fetches.set(pack_fetches.get() + 1);
                Ok(pack.clone())
            },
        )
        .unwrap();
    assert_eq!(first.cache_outcome, CacheLookupOutcome::FetchedAndCached);
    assert_eq!(first.answer.answered_from, AnsweredFrom::Hosted);
    assert_eq!(first.answer.hits.len(), 1);
    assert_eq!(first.answer.hits[0].value.as_deref(), Some("142"));
    assert!(
        first.rendered.contains("lib/lz4.h:142"),
        "{}",
        first.rendered
    );
    assert_eq!((cat_fetches.get(), pack_fetches.get()), (1, 1));
    assert_eq!(router.cache().network_fetches(), 2);

    // The archive and its detached signature are on disk as the hub serves them.
    let archive_name = format!("pack-lz4-{}.hubpack.tar.zst", &pin(1)[..8]);
    assert_eq!(
        std::fs::read(dir.path().join(&archive_name)).unwrap(),
        archive
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(format!("{archive_name}.sig"))).unwrap(),
        pack.signature_hex
    );

    let second = router
        .query_corpus(
            HubQueryRequest::new("lz4", "compress_default", AnswerMode::Def, 2, NOW + 10),
            || panic!("the catalogue must not be re-fetched"),
            |_| panic!("the pack must not be re-fetched"),
        )
        .unwrap();
    assert_eq!(second.cache_outcome, CacheLookupOutcome::HitFromDisk);
    assert_eq!(second.answer.answered_from, AnsweredFrom::LocalPack);
    assert_eq!(second.answer.hits.len(), 1);
    assert!(second.rendered.contains("answered from: local pack"));
    assert_eq!(router.cache().disk_hits(), 2);
    assert_eq!(router.cache().network_fetches(), 2);
}

#[test]
fn the_disk_cache_persists_across_reopen_without_refetching() {
    let hub = TestHub::new(1);
    let spec = Spec::new("sqlite", &pin(2));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let cat = hub.catalogue_download(std::slice::from_ref(&entry));
    let dir = tempfile::tempdir().unwrap();
    {
        let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);
        cache
            .get_or_fetch_catalogue(NOW, || Ok(cat.clone()))
            .unwrap();
        cache
            .get_or_fetch_pack(&entry, NOW, |_| Ok(hub.download(&archive)))
            .unwrap();
    }
    let mut reopened = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);
    assert!(reopened.contains_corpus("sqlite"));
    let c = reopened
        .get_or_fetch_catalogue(NOW + 100, || panic!("read from disk"))
        .unwrap();
    assert_eq!(c.outcome, CacheLookupOutcome::HitFromDisk);
    let p = reopened
        .get_or_fetch_pack(&entry, NOW + 100, |_| panic!("read from disk"))
        .unwrap();
    assert_eq!(p.outcome, CacheLookupOutcome::HitFromDisk);
    assert_eq!(p.value.defs.len(), 1);
    assert_eq!((reopened.disk_hits(), reopened.network_fetches()), (2, 0));
}

#[test]
fn ed25519_verification_accepts_the_hub_signature_form_and_nothing_else() {
    let hub = TestHub::new(3);
    let other = TestHub::new(4);
    let msg = b"archive bytes";
    let sig = hub.sign(msg);
    assert_eq!(sig.len(), 128);
    assert!(verify_ed25519_hex(msg, &sig, &hub.pubkey_hex).is_ok());
    assert!(verify_ed25519_hex(msg, &format!("{sig}\n"), &hub.pubkey_hex).is_ok());
    assert!(verify_ed25519_hex(b"changed", &sig, &hub.pubkey_hex).is_err());
    assert!(verify_ed25519_hex(msg, &sig, &other.pubkey_hex).is_err());
    assert!(verify_ed25519_hex(msg, "00", &hub.pubkey_hex)
        .unwrap_err()
        .contains("128 hex"));
    assert!(verify_ed25519_hex(msg, &sig, "zz")
        .unwrap_err()
        .contains("64 hex"));
    assert!(verify_ed25519_hex(msg, &"g".repeat(128), &hub.pubkey_hex)
        .unwrap_err()
        .contains("not hex"));
    assert!(verify_ed25519_hex(msg, &sig, &"é".repeat(32)).is_err());
}

#[test]
fn a_pack_signed_by_another_key_is_refused_and_nothing_is_cached() {
    let hub = TestHub::new(1);
    let rogue = TestHub::new(2);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);

    let err = cache
        .get_or_fetch_pack(&entry, NOW, |_| Ok(rogue.download(&archive)))
        .unwrap_err();
    assert!(
        matches!(&err, HubModeError::SignatureInvalid { subject, .. } if subject == "lz4"),
        "{err}"
    );
    assert!(!cache.contains_corpus("lz4"));
    assert!(pack_files(dir.path()).is_empty());

    // A catalogue signed by the wrong key is refused the same way.
    let rogue_cat = rogue.catalogue_download(&[entry]);
    let err = cache
        .get_or_fetch_catalogue(NOW, || Ok(rogue_cat))
        .unwrap_err();
    assert!(
        matches!(err, HubModeError::SignatureInvalid { .. }),
        "{err}"
    );
    assert!(!dir.path().join(CACHE_CATALOGUE_FILE).exists());
}

#[test]
fn tampered_download_bytes_fail_the_digest_before_the_signature() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);
    let err = cache
        .get_or_fetch_pack(&entry, NOW, |_| Ok(hub.download(b"tampered-bytes")))
        .unwrap_err();
    assert!(
        matches!(&err, HubModeError::IntegrityMismatch { slug, .. } if slug == "lz4"),
        "{err}"
    );
    assert!(pack_files(dir.path()).is_empty());
}

#[test]
fn a_stale_catalogue_is_refetched_so_a_pin_rotation_is_picked_up() {
    let hub = TestHub::new(1);
    let v1 = Spec::new("lz4", &pin(1));
    let v2 = Spec::new("lz4", &pin(2));
    let (a1, a2) = (v1.archive(), v2.archive());
    let (e1, e2) = (entry_for(&v1, &a1), entry_for(&v2, &a2));
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);

    // The reviewer's probe: cached at t=0 with ttl 60, read at t=10_000_000.
    cache
        .get_or_fetch_catalogue(0, || Ok(hub.catalogue_download(std::slice::from_ref(&e1))))
        .unwrap();
    let fresh = cache
        .get_or_fetch_catalogue(59, || panic!("still fresh at age 59"))
        .unwrap();
    assert_eq!(fresh.outcome, CacheLookupOutcome::HitFromDisk);
    assert!(!fresh.freshness.is_stale());

    let calls = Cell::new(0);
    let rotated = cache
        .get_or_fetch_catalogue(10_000_000, || {
            calls.set(calls.get() + 1);
            Ok(hub.catalogue_download(std::slice::from_ref(&e2)))
        })
        .unwrap();
    assert_eq!(calls.get(), 1, "a stale catalogue must call fetch");
    assert_eq!(rotated.outcome, CacheLookupOutcome::FetchedAndCached);
    assert_eq!(rotated.value.corpora[0].pin_sha, pin(2));
    assert!(!rotated.freshness.is_stale());

    // The boundary is `now >= fetched_at + ttl`.
    assert!(Freshness::evaluate(100, 160, 60).is_stale());
    assert!(!Freshness::evaluate(100, 159, 60).is_stale());
}

#[test]
fn a_stale_catalogue_is_served_with_a_warning_only_when_the_fetch_fails() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let entry = entry_for(&spec, &spec.archive());
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);
    cache
        .get_or_fetch_catalogue(0, || Ok(hub.catalogue_download(&[entry])))
        .unwrap();

    let served = cache
        .get_or_fetch_catalogue(600, || Err(fetch_failure()))
        .unwrap();
    assert_eq!(served.outcome, CacheLookupOutcome::HitFromDisk);
    assert!(served.freshness.is_stale());
    assert!(served
        .refresh_error
        .as_deref()
        .unwrap()
        .contains("connection refused"));
    let lines = served.warnings("catalogue");
    assert!(
        lines.len() == 1
            && lines[0].contains("stale (age 600s, ttl 60s)")
            && lines[0].contains("refresh failed"),
        "{lines:?}"
    );
}

#[test]
fn a_damaged_catalogue_or_index_repairs_itself_instead_of_failing_every_call() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);
    let cat = hub.catalogue_download(std::slice::from_ref(&entry));
    cache
        .get_or_fetch_catalogue(NOW, || Ok(cat.clone()))
        .unwrap();
    cache
        .get_or_fetch_pack(&entry, NOW, |_| Ok(hub.download(&archive)))
        .unwrap();

    // A corrupt catalogue.json: fetch repairs it, and says it did.
    std::fs::write(dir.path().join(CACHE_CATALOGUE_FILE), b"garbage").unwrap();
    let fixed = cache
        .get_or_fetch_catalogue(NOW + 1, || Ok(cat.clone()))
        .unwrap();
    assert_eq!(fixed.outcome, CacheLookupOutcome::FetchedAndCached);
    assert!(fixed
        .repaired
        .as_deref()
        .unwrap()
        .contains("sha256 mismatch"));
    assert!(fixed.warnings("catalogue")[0].contains("failed verification"));
    // With the fetch failing too there is nothing valid to serve: an error.
    std::fs::write(dir.path().join(CACHE_CATALOGUE_FILE), b"garbage").unwrap();
    assert!(matches!(
        cache.get_or_fetch_catalogue(NOW + 2, || Err(fetch_failure())),
        Err(HubModeError::HubFetch { .. })
    ));

    // A corrupt index.json is rebuilt; the pack it orphaned is swept.
    std::fs::write(dir.path().join(CACHE_INDEX_FILE), b"{not json").unwrap();
    let again = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);
    assert_eq!(again.entry_count(), 0);
    let mut again = again;
    let rebuilt = again
        .get_or_fetch_pack(&entry, NOW + 3, |_| Ok(hub.download(&archive)))
        .unwrap();
    assert_eq!(rebuilt.outcome, CacheLookupOutcome::FetchedAndCached);
    assert_eq!(again.entry_count(), 1);
    assert_eq!(pack_files(dir.path()).len(), 2, "one archive and its .sig");
}

#[test]
fn an_index_from_an_older_layout_is_rebuilt_and_a_newer_one_is_refused() {
    let hub = TestHub::new(1);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(CACHE_INDEX_FILE),
        r#"{"format_version":1,"access_clock":3,"catalogue_fetched_at_unix":null,"catalogue_sha256":null,"entries":{}}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("pack-old.json"), b"{}").unwrap();
    let cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);
    assert_eq!(cache.entry_count(), 0);

    let newer = tempfile::tempdir().unwrap();
    std::fs::write(
        newer.path().join(CACHE_INDEX_FILE),
        r#"{"format_version":9}"#,
    )
    .unwrap();
    let err = HubDiskCache::open(newer.path(), 1, 1).unwrap_err();
    assert!(matches!(
        err,
        HubModeError::UnsupportedFormatVersion {
            kind: "disk_cache_index",
            found: 9,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(newer.path().join(CACHE_INDEX_FILE)).unwrap(),
        r#"{"format_version":9}"#,
        "an index a newer build wrote is not ours to discard"
    );
}

#[test]
fn a_stale_pack_is_refetched_and_falls_back_only_when_the_fetch_fails() {
    let hub = TestHub::new(1);
    let spec = Spec::new("zstd", &pin(3));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);
    cache
        .get_or_fetch_pack(&entry, 1_000, |_| Ok(hub.download(&archive)))
        .unwrap();

    let refetched = Cell::new(0);
    let r = cache
        .get_or_fetch_pack(&entry, 1_120, |_| {
            refetched.set(refetched.get() + 1);
            Ok(hub.download(&archive))
        })
        .unwrap();
    assert_eq!(refetched.get(), 1, "a stale pack must call fetch");
    assert_eq!(r.outcome, CacheLookupOutcome::FetchedAndCached);
    assert_eq!(cache.entry_meta("zstd").unwrap().fetched_at_unix, 1_120);
    assert_eq!(
        pack_files(dir.path()).len(),
        2,
        "same pin: files replaced, not duplicated"
    );

    let s = cache
        .get_or_fetch_pack(&entry, 1_300, |_| Err(fetch_failure()))
        .unwrap();
    assert_eq!(s.outcome, CacheLookupOutcome::HitFromDisk);
    assert_eq!(
        s.freshness,
        Freshness::Stale {
            age_secs: 180,
            max_age_secs: 60
        }
    );
    assert!(s
        .refresh_error
        .as_deref()
        .unwrap()
        .contains("connection refused"));
    assert!(s.warnings("corpus 'zstd'")[0].contains("refresh failed"));
}

#[test]
fn a_damaged_pack_is_repaired_by_a_fetch_and_is_an_error_when_the_fetch_fails() {
    let hub = TestHub::new(1);
    let spec = Spec::new("brotli", &pin(4));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);
    cache
        .get_or_fetch_pack(&entry, 1_000, |_| Ok(hub.download(&archive)))
        .unwrap();
    let on_disk = dir
        .path()
        .join(format!("pack-brotli-{}.hubpack.tar.zst", &pin(4)[..8]));

    // Garbage on disk: before this change a hard error on every call.
    std::fs::write(&on_disk, b"corrupted").unwrap();
    let fixed = cache
        .get_or_fetch_pack(&entry, 1_005, |_| Ok(hub.download(&archive)))
        .unwrap();
    assert_eq!(fixed.outcome, CacheLookupOutcome::FetchedAndCached);
    assert!(fixed
        .repaired
        .as_deref()
        .unwrap()
        .contains("sha256 mismatch"));
    assert_eq!(std::fs::read(&on_disk).unwrap(), archive);

    // A deleted file is the same case.
    std::fs::remove_file(&on_disk).unwrap();
    let again = cache
        .get_or_fetch_pack(&entry, 1_006, |_| Ok(hub.download(&archive)))
        .unwrap();
    assert!(again.repaired.as_deref().unwrap().contains("cannot read"));

    // Damaged and no way to re-fetch: the fetch error, not a stale answer.
    std::fs::write(&on_disk, b"corrupted").unwrap();
    assert!(matches!(
        cache.get_or_fetch_pack(&entry, 1_007, |_| Err(fetch_failure())),
        Err(HubModeError::HubFetch { .. })
    ));

    // An intact archive whose signature file was swapped for another key's.
    let rogue = TestHub::new(9);
    std::fs::write(&on_disk, &archive).unwrap();
    std::fs::write(format!("{}.sig", on_disk.display()), rogue.sign(&archive)).unwrap();
    let resigned = cache
        .get_or_fetch_pack(&entry, 1_008, |_| Ok(hub.download(&archive)))
        .unwrap();
    assert!(resigned
        .repaired
        .as_deref()
        .unwrap()
        .contains("signature does not verify"));
}

#[test]
fn bytes_that_fail_verification_never_fall_back_to_a_stale_entry() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60);
    cache
        .get_or_fetch_pack(&entry, 0, |_| Ok(hub.download(&archive)))
        .unwrap();
    let err = cache
        .get_or_fetch_pack(&entry, 10_000, |_| Ok(hub.download(b"not the pack")))
        .unwrap_err();
    assert!(
        matches!(err, HubModeError::IntegrityMismatch { .. }),
        "{err}"
    );
    assert!(cache.contains_corpus("lz4"), "the verified entry is kept");
}

#[test]
fn the_cache_lock_serialises_processes_and_times_out_loudly() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub
        .cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 60)
        .with_lock_wait(Duration::from_millis(150));

    let held = FileLock::acquire(&cache.lock_path(), Duration::ZERO).unwrap();
    let err = cache
        .get_or_fetch_pack(&entry, NOW, |_| panic!("blocked before any fetch"))
        .unwrap_err();
    match &err {
        HubModeError::CacheLock { path, reason } => {
            assert!(path.ends_with(CACHE_LOCK_FILE), "{path}");
            assert!(reason.contains("held by pid"), "{reason}");
        }
        other => panic!("{other}"),
    }
    drop(held);
    cache
        .get_or_fetch_pack(&entry, NOW, |_| Ok(hub.download(&archive)))
        .unwrap();
    assert!(dir.path().join(CACHE_LOCK_FILE).is_file());
}

/// Each thread is a separate `xerj mcp` session: its own handle on the one
/// directory. Without `cache.lock` the index read-modify-write loses entries and
/// one session's eviction deletes a pack another just indexed.
#[test]
fn sessions_sharing_one_cache_directory_never_lose_or_orphan_an_entry() {
    let hub = TestHub::new(1);
    let dir = tempfile::tempdir().unwrap();
    let specs: Vec<Spec> = (0..8u8)
        .map(|i| {
            let mut s = Spec::new(&format!("corp-{i}"), &pin(i));
            s.snapshot_bytes = 4096;
            s
        })
        .collect();
    let archives: Vec<Vec<u8>> = specs.iter().map(Spec::archive).collect();
    let entries: Vec<CatalogueEntry> = specs
        .iter()
        .zip(&archives)
        .map(|(s, a)| entry_for(s, a))
        .collect();
    let largest = archives.iter().map(Vec::len).max().unwrap() as u64 + 140;
    let budget = largest * 3;
    assert!(budget < archives.iter().map(|a| a.len() as u64 + 128).min().unwrap() * 4);

    std::thread::scope(|scope| {
        for (entry, archive) in entries.iter().zip(&archives) {
            let (hub, dir) = (&hub, dir.path());
            scope.spawn(move || {
                let mut cache = hub.cache(dir, budget, 3600);
                for round in 0..3 {
                    cache
                        .get_or_fetch_pack(entry, NOW + round, |_| Ok(hub.download(archive)))
                        .unwrap();
                }
            });
        }
    });

    let cache = hub.cache(dir.path(), budget, 3600);
    assert_eq!(cache.entry_count(), 3, "the budget holds three packs");
    assert!(cache.used_bytes() <= budget);
    let index = load_index(dir.path()).unwrap().0;
    let mut referenced = Vec::new();
    for meta in index.entries.values() {
        let bytes = std::fs::read(dir.path().join(&meta.file_name)).unwrap();
        assert_eq!(sha256_hex(&bytes), meta.sha256, "{}", meta.slug);
        assert!(dir.path().join(&meta.sig_file_name).is_file());
        referenced.push(meta.file_name.clone());
        referenced.push(meta.sig_file_name.clone());
    }
    referenced.sort();
    assert_eq!(
        pack_files(dir.path()),
        referenced,
        "no orphan, no missing file"
    );
}

#[test]
fn lru_evicts_the_least_recently_used_pack_and_its_files() {
    let hub = TestHub::new(1);
    let mut specs: Vec<Spec> = ["corp-a", "corp-b", "corp-c"]
        .iter()
        .enumerate()
        .map(|(i, s)| Spec::new(s, &pin(i as u8 + 1)))
        .collect();
    for s in &mut specs {
        s.snapshot_bytes = 2048;
    }
    let archives: Vec<Vec<u8>> = specs.iter().map(Spec::archive).collect();
    let entries: Vec<CatalogueEntry> = specs
        .iter()
        .zip(&archives)
        .map(|(s, a)| entry_for(s, a))
        .collect();
    let sig_len = hub.sign(&archives[0]).len() as u64;
    let size = |i: usize| archives[i].len() as u64 + sig_len;
    let budget = size(0) + size(1) + 32;
    assert!(size(0) + size(1) + size(2) > budget);

    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), budget, 3600);
    for (i, now) in [(0usize, 1000u64), (1, 1001)] {
        cache
            .get_or_fetch_pack(&entries[i], now, |_| Ok(hub.download(&archives[i])))
            .unwrap();
    }
    assert_eq!(cache.entry_count(), 2);
    let touched = cache
        .get_or_fetch_pack(&entries[0], 1002, |_| panic!("corp-a must hit disk"))
        .unwrap();
    assert_eq!(touched.outcome, CacheLookupOutcome::HitFromDisk);

    let inserted = cache
        .get_or_fetch_pack(&entries[2], 1003, |_| Ok(hub.download(&archives[2])))
        .unwrap();
    assert_eq!(inserted.evicted_slugs, vec!["corp-b".to_string()]);
    assert!(cache.contains_corpus("corp-a") && cache.contains_corpus("corp-c"));
    assert!(!cache.contains_corpus("corp-b"));
    assert!(!pack_files(dir.path()).iter().any(|n| n.contains("corp-b")));
    assert!(cache.used_bytes() <= budget);
    assert_eq!(cache.evictions(), 1);
}

#[test]
fn a_pack_over_the_whole_budget_is_refused_before_it_is_downloaded() {
    let hub = TestHub::new(1);
    let small = Spec::new("small", &pin(1));
    let mut huge = Spec::new("huge", &pin(2));
    huge.snapshot_bytes = 8192;
    let (small_archive, huge_archive) = (small.archive(), huge.archive());
    let (small_entry, huge_entry) = (
        entry_for(&small, &small_archive),
        entry_for(&huge, &huge_archive),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), small_archive.len() as u64 + 300, 3600);
    cache
        .get_or_fetch_pack(&small_entry, NOW, |_| Ok(hub.download(&small_archive)))
        .unwrap();

    let err = cache
        .get_or_fetch_pack(&huge_entry, NOW, |_| panic!("must not download it"))
        .unwrap_err();
    assert!(
        matches!(err, HubModeError::EntryExceedsCacheBudget { ref slug, .. } if slug == "huge")
    );

    // A catalogue that understates the size is caught after the download.
    let mut liar = huge_entry.clone();
    liar.pack_bytes = 1;
    let err = cache
        .get_or_fetch_pack(&liar, NOW, |_| Ok(hub.download(&huge_archive)))
        .unwrap_err();
    assert!(
        matches!(err, HubModeError::EntryExceedsCacheBudget { ref slug, .. } if slug == "huge")
    );
    assert!(
        cache.contains_corpus("small"),
        "no eviction for a refused entry"
    );
}

#[test]
fn a_pin_rotation_replaces_the_cached_pack_and_removes_its_files() {
    let hub = TestHub::new(1);
    let v1 = Spec::new("snappy", &pin(8));
    let mut v2 = Spec::new("snappy", &pin(9));
    v2.snapshot_bytes = 256;
    let (a1, a2) = (v1.archive(), v2.archive());
    let (e1, e2) = (entry_for(&v1, &a1), entry_for(&v2, &a2));
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);

    cache
        .get_or_fetch_pack(&e1, 1_000, |_| Ok(hub.download(&a1)))
        .unwrap();
    assert_eq!(cache.entry_meta("snappy").unwrap().pin_sha, pin(8));
    let r2 = cache
        .get_or_fetch_pack(&e2, 2_000, |_| Ok(hub.download(&a2)))
        .unwrap();
    assert_eq!(r2.outcome, CacheLookupOutcome::FetchedAndCached);
    assert_eq!(cache.entry_meta("snappy").unwrap().pin_sha, pin(9));
    assert_eq!(cache.entry_count(), 1);
    assert_eq!(
        pack_files(dir.path()),
        vec![
            format!("pack-snappy-{}.hubpack.tar.zst", &pin(9)[..8]),
            format!("pack-snappy-{}.hubpack.tar.zst.sig", &pin(9)[..8]),
        ]
    );
}

#[test]
fn max_hits_caps_the_output_and_marks_it_truncated() {
    let hub = TestHub::new(1);
    let mut spec = Spec::new("big", &pin(1));
    spec.defs = (0..120)
        .map(|i| def(&format!("sym_{i:03}"), "src/a.c", i + 1, None))
        .collect();
    spec.defs.push(def("sym", "src/exact.c", 9, Some("9")));
    spec.files = (0..70)
        .map(|i| file(&format!("src/f{i:02}.c"), "MIT"))
        .collect();
    let archive = spec.archive();
    let cat = hub.catalogue_download(&[entry_for(&spec, &archive)]);
    let dir = tempfile::tempdir().unwrap();
    let mut router = HubFirstRouter::new(
        hub_only(),
        hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600),
    );
    let mut ask = |request: HubQueryRequest<'_>| {
        router
            .query_corpus(request, || Ok(cat.clone()), |_| Ok(hub.download(&archive)))
            .unwrap()
    };

    // `sym` matches all 121 definitions; the default cap is 50 and the exact
    // match ranks first.
    let r = ask(HubQueryRequest::new("big", "sym", AnswerMode::Def, 1, NOW));
    assert_eq!(r.answer.hits.len(), DEFAULT_MAX_HITS);
    assert!(r.answer.truncated);
    assert_eq!(r.answer.hits[0].file, "src/exact.c");
    assert!(r
        .answer
        .warnings
        .iter()
        .any(|w| w.contains("truncated to the best 50 of 121 matches")));

    let r = ask(HubQueryRequest::new("big", "sym", AnswerMode::Def, 1, NOW).with_max_hits(3));
    assert_eq!(r.answer.hits.len(), 3);
    assert!(r.answer.truncated);

    let r = ask(HubQueryRequest::new(
        "big",
        "sym_01",
        AnswerMode::Def,
        1,
        NOW,
    ));
    assert_eq!(r.answer.hits.len(), 10);
    assert!(!r.answer.truncated);
    assert!(r.answer.warnings.iter().all(|w| !w.contains("truncated")));

    // Files returned every file before; it is capped too.
    let r = ask(HubQueryRequest::new("big", "", AnswerMode::Files, 1, NOW));
    assert_eq!(r.answer.hits.len(), 50);
    assert!(r.answer.truncated);
    let r = ask(HubQueryRequest::new("big", "", AnswerMode::Files, 1, NOW).with_max_hits(0));
    assert_eq!(r.answer.hits.len(), 1, "a cap of zero still answers");
}

#[test]
fn an_empty_definition_query_is_refused_before_any_io() {
    let hub = TestHub::new(1);
    let dir = tempfile::tempdir().unwrap();
    let mut router = HubFirstRouter::new(
        hub_only(),
        hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600),
    );
    for query in ["", "   ", "\t\n"] {
        let err = router
            .query_corpus(
                HubQueryRequest::new("lz4", query, AnswerMode::Def, 1, NOW),
                || panic!("must not fetch the catalogue"),
                |_| panic!("must not fetch the pack"),
            )
            .unwrap_err();
        assert!(matches!(err, HubModeError::EmptyQuery { .. }), "{query:?}");
    }
    assert!(!dir.path().join(CACHE_CATALOGUE_FILE).exists());
}

#[test]
fn passage_questions_are_refused_loudly_because_a_pack_archive_holds_no_passages() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut router = HubFirstRouter::new(
        hub_only(),
        hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600),
    );
    let err = router
        .query_corpus(
            HubQueryRequest::new(
                "lz4",
                "how does block compression work",
                AnswerMode::How,
                1,
                NOW,
            ),
            || panic!("no network for a question it cannot answer"),
            |_| panic!("no network for a question it cannot answer"),
        )
        .unwrap_err();
    assert!(
        matches!(&err, HubModeError::PassageSearchNeedsNode { slug } if slug == "lz4"),
        "{err}"
    );
    let pack = decode_pack_archive("lz4", &archive).unwrap();
    assert!(matches!(
        answer_from_pack(&pack, &entry, &AnswerQuery::new("q", AnswerMode::How)),
        Err(HubModeError::PassageSearchNeedsNode { .. })
    ));
}

#[test]
fn files_mode_lists_the_per_file_licence_and_filters_by_path() {
    let hub = TestHub::new(1);
    let mut spec = Spec::new("lz4", &pin(1));
    spec.files = vec![
        file("lib/lz4.c", "BSD-2-Clause"),
        file("programs/lz4cli.c", "GPL-2.0-or-later"),
        file("README.md", "BSD-2-Clause"),
    ];
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let pack = decode_pack_archive("lz4", &archive).unwrap();
    let all = answer_from_pack(&pack, &entry, &AnswerQuery::new("", AnswerMode::Files)).unwrap();
    assert_eq!(all.hits.len(), 3);
    let cli = answer_from_pack(&pack, &entry, &AnswerQuery::new("CLI", AnswerMode::Files)).unwrap();
    assert_eq!(cli.hits.len(), 1);
    assert_eq!(cli.hits[0].file, "programs/lz4cli.c");
    assert_eq!(cli.hits[0].kind, "file");
    assert_eq!(cli.hits[0].text, "licence: GPL-2.0-or-later");
    drop(hub);
}

#[test]
fn unknown_corpus_and_invalid_slugs_fail_loudly() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let entry = entry_for(&spec, &spec.archive());
    let dir = tempfile::tempdir().unwrap();
    let mut router = HubFirstRouter::new(
        hub_only(),
        hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600),
    );

    let err = router
        .query_corpus(
            HubQueryRequest::new("nonexistent-corpus", "anything", AnswerMode::Def, 1, NOW),
            || Ok(hub.catalogue_download(&[entry])),
            |_| unreachable!("no pack fetch for an unknown corpus"),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        HubModeError::UnknownCorpus { ref slug, .. } if slug == "nonexistent-corpus"
    ));

    let overlong = "a".repeat(MAX_SLUG_BYTES + 1);
    for bad in [
        "",
        "../etc/passwd",
        "UPPERCASE",
        "-leading-dash",
        "a/b",
        overlong.as_str(),
    ] {
        let err = router
            .query_corpus(
                HubQueryRequest::new(bad, "q", AnswerMode::Def, 1, NOW),
                || panic!("no catalogue fetch for an invalid slug"),
                |_| panic!("no pack fetch for an invalid slug"),
            )
            .unwrap_err();
        assert!(
            matches!(err, HubModeError::InvalidCorpusSlug { .. }),
            "{bad}"
        );
    }
}

#[test]
fn unsupported_catalogue_pack_and_index_formats_are_refused_loudly() {
    let hub = TestHub::new(1);
    let spec = Spec::new("lz4", &pin(1));
    let archive = spec.archive();
    let entry = entry_for(&spec, &archive);
    let dir = tempfile::tempdir().unwrap();
    let mut cache = hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600);

    // A future catalogue shape, with fields this build does not know.
    let mut future = catalogue(vec![entry.clone()]);
    future.format_version = CATALOGUE_FORMAT_VERSION + 1;
    let mut value = serde_json::to_value(&future).unwrap();
    value["a_new_field"] = serde_json::json!(true);
    let err = cache
        .put_catalogue(&hub.download(&serde_json::to_vec(&value).unwrap()), NOW)
        .unwrap_err();
    assert!(matches!(
        err,
        HubModeError::UnsupportedFormatVersion { kind: "catalogue", found, .. }
            if found == CATALOGUE_FORMAT_VERSION + 1
    ));
    assert!(!dir.path().join(CACHE_CATALOGUE_FILE).exists());

    // A future pack manifest inside an otherwise valid, signed archive.
    let mut bad = manifest("lz4", &pin(1), LicenceClass::AdaptWithAttribution);
    bad.format_version = PACK_FORMAT_VERSION + 1;
    let pack_json = serde_json::to_vec(&bad).unwrap();
    let sums = format!(
        "{}  pack.json\n{}  defs.jsonl\n{}  files.jsonl\n",
        sha256_hex(&pack_json),
        sha256_hex(b""),
        sha256_hex(b"")
    );
    let future_archive = raw_archive(&[
        ("SHA256SUMS", sums.as_bytes()),
        ("pack.json", &pack_json),
        ("defs.jsonl", b""),
        ("files.jsonl", b""),
    ]);
    let future_entry = entry_for(&spec, &future_archive);
    let err = cache
        .get_or_fetch_pack(&future_entry, NOW, |_| Ok(hub.download(&future_archive)))
        .unwrap_err();
    assert!(matches!(
        err,
        HubModeError::UnsupportedFormatVersion { kind: "pack_manifest", found, .. }
            if found == PACK_FORMAT_VERSION + 1
    ));
}

#[test]
fn excluded_corpora_are_never_fetched_and_approach_only_hits_carry_a_warning() {
    let hub = TestHub::new(1);
    let mut gpl = Spec::new("gpl-lib", &pin(1));
    gpl.class = LicenceClass::ApproachOnly;
    let mut closed = Spec::new("proprietary", &pin(2));
    closed.class = LicenceClass::Excluded;
    let (gpl_archive, closed_archive) = (gpl.archive(), closed.archive());
    let cat = hub.catalogue_download(&[
        entry_for(&gpl, &gpl_archive),
        entry_for(&closed, &closed_archive),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut router = HubFirstRouter::new(
        hub_only(),
        hub.cache(dir.path(), DEFAULT_MCP_CACHE_MAX_BYTES, 3600),
    );

    let err = router
        .query_corpus(
            HubQueryRequest::new(
                "proprietary",
                "LZ4_compress_default",
                AnswerMode::Def,
                1,
                NOW,
            ),
            || Ok(cat.clone()),
            |_| panic!("an excluded pack must never be fetched"),
        )
        .unwrap_err();
    assert!(
        matches!(err, HubModeError::ExcludedCorpusLicence { ref slug, .. } if slug == "proprietary")
    );

    let ok = router
        .query_corpus(
            HubQueryRequest::new("gpl-lib", "LZ4_compress_default", AnswerMode::Def, 1, NOW),
            || unreachable!("catalogue already cached"),
            |_| Ok(hub.download(&gpl_archive)),
        )
        .unwrap();
    assert!(ok.answer.hits[0]
        .licence_warning
        .as_deref()
        .unwrap()
        .contains("approach-only licence"));
}

#[test]
fn the_archive_reader_is_bounded_and_checks_sha256sums() {
    let spec = Spec::new("lz4", &pin(1));
    let mut big = Spec::new("lz4", &pin(1));
    big.snapshot_bytes = 4096;
    let archive = spec.archive();
    let bad = |a: &[u8], max_unpacked: u64, max_members: usize| {
        decode_pack_archive_bounded("lz4", a, max_unpacked, max_members).unwrap_err()
    };

    assert!(decode_pack_archive_bounded("lz4", &archive, MAX_UNPACKED_BYTES, MAX_MEMBERS).is_ok());
    // Member and decompressed-size caps hold however well the archive compresses.
    let err = bad(&archive, MAX_UNPACKED_BYTES, 3);
    assert!(err.to_string().contains("more than 3 entries"), "{err}");
    let err = bad(&big.archive(), 1024, MAX_MEMBERS);
    assert!(err.to_string().contains("1024 bytes"), "{err}");
    let zeros = raw_archive(&[("snapshot/zeros.bin", &vec![0u8; 1 << 20])]);
    assert!(zeros.len() < 4096, "a megabyte of zeros is a tiny archive");
    let err = bad(&zeros, 4096, MAX_MEMBERS);
    assert!(err.to_string().contains("4096 bytes"), "{err}");

    // Not zstd, not tar, no SHA256SUMS.
    assert!(matches!(
        bad(b"plain text", MAX_UNPACKED_BYTES, MAX_MEMBERS),
        HubModeError::InvalidPack { .. }
    ));
    let no_sums = raw_archive(&[("pack.json", b"{}")]);
    assert!(bad(&no_sums, MAX_UNPACKED_BYTES, MAX_MEMBERS)
        .to_string()
        .contains("SHA256SUMS missing"));

    // A member that does not match SHA256SUMS, one SHA256SUMS does not cover.
    let pack_json = serde_json::to_vec(&manifest(
        "lz4",
        &pin(1),
        LicenceClass::AdaptWithAttribution,
    ))
    .unwrap();
    let sums = |defs_hash: &str, with_files: bool| {
        let mut s = format!(
            "{}  pack.json\n{defs_hash}  defs.jsonl\n",
            sha256_hex(&pack_json)
        );
        if with_files {
            s.push_str(&format!("{}  files.jsonl\n", sha256_hex(b"")));
        }
        s
    };
    let wrong = sums(&sha256_hex(b"other"), true);
    let a = raw_archive(&[
        ("SHA256SUMS", wrong.as_bytes()),
        ("pack.json", &pack_json),
        ("defs.jsonl", b""),
        ("files.jsonl", b""),
    ]);
    assert!(bad(&a, MAX_UNPACKED_BYTES, MAX_MEMBERS)
        .to_string()
        .contains("checksum mismatch for defs.jsonl"));
    let uncovered = sums(&sha256_hex(b""), false);
    let a = raw_archive(&[
        ("SHA256SUMS", uncovered.as_bytes()),
        ("pack.json", &pack_json),
        ("defs.jsonl", b""),
        ("files.jsonl", b""),
    ]);
    assert!(bad(&a, MAX_UNPACKED_BYTES, MAX_MEMBERS)
        .to_string()
        .contains("files.jsonl is not covered by SHA256SUMS"));

    // Paths are never trusted.
    assert_eq!(
        sanitize_archive_path(Path::new("snapshot/./a/b")).unwrap(),
        "snapshot/a/b"
    );
    assert!(sanitize_archive_path(Path::new("../x")).is_err());
    assert!(sanitize_archive_path(Path::new("/abs")).is_err());
}
