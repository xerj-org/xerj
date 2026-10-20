//! Route-level tests for the hosted Corpus Hub endpoints (plan §6) —
//! `GET /_hub/v1/{catalogue, def/{slug}/{symbol}, code/{slug}, search}` over the
//! real ES-compat router with a real engine:
//!
//! * the four answers: `def` returns the fixture value with its pin, `code`
//!   returns a passage with `file:line`, `search` groups by corpus, the
//!   catalogue is the file's bytes with a sha256 `ETag` and a `304`;
//! * the validation lines: unknown corpus is `404`, `k=1000` is `50`, a
//!   600-byte `q` is `400`, a stray parameter is `400`;
//! * `hub.public`: `false` needs the key; `true` lets EXACTLY the four routes
//!   through without one (near-miss paths and every other route stay `401`),
//!   a `POST` to them is `405` and leaves no audit entry;
//! * the rate limit: the 61st request in a minute from one address is `429`
//!   with `Retry-After`, keyed by the real peer (not by a header the caller
//!   writes), with a real socket on an ephemeral port as well as `oneshot`;
//! * the guards behind the routes: an alias cannot lead a hosted query out of
//!   the `xc-*` namespace, and a backend failure never reaches the client.
//!
//! The ledger root is process-global (`XERJ_CODE_HOME`, read by
//! `xerj_hub::def::lookup` the way `xerj hub pull` writes it), so every test
//! shares ONE fixture directory, created and exported once before any node is
//! built; each test then builds its own node and seeds its own indices.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, Request, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use xerj_api::hub_api::{self, RateLimiter};
use xerj_api::state::AppState;
use xerj_common::hub::{
    Answer, AnswerMode, AnsweredFrom, Catalogue, CatalogueEntry, Hit, LicenceClass, Sketch,
    CATALOGUE_FORMAT_VERSION,
};
use xerj_common::xccode::state::write_hubpack_state;

const ADMIN_KEY: &str = "hub-test-admin-key";
const PIN_SHA: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";
const PIN_DATE: &str = "2026-10-02";

/// `(slug, ledger index_prefix)`: pack-supplied build prefixes that are NOT
/// `xc-<that slug>-b<digits>`. Every one uses only characters a URL path can
/// carry, so the character-set guard alone does not stop them; each one is a way
/// to aim `<prefix>*` at somebody else's indices.
const HOSTILE_PREFIXES: [(&str, &str); 8] = [
    // `xc-*`: every hub index on the node.
    ("evil-bare", "xc-"),
    ("evil-wild", "xc-*"),
    // Another corpus's real build (the excluded one), and one no entry lists.
    ("evil-other", "xc-mini-closed-b1"),
    ("evil-orphan", "xc-orphan-b1"),
    // A comma list is two index expressions in one.
    ("evil-list", "xc-evil-list-b1,xc-mini-closed-b1"),
    // Not a build prefix: a whole index name, and a name with no build at all.
    ("evil-tail", "xc-evil-tail-b1-defs"),
    ("evil-nobuild", "xc-evil-nobuild"),
    ("evil-empty", ""),
];

/// Every slug whose ledger must be refused, `HOSTILE_PREFIXES` plus the one with
/// no `index_prefix` field.
fn hostile_slugs() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = HOSTILE_PREFIXES.iter().map(|(s, _)| *s).collect();
    v.push("evil-legacy");
    v
}

// ─────────────────────────────────────────────────────────────────────────────
// The shared fixture: ledgers + catalogue, written once
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    catalogue: PathBuf,
    catalogue_bytes: Vec<u8>,
    dir: PathBuf,
}

static FIXTURE: OnceLock<Fixture> = OnceLock::new();

/// Source of fresh client addresses for [`Hub::stranger`].
static NEXT_PEER: AtomicU32 = AtomicU32::new(1);

fn entry(slug: &str, spdx: &str, class: LicenceClass) -> CatalogueEntry {
    CatalogueEntry {
        slug: slug.into(),
        domain: "DEV".into(),
        topics: vec!["c".into()],
        ask_for: format!("fixture corpus {slug}"),
        pin_sha: PIN_SHA.into(),
        pin_date: PIN_DATE.into(),
        refresh_cadence_days: 7,
        licence_spdx: spdx.into(),
        licence_class: class,
        pack_bytes: 1024,
        pack_url: format!("https://example.invalid/{slug}.hubpack.tar.zst"),
        pack_sha256: "0".repeat(64),
        sig_url: format!("https://example.invalid/{slug}.hubpack.tar.zst.sig"),
        dependency_names: vec![],
        quality_grade: None,
        term_sketch: Sketch {
            m_bits: 64,
            k_hashes: 7,
            bits_base64: "AAAAAAAAAAA=".into(),
        },
    }
}

fn catalogue_bytes(entries: Vec<CatalogueEntry>) -> Vec<u8> {
    serde_json::to_vec(&Catalogue {
        format_version: CATALOGUE_FORMAT_VERSION,
        generated_at: "2026-10-14T00:00:00Z".into(),
        corpora: entries,
    })
    .expect("catalogue serialises")
}

/// Create the fixture and export `XERJ_CODE_HOME` — the first thing every test
/// does, before it builds a node or reads anything else from the environment.
fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let home = dir.join("code-home");
        let led = |slug: &str, prefix: &str, spdx: &str, class: &str| {
            write_hubpack_state(
                &home, slug, "http://x", prefix, PIN_SHA, PIN_DATE, spdx, class,
            )
            .expect("ledger");
        };
        led("mini-c", "xc-mini-c-b1", "MIT", "adapt-with-attribution");
        led(
            "mini-gpl",
            "xc-mini-gpl-b2",
            "GPL-3.0-only",
            "approach-only",
        );
        led(
            "mini-closed",
            "xc-mini-closed-b1",
            "LicenseRef-Closed",
            "excluded",
        );
        led("leaky", "xc-leaky-b1", "MIT", "adapt-with-attribution");
        // The catalogue lists it as servable; the ledger written for the pack on
        // THIS node says `excluded`. Both must permit, and either can refuse.
        led(
            "ledger-closed",
            "xc-ledger-closed-b1",
            "LicenseRef-Closed",
            "excluded",
        );
        // A ledger whose prefix could change the meaning of a URL: the backend
        // must refuse it, and the client must never see why.
        led(
            "badprefix",
            "xc-bad prefix?x=1",
            "MIT",
            "adapt-with-attribution",
        );
        // Ledgers whose pack-supplied `index_prefix` is not `xc-<own slug>-b<digits>`.
        // Each is built from characters the URL guard accepts, and each points at
        // indices that belong to somebody else.
        for (slug, prefix) in HOSTILE_PREFIXES {
            led(slug, prefix, "MIT", "adapt-with-attribution");
        }
        // No `index_prefix` at all: `query_prefix` falls back to the ledger's own
        // `prefix`, which here is the bare namespace.
        {
            let state_dir = home.join("state");
            std::fs::create_dir_all(&state_dir).expect("state dir");
            std::fs::write(
                state_dir.join("evil-legacy.json"),
                format!(
                    "{{\"corpus\":\"evil-legacy\",\"indexed_at\":\"2026-10-01T00:00:00Z\",\
                     \"prefix\":\"xc-\",\"url\":\"http://x\",\"autoindex_exit\":0,\
                     \"salvaged\":false,\"source\":\"hubpack\",\"pin_sha\":\"{PIN_SHA}\",\
                     \"pin_date\":\"{PIN_DATE}\",\"licence_spdx\":\"MIT\",\
                     \"licence_class\":\"adapt-with-attribution\"}}\n"
                ),
            )
            .expect("legacy ledger");
        }
        // The two sides of "either can refuse" the other way round, plus both
        // directions of "the stricter class wins".
        led(
            "cat-closed",
            "xc-cat-closed-b1",
            "MIT",
            "adapt-with-attribution",
        );
        led("gplish", "xc-gplish-b1", "MIT", "adapt-with-attribution");
        led(
            "ledger-gpl",
            "xc-ledger-gpl-b1",
            "AGPL-3.0-only",
            "approach-only",
        );
        // Corpora used by the failure-isolation tests: indices exist only on the
        // nodes of the tests that create them.
        led(
            "longbody",
            "xc-longbody-b1",
            "MIT",
            "adapt-with-attribution",
        );
        led("noindex", "xc-noindex-b1", "MIT", "adapt-with-attribution");
        led("big", "xc-big-b1", "MIT", "adapt-with-attribution");
        led(
            "bodyonly",
            "xc-bodyonly-b1",
            "MIT",
            "adapt-with-attribution",
        );
        // Well-formed until `rewrite_flip_ledger` replaces it mid-request.
        led("flip", "xc-flip-b1", "MIT", "adapt-with-attribution");
        // One corpus with an index that cannot be searched next to a good one,
        // and one whose every index cannot.
        led("mixed", "xc-mixed-b1", "MIT", "adapt-with-attribution");
        led("allbad", "xc-allbad-b1", "MIT", "adapt-with-attribution");
        let mut entries = vec![
            entry("mini-c", "MIT", LicenceClass::AdaptWithAttribution),
            entry("mini-gpl", "GPL-3.0-only", LicenceClass::ApproachOnly),
            entry("mini-closed", "LicenseRef-Closed", LicenceClass::Excluded),
            entry("leaky", "MIT", LicenceClass::AdaptWithAttribution),
            entry("ledger-closed", "MIT", LicenceClass::AdaptWithAttribution),
            entry("badprefix", "MIT", LicenceClass::AdaptWithAttribution),
            // Listed, but no ledger and no indices on any test node.
            entry("ghost", "MIT", LicenceClass::AdaptWithAttribution),
            // The catalogue refuses it; this node's ledger would serve it.
            entry("cat-closed", "LicenseRef-Closed", LicenceClass::Excluded),
            // The catalogue says approach-only; the ledger says permissive.
            entry("gplish", "GPL-3.0-only", LicenceClass::ApproachOnly),
            // The catalogue says permissive; the ledger says approach-only.
            entry("ledger-gpl", "MIT", LicenceClass::AdaptWithAttribution),
            entry("longbody", "MIT", LicenceClass::AdaptWithAttribution),
            entry("noindex", "MIT", LicenceClass::AdaptWithAttribution),
            entry("big", "MIT", LicenceClass::AdaptWithAttribution),
            entry("bodyonly", "MIT", LicenceClass::AdaptWithAttribution),
            entry("flip", "MIT", LicenceClass::AdaptWithAttribution),
            entry("mixed", "MIT", LicenceClass::AdaptWithAttribution),
            entry("allbad", "MIT", LicenceClass::AdaptWithAttribution),
            entry("evil-legacy", "MIT", LicenceClass::AdaptWithAttribution),
        ];
        for (slug, _) in HOSTILE_PREFIXES {
            entries.push(entry(slug, "MIT", LicenceClass::AdaptWithAttribution));
        }
        let bytes = catalogue_bytes(entries);
        let catalogue = dir.join("catalogue.json");
        std::fs::write(&catalogue, &bytes).expect("catalogue file");
        std::env::set_var("XERJ_CODE_HOME", &home);
        Fixture {
            catalogue,
            catalogue_bytes: bytes,
            dir,
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// A node under test
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Opts {
    public: bool,
    /// Turn authentication on, with [`ADMIN_KEY`].
    keyed: bool,
    catalogue_path: Option<String>,
    trusted_proxies: Vec<String>,
}

impl Opts {
    fn new(public: bool, keyed: bool) -> Self {
        Self {
            public,
            keyed,
            catalogue_path: None,
            trusted_proxies: vec![],
        }
    }
}

struct Hub {
    app: axum::Router,
    state: AppState,
    keyed: bool,
    _data: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }
    /// The body parsed as exactly an `Answer` — `deny_unknown_fields` makes
    /// this the wire-contract check.
    fn answer(&self) -> Answer {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "not an Answer ({e}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

fn config_for(o: &Opts, data: &std::path::Path) -> xerj_common::config::Config {
    let fx = fixture();
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = data.to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.hub.public = o.public;
    config.hub.catalogue_path = o
        .catalogue_path
        .clone()
        .unwrap_or_else(|| fx.catalogue.to_string_lossy().into_owned());
    config.server.trusted_proxies = o.trusted_proxies.clone();
    if o.keyed {
        config.auth.enabled = true;
        config.auth.admin_api_key = ADMIN_KEY.to_string();
    } else {
        config.auth.enabled = false;
    }
    config
}

fn build(o: &Opts) -> (AppState, tempfile::TempDir) {
    let data = tempfile::tempdir().expect("tempdir");
    let config = config_for(o, data.path());
    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
    (
        xerj_api::state::AppState::new(config, engine, metrics),
        data,
    )
}

/// A node with no indices at all.
fn bare(o: &Opts) -> Hub {
    fixture();
    let (state, data) = build(o);
    let app = xerj_api::router::build_es_compat_router(state.clone());
    Hub {
        app,
        state,
        keyed: o.keyed,
        _data: data,
    }
}

/// A node with the fixture indices seeded.
async fn hub(o: Opts) -> Hub {
    let hub = bare(&o);
    seed(&hub).await;
    hub
}

impl Hub {
    async fn send(&self, req: Request<Body>) -> Reply {
        let resp = self.app.clone().oneshot(req).await.expect("response");
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        Reply {
            status,
            headers,
            body,
        }
    }

    fn req(method: &str, path: &str) -> axum::http::request::Builder {
        Request::builder().method(method).uri(path)
    }

    async fn get(&self, path: &str) -> Reply {
        self.send(Self::req("GET", path).body(Body::empty()).unwrap())
            .await
    }

    async fn get_keyed(&self, path: &str) -> Reply {
        self.send(
            Self::req("GET", path)
                .header(header::AUTHORIZATION, format!("ApiKey {ADMIN_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn method(&self, method: &str, path: &str) -> Reply {
        self.send(Self::req(method, path).body(Body::empty()).unwrap())
            .await
    }

    async fn method_keyed(&self, method: &str, path: &str) -> Reply {
        self.send(
            Self::req(method, path)
                .header(header::AUTHORIZATION, format!("ApiKey {ADMIN_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// A request as if it arrived from `peer`.
    async fn get_from(&self, path: &str, peer: &str, xff: Option<&str>) -> Reply {
        let mut b = Self::req("GET", path);
        if let Some(x) = xff {
            b = b.header("x-forwarded-for", x);
        }
        let mut req = b.body(Body::empty()).unwrap();
        let addr: SocketAddr = format!("{peer}:40000").parse().expect("peer");
        req.extensions_mut().insert(ConnectInfo(addr));
        self.send(req).await
    }

    /// A GET from a fresh client address each time, so the per-address rate
    /// limit never fires: for tests that send many requests and are about
    /// something else.
    async fn stranger(&self, path: &str) -> Reply {
        let n = NEXT_PEER.fetch_add(1, Ordering::Relaxed);
        let peer = format!("10.{}.{}.{}", (n >> 16) & 255, (n >> 8) & 255, n & 255);
        self.get_from(path, &peer, None).await
    }

    /// `stranger`, presenting `ApiKey <encoded>`.
    async fn stranger_with_key(&self, path: &str, encoded: &str) -> Reply {
        let n = NEXT_PEER.fetch_add(1, Ordering::Relaxed);
        let peer = format!("10.{}.{}.{}", (n >> 16) & 255, (n >> 8) & 255, n & 255);
        let mut req = Self::req("GET", path)
            .header(header::AUTHORIZATION, format!("ApiKey {encoded}"))
            .body(Body::empty())
            .unwrap();
        let addr: SocketAddr = format!("{peer}:40000").parse().expect("peer");
        req.extensions_mut().insert(ConnectInfo(addr));
        self.send(req).await
    }

    /// Mint an API key (as the admin) that may read only `patterns`; returns the
    /// `encoded` value to present as `ApiKey <encoded>`.
    async fn mint_read_key(&self, name: &str, patterns: &[&str]) -> String {
        let r = self
            .admin(
                "POST",
                "/_security/api_key",
                Some(&json!({
                    "name": name,
                    "role_descriptors": { "r": { "indices": [
                        { "names": patterns, "privileges": ["read"] }
                    ]}}
                })),
            )
            .await;
        assert!(r.status.is_success(), "mint {name}: {:?}", r.json());
        r.json()["encoded"]
            .as_str()
            .unwrap_or_else(|| panic!("no encoded key in {:?}", r.json()))
            .to_string()
    }

    /// A direct (non-hub) `POST /{index}/_search` presenting `ApiKey <encoded>`.
    async fn direct_search_with_key(&self, index: &str, encoded: &str) -> Reply {
        self.send(
            Self::req("POST", &format!("/{index}/_search"))
                .header(header::AUTHORIZATION, format!("ApiKey {encoded}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"query": {"match_all": {}}}).to_string()))
                .unwrap(),
        )
        .await
    }

    /// A request of any method as if it arrived from `peer`.
    async fn method_from(&self, method: &str, path: &str, peer: &str) -> Reply {
        let mut req = Self::req(method, path).body(Body::empty()).unwrap();
        let addr: SocketAddr = format!("{peer}:40000").parse().expect("peer");
        req.extensions_mut().insert(ConnectInfo(addr));
        self.send(req).await
    }

    /// Admin-side call used only to seed the node.
    async fn admin(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let mut b = Self::req(method, path);
        if self.keyed {
            b = b.header(header::AUTHORIZATION, format!("ApiKey {ADMIN_KEY}"));
        }
        let req = match body {
            Some(v) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string())),
            None => b.body(Body::empty()),
        }
        .unwrap();
        self.send(req).await
    }

    async fn bulk(&self, index: &str, docs: &[Value]) {
        let mut nd = String::new();
        for d in docs {
            nd.push_str("{\"index\":{}}\n");
            nd.push_str(&d.to_string());
            nd.push('\n');
        }
        let mut b = Self::req("POST", &format!("/{index}/_bulk"))
            .header(header::CONTENT_TYPE, "application/x-ndjson");
        if self.keyed {
            b = b.header(header::AUTHORIZATION, format!("ApiKey {ADMIN_KEY}"));
        }
        let r = self.send(b.body(Body::from(nd)).unwrap()).await;
        assert!(r.status.is_success(), "bulk into {index}: {:?}", r.json());
        assert_eq!(
            r.json()["errors"],
            json!(false),
            "bulk into {index}: {:?}",
            r.json()
        );
    }

    async fn create(&self, index: &str, mapping: Value) {
        let r = self
            .admin(
                "PUT",
                &format!("/{index}"),
                Some(&json!({ "mappings": mapping })),
            )
            .await;
        assert!(r.status.is_success(), "create {index}: {:?}", r.json());
    }
}

// ── fixture indices ──────────────────────────────────────────────────────────

fn defs_mapping() -> Value {
    json!({ "properties": {
        "symbol": {"type": "keyword"}, "symbol_text": {"type": "text"},
        "kind": {"type": "keyword"}, "lang": {"type": "keyword"},
        "file": {"type": "keyword"}, "line": {"type": "integer"},
        "text": {"type": "text"}, "value": {"type": "keyword"}
    }})
}

fn docs_mapping() -> Value {
    json!({ "properties": {
        "body": {"type": "text"}, "defs": {"type": "text"}, "title": {"type": "text"}
    }})
}

fn def(symbol: &str, kind: &str, file: &str, line: u32, text: &str, value: Option<&str>) -> Value {
    json!({
        "symbol": symbol, "symbol_text": symbol, "kind": kind, "lang": "c",
        "file": file, "line": line, "text": text, "value": value,
    })
}

const HASH_C: &str = "#include \"limits.h\"\n#define MINI_HASH_SEED 5381\n\nunsigned mini_hash(const char *s) {\n    unsigned h = MINI_HASH_SEED;\n    while (*s) h = h * 33 + (unsigned char)*s++;\n    return h;\n}\n";
const CLAMP_C: &str =
    "int mini_clamp(int value, int lo, int hi) {\n    return value < lo ? lo : value > hi ? hi : value;\n}\n";

async fn seed(h: &Hub) {
    // ── mini-c (MIT) ──
    h.create("xc-mini-c-b1-defs", defs_mapping()).await;
    let mut defs = vec![
        def(
            "MINI_LIMIT",
            "define",
            "limits.h",
            5,
            "#define MINI_LIMIT 128",
            Some("128"),
        ),
        def(
            "MINI_HASH_SEED",
            "define",
            "hash.c",
            2,
            "#define MINI_HASH_SEED 5381",
            Some("5381"),
        ),
        def(
            "mini_hash",
            "function",
            "hash.c",
            4,
            "unsigned mini_hash(const char *s)",
            None,
        ),
        def(
            "mini_clamp",
            "function",
            "clamp.c",
            4,
            "int mini_clamp(int value, int lo, int hi)",
            None,
        ),
    ];
    // Sixty definitions of one symbol, to see `k=1000` clamped to 50.
    for n in 0..60 {
        defs.push(def(
            "DUP_SYM",
            "define",
            &format!("dup{n:02}.h"),
            n + 1,
            &format!("#define DUP_SYM {n}"),
            Some(&n.to_string()),
        ));
    }
    h.bulk("xc-mini-c-b1-defs", &defs).await;
    h.create("xc-mini-c-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-mini-c-b1-docs",
        &[
            json!({"ax_path": "mini-c/hash.c", "title": "hash.c", "body": HASH_C,
                   "defs": "define MINI_HASH_SEED\nfunction mini_hash",
                   "symbols": [{"name": "mini_hash", "kind": "function", "line": 4}]}),
            json!({"ax_path": "mini-c/clamp.c", "title": "clamp.c", "body": CLAMP_C,
                   "defs": "function mini_clamp",
                   "symbols": [{"name": "mini_clamp", "kind": "function", "line": 1}]}),
            json!({"ax_path": "mini-c/NOTES.md", "title": "notes",
                   "body": "frobnicate the buffer, then frobnicate it again"}),
            json!({"ax_path": "mini-c/README.md", "title": "readme",
                   "body": "A tiny C library. Call frobnicate before you hash anything."}),
        ],
    )
    .await;

    // ── mini-gpl (approach-only) ──
    h.create("xc-mini-gpl-b2-defs", defs_mapping()).await;
    h.bulk(
        "xc-mini-gpl-b2-defs",
        &[def(
            "GPL_LIMIT",
            "define",
            "limits.h",
            9,
            "#define GPL_LIMIT 256",
            Some("256"),
        )],
    )
    .await;
    h.create("xc-mini-gpl-b2-docs", docs_mapping()).await;
    h.bulk(
        "xc-mini-gpl-b2-docs",
        &[json!({"ax_path": "mini-gpl/gpl.c", "title": "gpl.c",
                 "body": "int gpl_frob(void) {\n    /* frobnicate frobnicate frobnicate the registry */\n    return 1;\n}\n",
                 "defs": "function gpl_frob",
                 "symbols": [{"name": "gpl_frob", "kind": "function", "line": 1}]})],
    )
    .await;

    // ── mini-closed (excluded): must never be served ──
    h.create("xc-mini-closed-b1-defs", defs_mapping()).await;
    h.bulk(
        "xc-mini-closed-b1-defs",
        &[def(
            "CLOSED_LIMIT",
            "define",
            "c.h",
            1,
            "#define CLOSED_LIMIT 7",
            Some("7"),
        )],
    )
    .await;
    h.create("xc-mini-closed-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-mini-closed-b1-docs",
        &[json!({"ax_path": "mini-closed/c.c", "body": "frobnicate closed text"})],
    )
    .await;

    // ── an index shaped like a hub pack that no catalogue entry lists ──
    h.create("xc-orphan-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-orphan-b1-docs",
        &[json!({"ax_path": "orphan/o.c", "body": "frobnicate orphan text"})],
    )
    .await;

    // ── leaky: a real docs index (so the corpus looks loaded) whose defs
    //    index is an ALIAS for a private index outside the hub namespace ──
    h.create("xc-leaky-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-leaky-b1-docs",
        &[json!({"ax_path": "leaky/l.c", "body": "leaky"})],
    )
    .await;
    h.create("secret-notes", defs_mapping()).await;
    h.bulk(
        "secret-notes",
        &[def(
            "SECRET",
            "define",
            "secret.h",
            1,
            "#define SECRET hunter2",
            Some("hunter2"),
        )],
    )
    .await;
    let alias = h
        .admin(
            "POST",
            "/_aliases",
            Some(&json!({"actions": [{"add": {"index": "secret-notes", "alias": "xc-leaky-b1-defs"}}]})),
        )
        .await;
    assert!(alias.status.is_success(), "alias: {:?}", alias.json());

    // ── corpora whose catalogue entry and node ledger disagree about the licence ──
    //
    // Each gets a docs index (`frobnicate …`) and a defs index, so every route
    // has something of theirs to find.
    for (slug, limit) in [
        ("ledger-closed", "LEDGER_CLOSED_LIMIT"),
        ("cat-closed", "CAT_CLOSED_LIMIT"),
        ("gplish", "GPLISH_LIMIT"),
        ("ledger-gpl", "LEDGER_GPL_LIMIT"),
    ] {
        h.create(&format!("xc-{slug}-b1-defs"), defs_mapping())
            .await;
        h.bulk(
            &format!("xc-{slug}-b1-defs"),
            &[def(
                limit,
                "define",
                "limits.h",
                3,
                &format!("#define {limit} 9"),
                Some("9"),
            )],
        )
        .await;
        h.create(&format!("xc-{slug}-b1-docs"), docs_mapping())
            .await;
        h.bulk(
            &format!("xc-{slug}-b1-docs"),
            &[json!({"ax_path": format!("{slug}/s.c"),
                     "body": format!("frobnicate {slug} text frobnicate"),
                     "defs": format!("function {slug}_fn"),
                     "symbols": [{"name": format!("{slug}_fn"), "kind": "function", "line": 1}]})],
        )
        .await;
    }

    // Searchable now, not after the next refresh tick.
    let r = h
        .admin(
            "POST",
            "/xc-mini-c-b1-defs,xc-mini-c-b1-docs,xc-mini-gpl-b2-defs,xc-mini-gpl-b2-docs,\
             xc-mini-closed-b1-defs,xc-mini-closed-b1-docs,xc-orphan-b1-docs,\
             xc-leaky-b1-docs,secret-notes,\
             xc-ledger-closed-b1-defs,xc-ledger-closed-b1-docs,\
             xc-cat-closed-b1-defs,xc-cat-closed-b1-docs,\
             xc-gplish-b1-defs,xc-gplish-b1-docs,\
             xc-ledger-gpl-b1-defs,xc-ledger-gpl-b1-docs/_refresh",
            None,
        )
        .await;
    assert!(
        r.status.is_success(),
        "refresh: {:?} {:?}",
        r.status,
        r.json()
    );
}

const FOUR: [&str; 4] = [
    "/_hub/v1/catalogue",
    "/_hub/v1/def/mini-c/MINI_LIMIT",
    "/_hub/v1/code/mini-c?q=mini_hash+unsigned",
    "/_hub/v1/search?q=frobnicate",
];

// ─────────────────────────────────────────────────────────────────────────────
// The four answers
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalogue_is_the_files_bytes_with_a_sha256_etag_and_a_304() {
    let h = hub(Opts::new(false, false)).await;
    let fx = fixture();
    let want_etag = format!("\"{:x}\"", Sha256::digest(&fx.catalogue_bytes));

    let r = h.get("/_hub/v1/catalogue").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body.as_ref(),
        fx.catalogue_bytes.as_slice(),
        "the file, byte for byte"
    );
    assert_eq!(r.header("etag").as_deref(), Some(want_etag.as_str()));
    assert_eq!(
        r.header("cache-control").as_deref(),
        Some("public, max-age=3600")
    );
    assert_eq!(
        r.header("content-type").as_deref(),
        Some("application/json")
    );

    // The validators a cache sends: exact, weak, a list, and `*` all revalidate.
    for inm in [
        want_etag.clone(),
        format!("W/{want_etag}"),
        format!("\"nope\", {want_etag}"),
        "*".to_string(),
    ] {
        let r = h
            .send(
                Hub::req("GET", "/_hub/v1/catalogue")
                    .header(header::IF_NONE_MATCH, &inm)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_MODIFIED, "If-None-Match: {inm}");
        assert!(r.body.is_empty(), "a 304 carries no body");
        assert_eq!(r.header("etag").as_deref(), Some(want_etag.as_str()));
        assert_eq!(
            r.header("cache-control").as_deref(),
            Some("public, max-age=3600")
        );
    }
    let r = h
        .send(
            Hub::req("GET", "/_hub/v1/catalogue")
                .header(header::IF_NONE_MATCH, "\"something-else\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "a different validator gets the file"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalogue_follows_the_file_when_it_changes() {
    fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalogue.json");
    let first = catalogue_bytes(vec![entry(
        "alpha",
        "MIT",
        LicenceClass::AdaptWithAttribution,
    )]);
    std::fs::write(&path, &first).unwrap();
    let mut o = Opts::new(false, false);
    o.catalogue_path = Some(path.to_string_lossy().into_owned());
    let (state, _data) = build(&o);
    let app = xerj_api::router::build_es_compat_router(state);
    let get = |app: axum::Router| async move {
        let resp = app
            .oneshot(
                Request::get("/_hub/v1/catalogue")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = resp
            .headers()
            .get("etag")
            .map(|v| v.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (etag, body)
    };
    let (etag1, body1) = get(app.clone()).await;
    assert_eq!(body1.as_ref(), first.as_slice());

    // A longer file: size differs, so the cache cannot be fooled by an mtime
    // tick shorter than the filesystem's granularity.
    let second = catalogue_bytes(vec![
        entry("alpha", "MIT", LicenceClass::AdaptWithAttribution),
        entry("beta", "MIT", LicenceClass::AdaptWithAttribution),
    ]);
    std::fs::write(&path, &second).unwrap();
    let (etag2, body2) = get(app).await;
    assert_eq!(
        body2.as_ref(),
        second.as_slice(),
        "the new bytes are served"
    );
    assert_ne!(etag1, etag2, "and under a new ETag");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn def_answers_with_the_fixture_value_the_pin_and_a_hosted_origin() {
    let h = hub(Opts::new(false, false)).await;
    let r = h.get("/_hub/v1/def/mini-c/MINI_LIMIT").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let a = r.answer();
    assert_eq!(a.corpus, "mini-c");
    assert_eq!(a.mode, AnswerMode::Def);
    assert_eq!(a.pin_sha, PIN_SHA);
    assert_eq!(a.pin_date, PIN_DATE);
    assert_eq!(a.licence_spdx, "MIT");
    assert_eq!(a.licence_class, LicenceClass::AdaptWithAttribution);
    assert_eq!(
        a.answered_from,
        AnsweredFrom::Hosted,
        "a hosted answer says so, whatever the server read locally"
    );
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    assert_eq!(a.hits.len(), 1);
    let hit = &a.hits[0];
    assert_eq!((hit.file.as_str(), hit.line), ("limits.h", 5));
    assert_eq!(hit.kind, "define");
    assert_eq!(hit.value.as_deref(), Some("128"));
    assert_eq!(hit.licence_warning, None);

    // A miss is an answer, not an error.
    let r = h.get("/_hub/v1/def/mini-c/NO_SUCH_SYMBOL").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.answer().hits.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn code_returns_a_passage_with_file_and_line() {
    let h = hub(Opts::new(false, false)).await;
    // The snake_case names are single tokens in this engine's analyzer, so the
    // query uses them whole.
    let r = h.get("/_hub/v1/code/mini-c?q=mini_hash+unsigned").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let a = r.answer();
    assert_eq!(a.corpus, "mini-c");
    assert_eq!(a.mode, AnswerMode::How);
    assert_eq!(a.answered_from, AnsweredFrom::Hosted);
    assert!(!a.truncated);
    assert!(!a.hits.is_empty());
    assert!(
        a.hits
            .iter()
            .all(|x| !x.file.is_empty() && x.line > 0 && x.score > 0.0),
        "every hit cites a file and a real line: {:#?}",
        a.hits
    );
    // The documents index returns the file; the pack's definitions index may
    // return a hit for the same name too, so pick the file's own hit.
    let top = a
        .hits
        .iter()
        .find(|x| x.file == "mini-c/hash.c")
        .unwrap_or_else(|| panic!("no hit for mini-c/hash.c in {:#?}", a.hits));
    assert_eq!(
        top.line, 4,
        "the matched definition's line, never an invented one"
    );
    assert_eq!(top.kind, "function");
    assert!(top.text.contains("mini_hash"), "{}", top.text);

    // The pack's definitions index is under the same prefix: a definition that
    // comes back keeps its kind, its value and its exact line.
    let a = h.get("/_hub/v1/code/mini-c?q=MINI_LIMIT").await.answer();
    let def = a
        .hits
        .iter()
        .find(|x| x.file == "limits.h")
        .unwrap_or_else(|| panic!("no definition hit in {:#?}", a.hits));
    assert_eq!((def.line, def.kind.as_str()), (5, "define"));
    assert_eq!(def.value.as_deref(), Some("128"));
    assert_eq!(def.text, "#define MINI_LIMIT 128");

    // A query nothing matches is a 200 with no hits.
    let r = h.get("/_hub/v1/code/mini-c?q=zzzzqqqq").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.answer().hits.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn code_max_tokens_holds_the_answer_and_says_so() {
    let h = hub(Opts::new(false, false)).await;
    // Two records mention `frobnicate` (NOTES.md and README.md); 10 tokens is
    // 40 characters, which one passage fills and two cannot.
    let full = h.get("/_hub/v1/code/mini-c?q=frobnicate").await.answer();
    assert_eq!(full.hits.len(), 2, "{:#?}", full.hits);
    assert!(!full.truncated, "without a budget nothing is cut");
    assert!(full.warnings.is_empty());

    let r = h
        .get("/_hub/v1/code/mini-c?q=frobnicate&max_tokens=10")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let a = r.answer();
    let chars: usize = a.hits.iter().map(|x| x.text.chars().count()).sum();
    assert!(chars <= 40, "{chars} characters over a 40-character budget");
    assert!(
        !a.hits.is_empty(),
        "the budget cuts, it does not empty the answer"
    );
    assert!(
        a.hits.len() < full.hits.len()
            || chars < full.hits.iter().map(|x| x.text.chars().count()).sum(),
        "something was held back"
    );
    assert!(a.truncated, "{a:#?}");
    assert!(
        a.warnings.iter().any(|w| w.contains("max_tokens=10")),
        "{:?}",
        a.warnings
    );
    // The hit that survives is the better-ranked one, and still cites its file.
    assert_eq!(a.hits[0].file, full.hits[0].file);

    for bad in ["0", "9999", "abc", "-1", ""] {
        let r = h
            .get(&format!("/_hub/v1/code/mini-c?q=x&max_tokens={bad}"))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "max_tokens={bad}");
    }
    let r = h.get("/_hub/v1/code/mini-c?q=x&mode=nope").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // The other arms are accepted (what they do is pinned in
    // `semantic_and_hybrid_say_what_they_actually_do`).
    for mode in ["semantic", "hybrid"] {
        let r = h
            .get(&format!("/_hub/v1/code/mini-c?q=mini_hash&mode={mode}"))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{mode}: {:?}", r.json());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn semantic_and_hybrid_say_what_they_actually_do() {
    // The fixture corpus maps `body` as plain text: it has no `semantic_text`
    // index for a vector arm to search.
    let h = hub(Opts::new(false, false)).await;
    let bm25 = h
        .get("/_hub/v1/code/mini-c?q=mini_hash+unsigned")
        .await
        .answer();
    assert!(!bm25.hits.is_empty());
    assert!(bm25.warnings.is_empty(), "{:?}", bm25.warnings);

    // `semantic` over a corpus with no semantic index finds NOTHING; it does not
    // fall back to BM25 (that is what `hybrid` does). The answer must say so.
    let sem = h
        .get("/_hub/v1/code/mini-c?q=mini_hash+unsigned&mode=semantic")
        .await;
    assert_eq!(sem.status, StatusCode::OK, "{:?}", sem.json());
    let sem = sem.answer();
    assert!(
        sem.hits.is_empty(),
        "the premise: semantic returns no hits here: {:#?}",
        sem.hits
    );
    let said = sem.warnings.join(" | ");
    assert!(!said.is_empty(), "an empty semantic answer explains itself");
    assert!(
        !said.contains("falls back to BM25"),
        "semantic does not fall back, and must not claim to: {said}"
    );
    assert!(
        said.contains("semantic_text") && said.contains("no hits"),
        "names why it is empty: {said}"
    );

    // `hybrid` over the same corpus answers from BM25 alone, and says that is
    // what happened.
    let hyb = h
        .get("/_hub/v1/code/mini-c?q=mini_hash+unsigned&mode=hybrid")
        .await
        .answer();
    assert!(
        !hyb.hits.is_empty(),
        "hybrid degrades to BM25 and still answers"
    );
    let said = hyb.warnings.join(" | ");
    assert!(
        said.contains("BM25") && said.contains("semantic_text"),
        "names the arm it used and the mapping it lacked: {said}"
    );
    assert_ne!(
        sem.warnings, hyb.warnings,
        "the two modes behave differently, so they are described differently"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_groups_by_corpus_best_first_and_lists_only_servable_corpora() {
    let h = hub(Opts::new(false, false)).await;
    let r = h.get("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let v = r.json();
    assert_eq!(
        v.as_object().unwrap().len(),
        1,
        "exactly {{corpora: [...]}}"
    );
    let rows = v["corpora"].as_array().expect("corpora");
    let slugs: Vec<&str> = rows.iter().map(|c| c["slug"].as_str().unwrap()).collect();
    assert!(
        slugs.contains(&"mini-c") && slugs.contains(&"mini-gpl"),
        "{slugs:?}"
    );
    assert!(
        !slugs.contains(&"mini-closed"),
        "an excluded corpus is never served: {slugs:?}"
    );
    assert!(
        !slugs.contains(&"orphan"),
        "an index no catalogue entry lists is skipped: {slugs:?}"
    );
    let unique: std::collections::BTreeSet<_> = slugs.iter().collect();
    assert_eq!(unique.len(), slugs.len(), "one row per corpus: {slugs:?}");
    assert!(rows.len() <= 10);
    let scores: Vec<f64> = rows
        .iter()
        .map(|c| c["best_score"].as_f64().unwrap())
        .collect();
    assert!(
        scores.windows(2).all(|w| w[0] >= w[1]),
        "best first: {scores:?}"
    );
    for row in rows {
        let hit: Hit = serde_json::from_value(row["best_hit"].clone()).expect("best_hit is a Hit");
        assert!(!hit.file.is_empty());
        assert!((hit.score as f64 - row["best_score"].as_f64().unwrap()).abs() < 1e-3);
    }
    let gpl = rows.iter().find(|c| c["slug"] == "mini-gpl").unwrap();
    assert_eq!(
        gpl["best_hit"]["licence_warning"],
        json!("approach-only licence (GPL-3.0-only): read, do not copy")
    );

    // A definition name finds the corpus through its -defs index.
    let v = h.get("/_hub/v1/search?q=mini_clamp").await.json();
    let top = &v["corpora"][0];
    assert_eq!(top["slug"], "mini-c", "{v}");

    // `k` bounds the number of corpora.
    let v = h.get("/_hub/v1/search?q=frobnicate&k=1").await.json();
    assert_eq!(v["corpora"].as_array().unwrap().len(), 1);

    // Nothing matches: an empty list, not an error.
    let r = h.get("/_hub/v1/search?q=zzzzqqqq").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"corpora": []}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_pack_restored_is_searchable_on_the_next_request() {
    // `search` reuses the field list it resolved from the mappings for a minute.
    // An empty node (no hub index yet) must not be remembered as "nothing to
    // search": a node that has just restored its first pack answers at once.
    let h = bare(&Opts::new(false, false));
    let r = h.get("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({"corpora": []}), "nothing restored yet");

    h.create("xc-mini-c-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-mini-c-b1-docs",
        &[json!({"ax_path": "mini-c/NOTES.md", "body": "frobnicate the buffer"})],
    )
    .await;
    let r = h.admin("POST", "/xc-mini-c-b1-docs/_refresh", None).await;
    assert!(r.status.is_success());

    let v = h.get("/_hub/v1/search?q=frobnicate").await.json();
    assert_eq!(v["corpora"][0]["slug"], "mini-c", "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn def_and_code_agree_on_what_they_say_about_a_corpus() {
    let h = hub(Opts::new(false, false)).await;
    for (slug, sym, q) in [
        ("mini-c", "MINI_LIMIT", "hash"),
        ("mini-gpl", "GPL_LIMIT", "frobnicate"),
    ] {
        let d = h.get(&format!("/_hub/v1/def/{slug}/{sym}")).await.answer();
        let c = h.get(&format!("/_hub/v1/code/{slug}?q={q}")).await.answer();
        assert_eq!(
            (
                d.corpus,
                d.pin_sha,
                d.pin_date,
                d.age_days,
                d.licence_spdx,
                d.licence_class,
                d.answered_from
            ),
            (
                c.corpus,
                c.pin_sha,
                c.pin_date,
                c.age_days,
                c.licence_spdx,
                c.licence_class,
                c.answered_from
            ),
            "{slug}: def and code must describe the corpus identically"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approach_only_corpus_tags_every_hit_with_its_licence() {
    let h = hub(Opts::new(false, false)).await;
    let want = "approach-only licence (GPL-3.0-only): read, do not copy";
    let d = h.get("/_hub/v1/def/mini-gpl/GPL_LIMIT").await.answer();
    assert_eq!(d.licence_class, LicenceClass::ApproachOnly);
    assert_eq!(d.hits[0].licence_warning.as_deref(), Some(want));
    let c = h.get("/_hub/v1/code/mini-gpl?q=frobnicate").await.answer();
    assert!(!c.hits.is_empty());
    assert!(c
        .hits
        .iter()
        .all(|x| x.licence_warning.as_deref() == Some(want)));
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corpora_the_catalogue_does_not_serve_are_404() {
    let h = hub(Opts::new(false, false)).await;
    for path in [
        "/_hub/v1/def/not-a-corpus/X",
        "/_hub/v1/code/not-a-corpus?q=x",
        // Listed but excluded: the same 404, so a caller cannot tell it exists.
        "/_hub/v1/def/mini-closed/CLOSED_LIMIT",
        "/_hub/v1/code/mini-closed?q=frobnicate",
        // The catalogue permits it but this node's own ledger says excluded.
        "/_hub/v1/def/ledger-closed/X",
        "/_hub/v1/code/ledger-closed?q=x",
        // Not a name at all.
        "/_hub/v1/def/..%2F..%2Fetc/passwd",
        "/_hub/v1/code/mini-c%00?q=x",
        "/_hub/v1/code/-lead?q=x",
        "/_hub/v1/code/mini-c*?q=x",
    ] {
        let r = h.get(path).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {:?}", r.json());
        assert_eq!(r.json()["status"], json!(404));
        assert_eq!(
            r.json()["error"]["type"],
            json!("resource_not_found_exception"),
            "{path}: refusals look alike"
        );
    }
    // Listed, servable, but this node never pulled it: a different 404.
    for path in ["/_hub/v1/def/ghost/X", "/_hub/v1/code/ghost?q=x"] {
        let r = h.get(path).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            r.json()["error"]["type"],
            json!("corpus_not_loaded"),
            "{path}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k_is_clamped_to_fifty_and_garbage_k_is_400() {
    let h = hub(Opts::new(false, false)).await;
    let n = |v: Value| v["hits"].as_array().unwrap().len();
    let r = h.get("/_hub/v1/def/mini-c/DUP_SYM?k=1000").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(n(r.json()), 50, "sixty exist; k=1000 is clamped to 50");
    assert_eq!(n(h.get("/_hub/v1/def/mini-c/DUP_SYM?k=2").await.json()), 2);
    assert_eq!(
        n(h.get("/_hub/v1/def/mini-c/DUP_SYM").await.json()),
        5,
        "default k"
    );
    assert_eq!(
        n(h.get("/_hub/v1/def/mini-c/DUP_SYM?k=50").await.json()),
        50
    );
    for bad in ["0", "-1", "abc", "1.5", ""] {
        let r = h.get(&format!("/_hub/v1/def/mini-c/DUP_SYM?k={bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "k={bad}");
    }
    // The pure parser agrees.
    assert_eq!(hub_api::parse_k(Some("1000"), 5), Ok(50));
    assert_eq!(hub_api::parse_k(None, 7), Ok(7));
    assert!(hub_api::parse_k(Some("0"), 5).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_blank_missing_and_stray_inputs_are_400() {
    let h = hub(Opts::new(false, false)).await;
    let big = "a".repeat(600);
    let edge = "a".repeat(512);
    for path in [
        format!("/_hub/v1/code/mini-c?q={big}"),
        format!("/_hub/v1/search?q={big}"),
        format!("/_hub/v1/def/mini-c/{big}"),
        "/_hub/v1/code/mini-c".to_string(),
        "/_hub/v1/search".to_string(),
        "/_hub/v1/code/mini-c?q=%20%20".to_string(),
        "/_hub/v1/search?q=x&nope=1".to_string(),
        "/_hub/v1/def/mini-c/MINI_LIMIT?q=x".to_string(),
        "/_hub/v1/search?q=x&q=y".to_string(),
        "/_hub/v1/def/mini-c/MINI_LIMIT?k=1&k=2".to_string(),
    ] {
        let r = h.get(&path).await;
        assert_eq!(
            r.status,
            StatusCode::BAD_REQUEST,
            "{}: {:?}",
            &path[..path.len().min(60)],
            r.json()
        );
        assert_eq!(
            r.json()["error"]["type"],
            json!("illegal_argument_exception")
        );
    }
    // 512 bytes is the limit, not 511.
    for path in [
        format!("/_hub/v1/code/mini-c?q={edge}"),
        format!("/_hub/v1/search?q={edge}"),
        format!("/_hub/v1/def/mini-c/{edge}"),
    ] {
        let r = h.get(&path).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// hub.public
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyed_node_needs_the_key_on_all_four_routes() {
    let h = hub(Opts::new(false, true)).await;
    for path in FOUR {
        let r = h.get(path).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path} without a key");
        let r = h
            .send(
                Hub::req("GET", path)
                    .header(header::AUTHORIZATION, "ApiKey wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "{path} with a wrong key"
        );
        let r = h.get_keyed(path).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{path} with the key: {:?}",
            r.json()
        );
        // Only GET: with the key a POST is 405, without it 401 first.
        let r = h.method_keyed("POST", path).await;
        assert_eq!(
            r.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "POST {path} with the key"
        );
        let r = h.method("POST", path).await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "POST {path} without a key"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_public_node_opens_exactly_the_four_routes() {
    let h = hub(Opts::new(true, true)).await;
    for path in FOUR {
        let r = h.get(path).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{path} needs no key: {:?}",
            r.json()
        );
        // Every other method is a 405 from the router, and needs no key either:
        // there is no handler to protect, and nothing for the audit layer to log.
        for m in ["POST", "PUT", "DELETE", "PATCH"] {
            let r = h.method(m, path).await;
            assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m} {path}");
            let allow = r.header("allow").unwrap_or_default();
            assert!(allow.contains("GET"), "{m} {path}: Allow: {allow}");
        }
    }
    // HEAD is GET without a body: public like GET, and nothing more.
    let r = h.method("HEAD", FOUR[0]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.is_empty());
    assert!(r.header("etag").is_some());
    // The key still works on a public node.
    assert_eq!(h.get_keyed(FOUR[1]).await.status, StatusCode::OK);

    // Near misses are NOT public: a different shape falls through to
    // authentication like any other path.
    for path in [
        "/_hub/v1/def/mini-c",
        "/_hub/v1/def/mini-c/MINI_LIMIT/extra",
        "/_hub/v1/def//MINI_LIMIT",
        "/_hub/v1/code",
        "/_hub/v1/code/mini-c/extra",
        "/_hub/v1/code/",
        "/_hub/v1/search/",
        "/_hub/v1//search",
        "/_hub/v1/catalogue/",
        "/_hub/v1/catalogue/x",
        "/_hub/v1/other",
        "/_hub/v1",
        "/_hub",
        "/_HUB/v1/search",
        "/_hub/v2/search",
        "/x/_hub/v1/search",
        "/_hub/v1/../v1/search",
    ] {
        let r = h.get(path).await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "{path} must keep authentication"
        );
    }
    // Everything else on the node keeps authentication, and a hub-looking query
    // string does not change what the path is.
    for path in [
        "/_cluster/health",
        "/_cat/indices",
        "/xc-mini-c-b1-defs/_search",
        "/xc-mini-c-b1-defs/_mapping",
        "/_memory/x",
        "/_cluster/health?next=/_hub/v1/search",
        "/_aliases",
    ] {
        let r = h.get(path).await;
        assert_eq!(
            r.status,
            StatusCode::UNAUTHORIZED,
            "{path} must keep authentication"
        );
    }
    let r = h.method("POST", "/xc-mini-c-b1-defs/_search").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        h.get("/health/live").await.status,
        StatusCode::OK,
        "probes are unchanged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_hub_traffic_cannot_write_the_audit_ring() {
    let h = hub(Opts::new(true, true)).await;
    let audit = &h.state.engine.audit;

    // The operator's evidence: whatever is in the ring before any stranger
    // arrives (the seeding above wrote real entries).
    let before = audit.next_seq();
    let evidence = audit.snapshot();
    assert!(
        !evidence.is_empty(),
        "fixture sanity: the ring holds the operator's own entries"
    );
    let unchanged = |what: &str| {
        assert_eq!(audit.next_seq(), before, "{what}: the sequence moved");
        let now = audit.snapshot();
        assert_eq!(now.len(), evidence.len(), "{what}: the ring changed size");
        assert_eq!(
            now.iter().map(|e| e.hash.as_str()).collect::<Vec<_>>(),
            evidence.iter().map(|e| e.hash.as_str()).collect::<Vec<_>>(),
            "{what}: the ring's entries changed"
        );
    };

    // A flood of non-GET requests to the public paths and of 401s elsewhere
    // leaves nothing: the audit layer records unauthenticated write-shaped
    // requests, and it must not see these.
    // 4 rounds x 13 hub requests = 52, inside the 60-a-minute budget, so each
    // one reaches the layer that would have recorded it.
    for _ in 0..4 {
        for path in FOUR {
            for m in ["POST", "PUT", "DELETE"] {
                h.method(m, path).await;
            }
        }
        h.get("/_hub/v1/search/").await;
        h.get("/_cluster/health").await;
        h.get(FOUR[0]).await;
    }
    unchanged("write-shaped and unauthenticated requests");

    // The reads themselves. Every valid `def`, `code` and `search` GET is served
    // by the node's own `_search` handler, which audits what it serves; the hub's
    // in-process call must not. Distinct client addresses, so the rate limit
    // does not stand in the way: this is what a crowd of strangers sends.
    let reads = [
        "/_hub/v1/def/mini-c/MINI_LIMIT",
        "/_hub/v1/code/mini-c?q=mini_hash+unsigned",
        "/_hub/v1/search?q=frobnicate",
    ];
    // More reads than the ring holds (4,096): had each one appended an entry,
    // not one of the operator's entries would be left.
    let mut sent = 0usize;
    for round in 0..1400 {
        for path in reads {
            let r = h.stranger(path).await;
            assert_eq!(r.status, StatusCode::OK, "round {round} {path}");
            sent += 1;
        }
    }
    assert!(sent > xerj_engine::audit::DEFAULT_AUDIT_CAPACITY, "{sent}");
    unchanged("4,200 anonymous def/code/search reads");

    // The same holds for a caller who does present the key: the hub's reads are
    // not a source of audit entries in either mode.
    for path in reads {
        assert_eq!(h.get_keyed(path).await.status, StatusCode::OK);
    }
    unchanged("keyed hub reads");

    // The suppression belongs to the hub's in-process call and nothing else: a
    // direct `_search` by the operator is still audited.
    let r = h
        .direct_search_with_key("xc-mini-c-b1-docs", ADMIN_KEY)
        .await;
    assert!(r.status.is_success(), "{:?}", r.json());
    let after = audit.snapshot();
    let new: Vec<_> = after.iter().filter(|e| e.seq >= before).collect();
    assert_eq!(new.len(), 1, "exactly the direct search: {new:?}");
    assert_eq!(new[0].op, "search");
    assert_eq!(new[0].resource, "xc-mini-c-b1-docs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyed_node_does_not_audit_hub_reads_either() {
    let h = hub(Opts::new(false, true)).await;
    let before = h.state.engine.audit.next_seq();
    for path in FOUR {
        assert_eq!(h.get_keyed(path).await.status, StatusCode::OK, "{path}");
    }
    assert_eq!(
        h.state.engine.audit.next_seq(),
        before,
        "a hub read is not an audited data-path operation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_without_a_usable_catalogue_says_so() {
    fixture();
    let dir = tempfile::tempdir().unwrap();

    // No catalogue_path at all.
    let mut o = Opts::new(false, false);
    o.catalogue_path = Some(String::new());
    let (state, _d1) = build(&o);
    let app = xerj_api::router::build_es_compat_router(state);
    let status = |app: axum::Router, path: &'static str| async move {
        app.oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    };
    assert_eq!(
        status(app.clone(), "/_hub/v1/catalogue").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(app.clone(), "/_hub/v1/def/mini-c/X").await,
        StatusCode::NOT_FOUND
    );

    // A path that does not exist.
    let mut o = Opts::new(false, false);
    o.catalogue_path = Some(
        dir.path()
            .join("missing.json")
            .to_string_lossy()
            .into_owned(),
    );
    let (state, _d2) = build(&o);
    let app = xerj_api::router::build_es_compat_router(state);
    assert_eq!(
        status(app.clone(), "/_hub/v1/catalogue").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status(app, "/_hub/v1/search?q=x").await,
        StatusCode::SERVICE_UNAVAILABLE
    );

    // A file that is not a catalogue: the bytes are still served (a client
    // verifies the signature itself) but nothing is validated against them.
    let junk = dir.path().join("junk.json");
    std::fs::write(&junk, b"{\"not\": \"a catalogue\"}").unwrap();
    let mut o = Opts::new(false, false);
    o.catalogue_path = Some(junk.to_string_lossy().into_owned());
    let (state, _d3) = build(&o);
    let app = xerj_api::router::build_es_compat_router(state);
    assert_eq!(
        status(app.clone(), "/_hub/v1/catalogue").await,
        StatusCode::OK
    );
    assert_eq!(
        status(app.clone(), "/_hub/v1/def/mini-c/X").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status(app, "/_hub/v1/search?q=x").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Rate limiting
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_61st_request_in_a_minute_from_one_address_is_429() {
    let h = hub(Opts::new(true, true)).await;
    let path = "/_hub/v1/catalogue";
    for i in 1..=60 {
        let r = h.get_from(path, "203.0.113.7", None).await;
        assert_eq!(r.status, StatusCode::OK, "request {i}");
    }
    let r = h.get_from(path, "203.0.113.7", None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "the 61st");
    let retry: u64 = r
        .header("retry-after")
        .expect("Retry-After")
        .parse()
        .expect("seconds");
    assert!((1..=60).contains(&retry), "Retry-After: {retry}");
    assert_eq!(r.json()["error"]["type"], json!("too_many_requests"));
    assert_eq!(r.json()["status"], json!(429));
    // It stays limited, on every route, whatever the method: the limit sits in
    // front of the router's own 405.
    for p in FOUR {
        assert_eq!(
            h.get_from(p, "203.0.113.7", None).await.status,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    assert_eq!(
        h.method_from("POST", path, "203.0.113.7").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    // A non-GET spends budget too: 59 GETs, one POST (a 405, but counted), and the
    // next GET is refused.
    for _ in 0..59 {
        assert_eq!(
            h.get_from(path, "203.0.113.9", None).await.status,
            StatusCode::OK
        );
    }
    assert_eq!(
        h.method_from("POST", path, "203.0.113.9").await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        h.get_from(path, "203.0.113.9", None).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Another address has its own budget.
    assert_eq!(
        h.get_from(path, "203.0.113.8", None).await.status,
        StatusCode::OK
    );

    // A header the caller writes does not buy a fresh one: the peer decides.
    for n in 0..5 {
        let r = h
            .get_from(path, "203.0.113.7", Some(&format!("198.51.100.{n}")))
            .await;
        assert_eq!(
            r.status,
            StatusCode::TOO_MANY_REQUESTS,
            "X-Forwarded-For is not believed"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ipv6_host_shares_one_budget_across_its_64() {
    let h = hub(Opts::new(true, true)).await;
    let path = "/_hub/v1/catalogue";
    // Sixty different addresses out of one /64: one budget.
    for n in 0..60 {
        let peer = format!("[2001:db8:1:2::{n:x}]");
        assert_eq!(h.get_from(path, &peer, None).await.status, StatusCode::OK);
    }
    assert_eq!(
        h.get_from(path, "[2001:db8:1:2:ffff::1]", None)
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        h.get_from(path, "[2001:db8:1:3::1]", None).await.status,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn behind_a_declared_proxy_the_forwarded_client_is_the_bucket() {
    let mut o = Opts::new(true, true);
    o.trusted_proxies = vec!["10.0.0.0/8".into()];
    let h = hub(o).await;
    let path = "/_hub/v1/catalogue";
    for _ in 0..60 {
        let r = h.get_from(path, "10.0.0.1", Some("198.51.100.1")).await;
        assert_eq!(r.status, StatusCode::OK);
    }
    assert_eq!(
        h.get_from(path, "10.0.0.1", Some("198.51.100.1"))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS,
        "that client is spent"
    );
    assert_eq!(
        h.get_from(path, "10.0.0.1", Some("198.51.100.2"))
            .await
            .status,
        StatusCode::OK,
        "another client behind the same proxy is not"
    );
    // An undeclared peer cannot claim to be somebody else.
    assert_eq!(
        h.get_from(path, "192.0.2.9", Some("198.51.100.99"))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_limit_holds_on_a_keyed_node_too() {
    let h = hub(Opts::new(false, true)).await;
    let mut statuses = Vec::new();
    for _ in 0..61 {
        let mut req = Hub::req("GET", "/_hub/v1/catalogue")
            .header(header::AUTHORIZATION, format!("ApiKey {ADMIN_KEY}"))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo("203.0.113.40:1".parse::<SocketAddr>().unwrap()));
        statuses.push(h.send(req).await.status);
    }
    assert!(
        statuses[..60].iter().all(|s| *s == StatusCode::OK),
        "{statuses:?}"
    );
    assert_eq!(statuses[60], StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn the_limiter_is_a_sliding_window_with_bounded_memory() {
    let t0 = Instant::now();
    let l = RateLimiter::new(3, Duration::from_secs(60), 100);
    assert_eq!(l.admit("a", t0), Ok(()));
    assert_eq!(l.admit("a", t0 + Duration::from_secs(10)), Ok(()));
    assert_eq!(l.admit("a", t0 + Duration::from_secs(20)), Ok(()));
    // Full: the oldest ages out at t0+60s, so 40 seconds from t0+20s.
    assert_eq!(l.admit("a", t0 + Duration::from_secs(20)), Err(40));
    // Refusals are not recorded, so hammering does not extend the wait.
    assert_eq!(l.admit("a", t0 + Duration::from_secs(21)), Err(39));
    // One request ages out at t0+60s and exactly one slot opens.
    assert_eq!(l.admit("a", t0 + Duration::from_secs(60)), Ok(()));
    assert!(l.admit("a", t0 + Duration::from_secs(60)).is_err());
    // Never "0 seconds": a sub-second wait rounds up.
    assert_eq!(l.admit("a", t0 + Duration::from_millis(69_999)), Err(1));
    // A second client is independent.
    assert_eq!(l.admit("b", t0), Ok(()));

    // Past the table cap, unseen addresses share one overflow bucket.
    let small = RateLimiter::new(1, Duration::from_secs(60), 2);
    assert_eq!(small.admit("c1", t0), Ok(()));
    assert_eq!(small.admit("c2", t0), Ok(()));
    assert_eq!(small.admit("c3", t0), Ok(()), "first overflow request");
    assert!(
        small.admit("c4", t0).is_err(),
        "later strangers share that bucket"
    );
    // …and the table is reclaimed once the window has passed.
    assert_eq!(small.admit("c5", t0 + Duration::from_secs(61)), Ok(()));
}

#[test]
fn client_buckets_collapse_ipv6_to_the_64() {
    assert_eq!(hub_api::client_bucket("203.0.113.9"), "203.0.113.9");
    assert_eq!(
        hub_api::client_bucket("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
        hub_api::client_bucket("2001:db8:1:2::1")
    );
    assert_eq!(
        hub_api::client_bucket("2001:db8:1:2::1"),
        "2001:db8:1:2::/64"
    );
    assert_ne!(
        hub_api::client_bucket("2001:db8:1:3::1"),
        hub_api::client_bucket("2001:db8:1:2::1")
    );
    assert_eq!(hub_api::client_bucket("unknown"), "unknown");
}

// ─────────────────────────────────────────────────────────────────────────────
// A real socket on an ephemeral port
// ─────────────────────────────────────────────────────────────────────────────

/// One HTTP/1.0 GET over a real connection: (status, lower-cased headers, body).
async fn raw_get(
    addr: SocketAddr,
    path: &str,
    extra: &[(&str, &str)],
) -> (u16, Vec<(String, String)>, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut head = format!("GET {path} HTTP/1.0\r\nHost: hub.test\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("a response");
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers, body.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_a_real_socket_the_peer_address_is_the_bucket() {
    fixture();
    let o = Opts::new(true, true);
    let (state, _data) = build(&o);
    let app = xerj_api::router::build_es_compat_router(state);
    // Port 0: the OS picks a free one. Never 9200, never the reference nodes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    assert_ne!(addr.port(), 9200);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    // Public, no key, over the wire.
    let (status, headers, body) = raw_get(addr, "/_hub/v1/catalogue", &[]).await;
    assert_eq!(status, 200);
    assert!(headers
        .iter()
        .any(|(k, v)| k == "cache-control" && v == "public, max-age=3600"));
    assert!(body.contains("\"corpora\""));
    // …and the rest of the node is still closed.
    let (status, _, _) = raw_get(addr, "/_cluster/health", &[]).await;
    assert_eq!(status, 401);

    // 59 more are admitted, the 61st overall is not — and a forged
    // X-Forwarded-For does not help, because the peer (127.0.0.1) is not a
    // declared proxy.
    for _ in 0..59 {
        let (status, _, _) = raw_get(addr, "/_hub/v1/catalogue", &[]).await;
        assert_eq!(status, 200);
    }
    let (status, headers, _) = raw_get(
        addr,
        "/_hub/v1/catalogue",
        &[("X-Forwarded-For", "198.51.100.77")],
    )
    .await;
    assert_eq!(status, 429);
    assert!(headers.iter().any(|(k, _)| k == "retry-after"));
}

// ─────────────────────────────────────────────────────────────────────────────
// What stands behind the routes
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_alias_cannot_lead_a_hosted_query_out_of_the_hub_namespace() {
    let h = hub(Opts::new(false, true)).await;
    // The alias is real: with the key, the ordinary API reads the private index
    // through it. (Without this the assertion below proves nothing.)
    let r = h
        .send(
            Hub::req("POST", "/xc-leaky-b1-defs/_search")
                .header(header::AUTHORIZATION, format!("ApiKey {ADMIN_KEY}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"query": {"term": {"symbol": "SECRET"}}}).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert!(
        String::from_utf8_lossy(&r.body).contains("hunter2"),
        "fixture sanity: the alias resolves to the private index: {:?}",
        r.json()
    );

    // Through the hosted route the same name finds nothing.
    let r = h.get_keyed("/_hub/v1/def/leaky/SECRET").await;
    assert!(
        !String::from_utf8_lossy(&r.body).contains("hunter2"),
        "the private index leaked through an alias: {:?}",
        r.json()
    );
    let a = r.answer();
    assert!(a.hits.is_empty(), "{a:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backend_failure_is_a_fixed_503_that_names_nothing() {
    let h = hub(Opts::new(false, false)).await;
    for path in ["/_hub/v1/def/badprefix/X", "/_hub/v1/code/badprefix?q=x"] {
        let r = h.get(path).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {:?}",
            r.json()
        );
        assert_eq!(r.json()["error"]["type"], json!("hub_unavailable"));
        let text = String::from_utf8_lossy(&r.body).to_string();
        for leaked in ["bad prefix", "refused", "x=1", "xc-", "HTTP", "state"] {
            assert!(!text.contains(leaked), "{path} leaked {leaked:?}: {text}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_house_is_a_503_with_retry_after_and_recovers() {
    fixture();
    let (state, _data) = build(&Opts::new(false, false));
    // The routes alone, with a handle on the shared state, so the test can hold
    // every query slot the way sixteen slow queries would.
    let shared = hub_api::HubShared::new(state);
    let app: axum::Router = hub_api::routes(shared.clone());
    let held = shared.hold_slots_for_test(hub_api::MAX_CONCURRENT_QUERIES);
    assert_eq!(held.len(), hub_api::MAX_CONCURRENT_QUERIES);

    let get = |app: axum::Router, path: &'static str| async move {
        app.oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    };
    let r = get(app.clone(), "/_hub/v1/def/mini-c/MINI_LIMIT").await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.headers().get("retry-after").unwrap(), "1");
    // The catalogue needs no engine query, so it is not held up.
    assert_eq!(
        get(app.clone(), "/_hub/v1/catalogue").await.status(),
        StatusCode::OK
    );

    drop(held);
    let r = get(app, "/_hub/v1/def/mini-c/MINI_LIMIT").await;
    // Free again: the query runs (the node has no indices, so the corpus is
    // known to the ledger but not loaded — a 404, not a 503).
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

// ─────────────────────────────────────────────────────────────────────────────
// Responses too large for the in-process cap
// ─────────────────────────────────────────────────────────────────────────────

/// The hub's routes alone over `state`, with the in-process limits lowered.
fn lowered(
    state: &AppState,
    response_cap_bytes: usize,
    fields_failure_ttl: Duration,
) -> (axum::Router, std::sync::Arc<hub_api::HubShared>) {
    let shared = hub_api::HubShared::with_tuning(
        state.clone(),
        hub_api::Tuning {
            response_cap_bytes,
            fields_failure_ttl,
            ..Default::default()
        },
    );
    (hub_api::routes(shared.clone()), shared)
}

async fn get_on(app: &axum::Router, path: &str) -> Reply {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    Reply {
        status,
        headers,
        body,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_wide_mapping_over_the_cap_is_a_503_not_an_empty_search() {
    let h = hub(Opts::new(true, true)).await;
    // Premise: with the production cap the same node searches fine.
    let ok = h.stranger("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(ok.status, StatusCode::OK);
    assert!(!ok.json()["corpora"].as_array().unwrap().is_empty());

    // What a pack with enormous mappings does at 16 MiB, in miniature: the
    // node-wide `xc-*` mapping is one byte too large for the cap on a single
    // in-process response (and everything else on the node still fits).
    let mapping = h.admin("GET", "/xc-*/_mapping", None).await;
    let cap = mapping.body.len() - 1;
    assert!(cap > 2048, "fixture sanity: a {cap}-byte mapping");
    let ttl = Duration::from_millis(600);
    let (app, shared) = lowered(&h.state, cap, ttl);

    let r = get_on(&app, "/_hub/v1/search?q=frobnicate").await;
    assert_eq!(
        r.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an unreadable mapping is an outage, not 'no results': {:?}",
        r.json()
    );
    assert_eq!(r.json()["error"]["type"], json!("hub_unavailable"));
    let text = String::from_utf8_lossy(&r.body).to_string();
    for leaked in [cap.to_string().as_str(), "bytes", "mapping", "xc-", "over"] {
        assert!(!text.contains(leaked), "leaked {leaked:?}: {text}");
    }
    assert_ne!(r.json(), json!({"corpora": []}));

    // The failure is remembered: a burst of requests does not repeat the
    // expensive read (each repeat costs seconds at the real size).
    for _ in 0..6 {
        let r = get_on(&app, "/_hub/v1/search?q=frobnicate").await;
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(
        shared.mapping_reads_for_test(),
        1,
        "seven requests, one read of the mappings"
    );

    // …but not for long: a transient fault clears itself, and the read is
    // tried again once the memory expires.
    tokio::time::sleep(ttl + Duration::from_millis(300)).await;
    let r = get_on(&app, "/_hub/v1/search?q=frobnicate").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        shared.mapping_reads_for_test(),
        2,
        "read again after the ttl"
    );

    // Routes about one corpus do not read the node-wide mapping and are not
    // taken down with `search`.
    let r = get_on(&app, "/_hub/v1/def/mini-c/MINI_LIMIT").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn records_too_big_to_return_together_shrink_the_window_and_say_so() {
    let h = hub(Opts::new(false, false)).await;
    // Two records of ~48 KiB that both match; `code` asks the engine for whole
    // records, ten of them at once even for k=1.
    let filler = "int x; ".repeat(48 * 1024 / 7);
    h.create("xc-big-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-big-b1-docs",
        &[
            json!({"ax_path": "big/a.c", "title": "a.c",
                   "body": format!("needleword alpha {filler}"), "defs": "function needleword"}),
            json!({"ax_path": "big/b.c", "title": "b.c",
                   "body": format!("needleword beta {filler}"), "defs": "function needleword"}),
        ],
    )
    .await;
    h.admin("POST", "/xc-big-b1-docs/_refresh", None).await;

    // Premise: with the production cap both fit, and nothing is cut.
    let whole = h.get("/_hub/v1/code/big?q=needleword&k=1").await;
    assert_eq!(whole.status, StatusCode::OK, "{:?}", whole.json());
    let whole = whole.answer();
    assert_eq!(whole.hits.len(), 1);
    assert!(!whole.truncated, "{:?}", whole.warnings);
    assert!(whole.warnings.is_empty(), "{:?}", whole.warnings);

    // With the cap below two records (what two 9 MiB records do to 16 MiB), the
    // answer is built from fewer candidates and says so — not a 503.
    let (app, _shared) = lowered(&h.state, 64 * 1024, Duration::from_secs(15));
    for k in [1, 2, 5] {
        let r = get_on(&app, &format!("/_hub/v1/code/big?q=needleword&k={k}")).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "k={k}: records that do not fit together must not be a 503: {:?}",
            r.json()
        );
        let a = r.answer();
        assert_eq!(a.corpus, "big");
        assert_eq!(a.hits.len(), 1, "k={k}: the one record that fits");
        assert!(
            a.hits[0].file == "big/a.c" || a.hits[0].file == "big/b.c",
            "{:?}",
            a.hits[0]
        );
        assert!(a.hits[0].score > 0.0);
        assert!(a.truncated, "k={k}: the answer is cut and says so");
        assert!(
            a.warnings.iter().any(|w| w.contains("too large")),
            "k={k}: {:?}",
            a.warnings
        );
    }

    // A single record over the cap cannot be returned at all, and that is a
    // fixed 503 rather than a made-up answer.
    let (tiny, _shared) = lowered(&h.state, 8 * 1024, Duration::from_secs(15));
    let r = get_on(&tiny, "/_hub/v1/code/big?q=needleword&k=1").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{:?}", r.json());
    assert_eq!(r.json()["error"]["type"], json!("hub_unavailable"));
}

// ─────────────────────────────────────────────────────────────────────────────
// What a pack's own ledger cannot talk the node into
// ─────────────────────────────────────────────────────────────────────────────

/// Text that lives only in somebody else's indices: the excluded corpus, an
/// index no catalogue entry lists, and the private index behind an alias.
const OTHER_PEOPLES_DATA: [&str; 6] = [
    "closed text",
    "orphan text",
    "CLOSED_LIMIT",
    "mini-closed/",
    "orphan/",
    "hunter2",
];

fn assert_no_leak(what: &str, r: &Reply) {
    let text = String::from_utf8_lossy(&r.body);
    for needle in OTHER_PEOPLES_DATA {
        assert!(
            !text.contains(needle),
            "{what} leaked {needle:?} (status {}): {text}",
            r.status
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ledger_prefix_that_is_not_the_corpus_own_build_is_never_queried() {
    // A public node: the stranger is exactly who this guards against.
    let h = hub(Opts::new(true, true)).await;

    // Premises. A well-formed corpus answers; and the data a hostile prefix
    // would reach is really on this node (without these the test proves nothing).
    let ok = h.stranger("/_hub/v1/code/mini-c?q=frobnicate&k=20").await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json());
    assert!(!ok.answer().hits.is_empty());
    for (index, needle) in [
        ("xc-mini-closed-b1-docs", "closed text"),
        ("xc-orphan-b1-docs", "orphan text"),
        ("xc-mini-closed-b1-defs", "CLOSED_LIMIT"),
    ] {
        let r = h
            .admin(
                "POST",
                &format!("/{index}/_search"),
                Some(&json!({"query": {"match_all": {}}})),
            )
            .await;
        assert!(
            String::from_utf8_lossy(&r.body).contains(needle),
            "fixture sanity: {index} holds {needle:?}"
        );
    }

    // Every way a ledger can name a prefix that is not `xc-<its own slug>-b<n>`.
    // Each is refused before any query, with the same fixed 503, and none of
    // them reads another corpus.
    for slug in hostile_slugs() {
        for path in [
            format!("/_hub/v1/code/{slug}?q=frobnicate&k=20"),
            format!("/_hub/v1/code/{slug}?q=closed"),
            format!("/_hub/v1/code/{slug}?q=orphan"),
            format!("/_hub/v1/code/{slug}?q=frobnicate&mode=hybrid"),
            format!("/_hub/v1/def/{slug}/CLOSED_LIMIT"),
            format!("/_hub/v1/def/{slug}/MINI_LIMIT"),
        ] {
            let r = h.stranger(&path).await;
            assert_no_leak(&path, &r);
            assert_eq!(
                r.status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{path}: {:?}",
                r.json()
            );
            assert_eq!(
                r.json()["error"]["type"],
                json!("hub_unavailable"),
                "{path}"
            );
            let text = String::from_utf8_lossy(&r.body).to_string();
            for leaked in ["xc-", "prefix", "ledger", "state", slug] {
                assert!(!text.contains(leaked), "{path} said {leaked:?}: {text}");
            }
        }
    }

    // `search` does not list them either, and still lists the honest corpora.
    let r = h.stranger("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_no_leak("search", &r);
    let slugs: Vec<String> = r.json()["corpora"]
        .as_array()
        .expect("corpora")
        .iter()
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(slugs.contains(&"mini-c".to_string()), "{slugs:?}");
    for slug in hostile_slugs() {
        assert!(!slugs.contains(&slug.to_string()), "{slug} in {slugs:?}");
    }
}

/// While armed, [`rewrite_flip_ledger`] does what a `xerj hub pull` landing at
/// the worst moment would.
static FLIP_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Replace the `flip` corpus's ledger with one naming the whole namespace. Run
/// by the hub right after the ledger passed its check and before the shared
/// query pipeline reads it a second time.
fn rewrite_flip_ledger(root: &std::path::Path, slug: &str) {
    if slug == "flip" && FLIP_ARMED.load(Ordering::SeqCst) {
        write_hubpack_state(
            root,
            slug,
            "http://x",
            "xc-",
            PIN_SHA,
            PIN_DATE,
            "MIT",
            "adapt-with-attribution",
        )
        .expect("rewrite the ledger");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ledger_that_changes_after_it_was_checked_still_cannot_leave_its_build() {
    // The `def` and `code` pipelines read the ledger themselves, a second time,
    // to decide what to query. Checking the first read is not enough if the
    // second can differ (a `xerj hub pull` landing mid-request).
    let h = hub(Opts::new(true, true)).await;
    h.create("xc-flip-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-flip-b1-docs",
        &[json!({"ax_path": "flip/f.c", "body": "frobnicate flip text"})],
    )
    .await;
    h.admin("POST", "/xc-flip-b1-docs/_refresh", None).await;
    let shared = hub_api::HubShared::with_tuning(
        h.state.clone(),
        hub_api::Tuning {
            after_ledger_check: Some(rewrite_flip_ledger),
            ..Default::default()
        },
    );
    let app: axum::Router = hub_api::routes(shared);
    let ledger = std::path::PathBuf::from(std::env::var("XERJ_CODE_HOME").unwrap())
        .join("state")
        .join("flip.json");

    // Premise: with the ledger as written, the corpus answers from its own data.
    FLIP_ARMED.store(false, Ordering::SeqCst);
    let r = get_on(&app, "/_hub/v1/code/flip?q=frobnicate&k=20").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let hits = r.answer().hits;
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|x| x.file.starts_with("flip/")),
        "{hits:#?}"
    );

    // Armed: the ledger names `xc-` by the time the pipeline reads it. Without
    // the pin that is every hub index; with it, a refusal.
    FLIP_ARMED.store(true, Ordering::SeqCst);
    for path in [
        "/_hub/v1/code/flip?q=frobnicate&k=20",
        "/_hub/v1/code/flip?q=closed",
        "/_hub/v1/def/flip/CLOSED_LIMIT",
    ] {
        let r = get_on(&app, path).await;
        assert_no_leak(path, &r);
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {:?}",
            r.json()
        );
        assert_eq!(r.json()["error"]["type"], json!("hub_unavailable"));
    }
    let on_disk = std::fs::read_to_string(&ledger).expect("ledger");
    assert!(
        on_disk.contains("\"index_prefix\":\"xc-\""),
        "premise: the ledger really was replaced mid-request: {on_disk}"
    );

    // Put it back for anything that runs after this.
    FLIP_ARMED.store(false, Ordering::SeqCst);
    write_hubpack_state(
        ledger.parent().and_then(|p| p.parent()).unwrap(),
        "flip",
        "http://x",
        "xc-flip-b1",
        PIN_SHA,
        PIN_DATE,
        "MIT",
        "adapt-with-attribution",
    )
    .expect("restore the ledger");
}

#[test]
fn only_the_exact_build_prefix_shape_is_accepted_for_a_ledger() {
    // `xc-<slug>-b<digits>` for the corpus's OWN slug, in every accepted shape:
    // digits of any length, slugs containing `-`, `.`, `_` and `+`.
    for (slug, prefix) in [
        ("mini-c", "xc-mini-c-b1"),
        ("mini-c", "xc-mini-c-b20260921"),
        ("mini-gpl", "xc-mini-gpl-b2"),
        ("rust-vulns", "xc-rust-vulns-b1700000000"),
        ("c++", "xc-c++-b3"),
        ("a.b_c", "xc-a.b_c-b0"),
        // A slug that itself ends in a build-like token is still that slug.
        ("lib-b2", "xc-lib-b2-b7"),
    ] {
        assert!(
            hub_api::valid_build_prefix(prefix, slug),
            "{prefix} for {slug}"
        );
    }
    for (slug, prefix) in [
        ("mini-c", "xc-"),
        ("mini-c", "xc-*"),
        ("mini-c", "xc-mini-c"),
        ("mini-c", "xc-mini-c-b"),
        ("mini-c", "xc-mini-c-b1x"),
        ("mini-c", "xc-mini-c-bx1"),
        ("mini-c", "xc-mini-c-b1-defs"),
        ("mini-c", "xc-mini-c-b1*"),
        ("mini-c", "xc-mini-c-b1,xc-mini-closed-b1"),
        ("mini-c", "xc-mini-c-b1 "),
        ("mini-c", " xc-mini-c-b1"),
        ("mini-c", "XC-mini-c-b1"),
        ("mini-c", "xc-MINI-C-b1"),
        ("mini-c", "xc-mini-gpl-b2"),
        ("mini-c", "xc-mini-closed-b1"),
        ("mini-c", "xc-mini-c-b-1"),
        ("mini-c", "xc-mini-c-b+1"),
        ("mini-c", "xc-mini-c-b١"),
        ("mini-c", ""),
        ("mini-c", "mini-c-b1"),
        ("mini", "xc-mini-c-b1"),
    ] {
        assert!(
            !hub_api::valid_build_prefix(prefix, slug),
            "{prefix:?} must not be accepted for {slug}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_key_sees_the_hub_through_its_own_grants_only() {
    // A keyed, non-public node: callers are principals with grants, and the hub
    // routes must not widen what a grant allows.
    let h = hub(Opts::new(false, true)).await;

    // A tenant key with nothing under `xc-*`.
    let tenant = h.mint_read_key("tenant-a", &["tenant-a-*"]).await;
    let direct = h.direct_search_with_key("xc-mini-c-b1-docs", &tenant).await;
    assert_eq!(
        direct.status,
        StatusCode::FORBIDDEN,
        "premise: the ordinary API refuses this key the corpus: {:?}",
        direct.json()
    );
    let r = h
        .stranger_with_key("/_hub/v1/code/mini-c?q=frobnicate", &tenant)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.json());
    assert_eq!(r.json()["error"]["type"], json!("corpus_not_loaded"));
    let r = h
        .stranger_with_key("/_hub/v1/def/mini-c/MINI_LIMIT", &tenant)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.json());
    assert_eq!(r.json()["error"]["type"], json!("corpus_not_loaded"));
    let r = h
        .stranger_with_key("/_hub/v1/search?q=frobnicate", &tenant)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(
        r.json(),
        json!({"corpora": []}),
        "the key sees none of the hub, so `search` finds none of it"
    );
    for text in ["mini-c", "frobnicate the buffer", "MINI_LIMIT", "128"] {
        assert!(
            !String::from_utf8_lossy(&r.body).contains(text),
            "{text} reached a key that holds nothing under xc-*"
        );
    }

    // A key granted ONE corpus sees that corpus through the hub, and no other:
    // the hub narrows to its own namespace, it does not widen to it.
    let mini = h.mint_read_key("mini-reader", &["xc-mini-c-*"]).await;
    let ok = h.direct_search_with_key("xc-mini-c-b1-docs", &mini).await;
    assert!(ok.status.is_success(), "premise: granted: {:?}", ok.json());
    let no = h.direct_search_with_key("xc-mini-gpl-b2-docs", &mini).await;
    assert_eq!(no.status, StatusCode::FORBIDDEN, "premise: not granted");

    let r = h
        .stranger_with_key("/_hub/v1/code/mini-c?q=mini_hash+unsigned", &mini)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert!(!r.answer().hits.is_empty(), "its own corpus is answered");
    let r = h
        .stranger_with_key("/_hub/v1/def/mini-c/MINI_LIMIT", &mini)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    assert_eq!(r.answer().hits.len(), 1);
    for path in [
        "/_hub/v1/code/mini-gpl?q=frobnicate",
        "/_hub/v1/def/mini-gpl/GPL_LIMIT",
    ] {
        let r = h.stranger_with_key(path, &mini).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {:?}", r.json());
        assert_eq!(r.json()["error"]["type"], json!("corpus_not_loaded"));
    }
    let r = h
        .stranger_with_key("/_hub/v1/search?q=frobnicate", &mini)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
    let slugs: Vec<String> = r.json()["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(slugs, ["mini-c"], "only the granted corpus is searched");

    // The admin key is unrestricted, and it is the same hub: both corpora.
    let r = h.get_keyed("/_hub/v1/search?q=frobnicate").await;
    let all: Vec<String> = r.json()["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(
        all.contains(&"mini-c".to_string()) && all.contains(&"mini-gpl".to_string()),
        "{all:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_caller_neither_reads_nor_poisons_the_shared_field_list() {
    // `search` learns which of body/defs/symbol_text exist from the node's
    // mappings and reuses that for a minute. A caller who sees only part of the
    // node reads a different list; it must not be shared in either direction.
    async fn node() -> (Hub, String) {
        let h = hub(Opts::new(false, true)).await;
        h.create(
            "xc-bodyonly-b1-docs",
            json!({"properties": {"body": {"type": "text"}, "ax_path": {"type": "keyword"}}}),
        )
        .await;
        h.bulk(
            "xc-bodyonly-b1-docs",
            &[json!({"ax_path": "bodyonly/b.c", "body": "frobnicate quasar nebula"})],
        )
        .await;
        h.admin("POST", "/xc-bodyonly-b1-docs/_refresh", None).await;
        let key = h.mint_read_key("bodyonly", &["xc-bodyonly-*"]).await;
        (h, key)
    }
    let slugs = |r: &Reply| -> Vec<String> {
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
        r.json()["corpora"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["slug"].as_str().unwrap().to_string())
            .collect()
    };

    // The operator searches first, so the shared list names all three fields; the
    // scoped key (whose only index maps `body`) must not borrow it.
    let (h, key) = node().await;
    assert!(slugs(&h.get_keyed("/_hub/v1/search?q=frobnicate").await).contains(&"mini-c".into()));
    let r = h
        .stranger_with_key("/_hub/v1/search?q=frobnicate+quasar", &key)
        .await;
    assert_eq!(
        slugs(&r),
        ["bodyonly"],
        "the scoped key finds its own corpus"
    );

    // The scoped key searches first; the operator must not inherit its view,
    // where only `body` exists: a definition name lives in `symbol_text`.
    let (h, key) = node().await;
    let r = h
        .stranger_with_key("/_hub/v1/search?q=frobnicate+quasar", &key)
        .await;
    assert_eq!(slugs(&r), ["bodyonly"]);
    let admin = slugs(&h.get_keyed("/_hub/v1/search?q=MINI_LIMIT").await);
    assert_eq!(
        admin.first().map(String::as_str),
        Some("mini-c"),
        "a definition is found through `symbol_text`: {admin:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stricter_of_the_catalogue_and_the_ledger_governs_every_route() {
    let h = hub(Opts::new(false, false)).await;
    let note = |spdx: &str| format!("approach-only licence ({spdx}): read, do not copy");

    // Premises: these corpora are really indexed, so an absence below is the
    // licence rule at work and not an empty fixture.
    for slug in ["ledger-closed", "cat-closed", "gplish", "ledger-gpl"] {
        let r = h
            .admin(
                "POST",
                &format!("/xc-{slug}-b1-docs/_search"),
                Some(&json!({"query": {"match": {"body": "frobnicate"}}})),
            )
            .await;
        assert!(
            String::from_utf8_lossy(&r.body).contains(&format!("{slug} text")),
            "fixture sanity: {slug} is indexed"
        );
    }

    // Either side can refuse: catalogue-servable but ledger-excluded, and the
    // other way round. Same 404 everywhere, and `search` never returns a hit.
    for slug in ["ledger-closed", "cat-closed"] {
        for path in [
            format!("/_hub/v1/def/{slug}/X"),
            format!("/_hub/v1/code/{slug}?q=frobnicate"),
        ] {
            let r = h.get(&path).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                r.json()["error"]["type"],
                json!("resource_not_found_exception"),
                "{path}: refusals look alike"
            );
        }
    }
    for q in ["frobnicate", "ledger-closed", "cat-closed"] {
        let r = h.get(&format!("/_hub/v1/search?q={q}&k=10")).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());
        let text = String::from_utf8_lossy(&r.body).to_string();
        assert!(
            !text.contains("ledger-closed") && !text.contains("cat-closed"),
            "search q={q} returned a refused corpus: {text}"
        );
        assert!(
            !text.contains("ledger-closed text") && !text.contains("cat-closed text"),
            "search q={q}: {text}"
        );
    }

    // Approach-only on EITHER side is approach-only: the header says so, the
    // licence it names is the one that decided, and every hit carries the note.
    for (slug, spdx, symbol) in [
        ("gplish", "GPL-3.0-only", "GPLISH_LIMIT"),
        ("ledger-gpl", "AGPL-3.0-only", "LEDGER_GPL_LIMIT"),
    ] {
        let d = h.get(&format!("/_hub/v1/def/{slug}/{symbol}")).await;
        assert_eq!(d.status, StatusCode::OK, "{:?}", d.json());
        let d = d.answer();
        assert_eq!(d.licence_class, LicenceClass::ApproachOnly, "{slug} def");
        assert_eq!(d.licence_spdx, spdx, "{slug} def");
        assert_eq!(d.hits.len(), 1, "{slug} def");
        assert_eq!(
            d.hits[0].licence_warning.as_deref(),
            Some(note(spdx).as_str())
        );

        let c = h.get(&format!("/_hub/v1/code/{slug}?q=frobnicate")).await;
        assert_eq!(c.status, StatusCode::OK, "{:?}", c.json());
        let c = c.answer();
        assert_eq!(c.licence_class, LicenceClass::ApproachOnly, "{slug} code");
        assert_eq!(c.licence_spdx, spdx, "{slug} code");
        assert!(!c.hits.is_empty(), "{slug} code");
        assert!(
            c.hits
                .iter()
                .all(|x| x.licence_warning.as_deref() == Some(note(spdx).as_str())),
            "{slug} code: {:#?}",
            c.hits
        );

        // `def` and `code` describe the corpus identically (the pin test's rule).
        assert_eq!(
            (&d.corpus, &d.licence_spdx, d.licence_class),
            (&c.corpus, &c.licence_spdx, c.licence_class)
        );

        let r = h.get("/_hub/v1/search?q=frobnicate&k=10").await;
        let rows = r.json()["corpora"].as_array().cloned().unwrap();
        let row = rows
            .iter()
            .find(|row| row["slug"] == slug)
            .unwrap_or_else(|| panic!("{slug} missing from search: {rows:?}"));
        assert_eq!(
            row["best_hit"]["licence_warning"],
            json!(note(spdx)),
            "{slug} search"
        );
    }

    // And the permissive corpora are untouched by all this.
    let c = h
        .get("/_hub/v1/code/mini-c?q=mini_hash+unsigned")
        .await
        .answer();
    assert_eq!(c.licence_class, LicenceClass::AdaptWithAttribution);
    assert!(c.hits.iter().all(|x| x.licence_warning.is_none()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_bad_index_does_not_take_search_down_for_everyone() {
    let h = hub(Opts::new(true, true)).await;
    let slugs_of = |r: &Reply| -> Vec<String> {
        r.json()["corpora"]
            .as_array()
            .unwrap_or_else(|| panic!("no corpora in {:?}", r.json()))
            .iter()
            .map(|c| c["slug"].as_str().unwrap().to_string())
            .collect()
    };
    let baseline = h.stranger("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(baseline.status, StatusCode::OK);
    let good = slugs_of(&baseline);
    assert!(good.contains(&"mini-c".to_string()), "{good:?}");

    // ── A corpus that maps `body` as a number ──────────────────────────────────
    // Its `defs` is ordinary text. A lenient clause skips the numeric field and
    // still searches `defs`, so this corpus is FOUND; a strict one fails the whole
    // index, and the corpus is missing.
    h.create(
        "xc-longbody-b1-docs",
        json!({"properties": {
            "body": {"type": "long"},
            "defs": {"type": "text"},
            "ax_path": {"type": "keyword"}}}),
    )
    .await;
    h.bulk(
        "xc-longbody-b1-docs",
        &[json!({"ax_path": "longbody/n.c", "body": 5, "defs": "function frobnicate numbers"})],
    )
    .await;
    h.admin("POST", "/xc-longbody-b1-docs/_refresh", None).await;
    // Premise: the engine itself refuses a text query against that field when the
    // clause is not lenient, and accepts it when it is — so the hub must ask.
    let strict = h
        .admin(
            "POST",
            "/xc-*/_search",
            Some(&json!({"query": {"multi_match": {
                "query": "frobnicate", "fields": ["body"], "lenient": false}}})),
        )
        .await;
    assert!(
        !strict.status.is_success(),
        "premise: a strict multi_match over a long field fails the fan-out: {:?}",
        strict.json()
    );
    for q in ["frobnicate", "frobnicate+buffer", "mini_clamp"] {
        let r = h.stranger(&format!("/_hub/v1/search?q={q}")).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "q={q} with a long-mapped body somewhere on the node: {:?}",
            r.json()
        );
    }
    let with_long = slugs_of(&h.stranger("/_hub/v1/search?q=frobnicate").await);
    let as_set = |v: &[String]| v.iter().cloned().collect::<std::collections::BTreeSet<_>>();
    let mut want = as_set(&good);
    want.insert("longbody".to_string());
    assert_eq!(
        as_set(&with_long),
        want,
        "the honest corpora are answered exactly as before, and the numeric one is \
         searched through its text field (lenient), not dropped: {with_long:?}"
    );
    // A query that happens to be a number is no problem for the numeric field.
    let r = h.stranger("/_hub/v1/search?q=5").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.json());

    // ── A corpus whose `body` cannot be searched at all ────────────────────────
    // Lenient does not help here (the engine refuses a NAMED unsearchable field),
    // so this is the failure the per-index isolation exists for.
    h.create(
        "xc-noindex-b1-docs",
        json!({"properties": {
            "body": {"type": "text", "index": false},
            "ax_path": {"type": "keyword"}}}),
    )
    .await;
    h.bulk(
        "xc-noindex-b1-docs",
        &[json!({"ax_path": "noindex/n.c", "body": "frobnicate hidden away"})],
    )
    .await;
    h.admin("POST", "/xc-noindex-b1-docs/_refresh", None).await;
    let direct = h
        .admin(
            "POST",
            "/xc-*/_search",
            Some(&json!({"query": {"multi_match": {
                "query": "frobnicate", "fields": ["body"], "lenient": true}}})),
        )
        .await;
    assert!(
        !direct.status.is_success(),
        "premise: even a lenient clause fails the whole fan-out on an unsearchable field: {:?}",
        direct.json()
    );
    let r = h.stranger("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "one corpus it cannot search must not 503 the rest: {:?}",
        r.json()
    );
    let after = slugs_of(&r);
    assert_eq!(
        as_set(&after),
        want,
        "the bad corpus is skipped, the rest are kept: {after:?}"
    );
    assert!(!after.contains(&"noindex".to_string()));

    // The other routes are about one corpus each and are untouched.
    let r = h
        .stranger("/_hub/v1/code/mini-c?q=mini_hash+unsigned")
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.answer().hits.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn code_skips_an_index_it_cannot_search_and_says_so() {
    let h = hub(Opts::new(true, true)).await;
    let unsearchable = json!({"properties": {
        "body": {"type": "text", "index": false},
        "ax_path": {"type": "keyword"}}});

    // `mixed`: one good index and one whose `body` cannot be searched.
    h.create("xc-mixed-b1-docs", docs_mapping()).await;
    h.bulk(
        "xc-mixed-b1-docs",
        &[json!({"ax_path": "mixed/good.c", "body": "frobnicate the good text"})],
    )
    .await;
    h.create("xc-mixed-b1-extra", unsearchable.clone()).await;
    h.bulk(
        "xc-mixed-b1-extra",
        &[json!({"ax_path": "mixed/bad.c", "body": "frobnicate the hidden text"})],
    )
    .await;
    h.admin("POST", "/xc-mixed-b1-docs,xc-mixed-b1-extra/_refresh", None)
        .await;
    let strict = h
        .admin(
            "POST",
            "/xc-mixed-b1*/_search",
            Some(&json!({"query": {"multi_match": {"query": "frobnicate", "fields": ["body"]}}})),
        )
        .await;
    assert!(
        !strict.status.is_success(),
        "premise: one unsearchable index fails the whole direct fan-out: {:?}",
        strict.json()
    );

    let r = h.stranger("/_hub/v1/code/mixed?q=frobnicate").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "one bad index must not be a 503 for its corpus: {:?}",
        r.json()
    );
    let a = r.answer();
    assert!(!a.hits.is_empty());
    assert!(
        a.hits.iter().all(|x| x.file == "mixed/good.c"),
        "the answer is from the index that could be searched: {:#?}",
        a.hits
    );
    assert!(
        a.warnings
            .iter()
            .any(|w| w.starts_with("1 of this corpus's indices could not be searched")),
        "a partial answer says so: {:?}",
        a.warnings
    );
    // A corpus whose indices all search fine carries no such warning.
    let ok = h
        .stranger("/_hub/v1/code/mini-c?q=mini_hash+unsigned")
        .await
        .answer();
    assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);

    // `allbad`: nothing could be searched, so there is no answer to give.
    h.create("xc-allbad-b1-docs", unsearchable).await;
    h.bulk(
        "xc-allbad-b1-docs",
        &[json!({"ax_path": "allbad/b.c", "body": "frobnicate hidden"})],
    )
    .await;
    h.admin("POST", "/xc-allbad-b1-docs/_refresh", None).await;
    let r = h.stranger("/_hub/v1/code/allbad?q=frobnicate").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{:?}", r.json());
    assert_eq!(r.json()["error"]["type"], json!("hub_unavailable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_every_searched_index_fails_search_says_so_instead_of_an_empty_answer() {
    // Nothing but an unsearchable corpus on the node: "no results" would be a lie.
    let h = bare(&Opts::new(true, true));
    h.create(
        "xc-noindex-b1-docs",
        json!({"properties": {
            "body": {"type": "text", "index": false},
            "ax_path": {"type": "keyword"}}}),
    )
    .await;
    h.bulk(
        "xc-noindex-b1-docs",
        &[json!({"ax_path": "noindex/n.c", "body": "frobnicate hidden away"})],
    )
    .await;
    h.admin("POST", "/xc-noindex-b1-docs/_refresh", None).await;
    let r = h.stranger("/_hub/v1/search?q=frobnicate").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{:?}", r.json());
    assert_eq!(r.json()["error"]["type"], json!("hub_unavailable"));
    let text = String::from_utf8_lossy(&r.body).to_string();
    for leaked in ["noindex", "body", "unsearchable", "xc-", "HTTP"] {
        assert!(!text.contains(leaked), "leaked {leaked:?}: {text}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure helpers
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn only_the_four_exact_shapes_are_public() {
    for yes in [
        "/_hub/v1/catalogue",
        "/_hub/v1/search",
        "/_hub/v1/code/lz4",
        "/_hub/v1/def/lz4/LZ4_VERSION_RELEASE",
        "/_hub/v1/def/lz4/a%2Fb",
    ] {
        assert!(hub_api::is_public_path(yes), "{yes}");
    }
    for no in [
        "",
        "/",
        "/_hub",
        "/_hub/",
        "/_hub/v1",
        "/_hub/v1/",
        "/_hub/v1/catalogue/",
        "/_hub/v1/catalogue/x",
        "/_hub/v1/search/",
        "/_hub/v1/code",
        "/_hub/v1/code/",
        "/_hub/v1/code/lz4/",
        "/_hub/v1/code/lz4/x",
        "/_hub/v1/def",
        "/_hub/v1/def/lz4",
        "/_hub/v1/def/lz4/",
        "/_hub/v1/def//X",
        "/_hub/v1/def/lz4/X/",
        "/_hub/v1/def/lz4/X/Y",
        "/_hub/v1//catalogue",
        "/_hub/v1/Catalogue",
        "/_Hub/v1/catalogue",
        "/_hub/v2/catalogue",
        "_hub/v1/catalogue",
        "//_hub/v1/catalogue",
        "/a/_hub/v1/catalogue",
        "/_hub/v1/../catalogue",
    ] {
        assert!(!hub_api::is_public_path(no), "{no:?}");
    }
}

#[test]
fn an_index_maps_back_to_its_corpus_by_the_stamped_build_rule() {
    let known: std::collections::HashMap<String, ()> = ["lz4", "mini-c", "a", "a-b1", "rust-vulns"]
        .iter()
        .map(|s| (s.to_string(), ()))
        .collect();
    let slug = |i: &str| hub_api::slug_of_index(i, &known);
    assert_eq!(slug("xc-lz4-b1-defs"), Some("lz4"));
    assert_eq!(slug("xc-lz4-b1-docs"), Some("lz4"));
    assert_eq!(slug("xc-mini-c-b17-docs"), Some("mini-c"));
    assert_eq!(slug("xc-rust-vulns-b1700000000-defs"), Some("rust-vulns"));
    assert_eq!(slug("xc-lz4-b1"), Some("lz4"), "the stamped prefix itself");
    assert_eq!(
        slug("xc-lz4-b1-tests-b2-x-docs"),
        Some("lz4"),
        "a dataset named like a build"
    );
    // Not stamped builds: legacy `xc-<name>-<dataset>`, a non-numeric stamp,
    // digits glued to letters, no stamp.
    assert_eq!(slug("xc-lz4-docs"), None);
    assert_eq!(slug("xc-lz4-bx-docs"), None);
    assert_eq!(slug("xc-lz4-b1x-docs"), None);
    assert_eq!(slug("xc-lz4-b-docs"), None);
    // Not hub names, or not listed.
    assert_eq!(slug("lz4-b1-docs"), None);
    assert_eq!(slug("xc-other-b1-docs"), None);
    assert_eq!(slug("secret-notes"), None);
    // `a` and `a-b1` both fit `xc-a-b1-b2-docs`: the shortest wins, always.
    assert_eq!(slug("xc-a-b1-b2-docs"), Some("a"));
}

#[test]
fn the_token_budget_cuts_in_rank_order_and_says_what_it_did() {
    let hit = |text: &str, score: f32| Hit {
        file: "f".into(),
        line: 1,
        kind: "passage".into(),
        text: text.into(),
        value: None,
        score,
        licence_warning: None,
    };
    // 3 tokens = 12 characters.
    let mut hits = vec![
        hit("aaaaaaaa", 3.0),
        hit("bbbbbbbb", 2.0),
        hit("cccccccc", 1.0),
    ];
    let mut warnings = vec![];
    assert!(hub_api::apply_token_budget(&mut hits, 3, &mut warnings));
    let texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(
        texts,
        ["aaaaaaaa", "bbbb"],
        "the second is cut to what is left, the third dropped"
    );
    assert!(
        warnings[0].contains("max_tokens=3") && warnings[0].contains("1 lower-ranked"),
        "{warnings:?}"
    );

    // It fits: untouched, no warning.
    let mut hits = vec![hit("aaaa", 1.0), hit("bbbb", 1.0)];
    let mut warnings = vec![];
    assert!(!hub_api::apply_token_budget(&mut hits, 2, &mut warnings));
    assert_eq!(hits.len(), 2);
    assert!(warnings.is_empty());

    // Characters, not bytes.
    let mut hits = vec![hit("ééééééééé", 1.0)];
    assert!(hub_api::apply_token_budget(&mut hits, 1, &mut vec![]));
    assert_eq!(hits[0].text, "éééé");

    assert_eq!(hub_api::passage_width(None), 800);
    assert_eq!(hub_api::passage_width(Some(10)), 40);
    assert_eq!(
        hub_api::passage_width(Some(8192)),
        800,
        "never wider than the CLI's default"
    );
}

#[test]
fn the_search_field_gate_keeps_only_fields_some_index_maps() {
    let mapping = json!({
        "xc-a-b1-defs": {"mappings": {"properties": {"symbol_text": {"type": "text"}, "symbol": {"type": "keyword"}}}},
        "xc-a-b1-docs": {"mappings": {"properties": {"body": {"type": "text"}}}},
    });
    assert_eq!(
        hub_api::mapped_search_fields(&mapping),
        vec!["body", "symbol_text"]
    );
    assert!(hub_api::mapped_search_fields(&json!({})).is_empty());
    assert!(hub_api::mapped_search_fields(&json!(null)).is_empty());
}

#[test]
fn slugs_are_plain_names() {
    let (l64, l65) = ("a".repeat(64), "a".repeat(65));
    for ok in ["lz4", "rust-vulns", "c++", "a.b_c", "0day", l64.as_str()] {
        assert!(hub_api::valid_slug(ok), "{ok}");
    }
    for bad in [
        "",
        "-x",
        ".x",
        "_x",
        "a/b",
        "a b",
        "a*",
        "a\0",
        "a?b",
        "a%2f",
        "é",
        l65.as_str(),
    ] {
        assert!(!hub_api::valid_slug(bad), "{bad:?}");
    }
}

#[test]
fn if_none_match_handles_the_forms_a_cache_sends() {
    use axum::http::HeaderValue;
    let m =
        |h: &str, etag: &str| hub_api::etag_matches(Some(&HeaderValue::from_str(h).unwrap()), etag);
    assert!(m("\"abc\"", "\"abc\""));
    assert!(m("W/\"abc\"", "\"abc\""));
    assert!(m("\"x\", \"abc\"", "\"abc\""));
    assert!(m("*", "\"abc\""));
    assert!(!m("\"abd\"", "\"abc\""));
    assert!(!m("abc", "\"abc\""), "an unquoted tag is not a validator");
    assert!(!hub_api::etag_matches(None, "\"abc\""));
}

#[test]
fn the_fixture_dir_is_the_shared_one() {
    // Guards the premise of the whole file: one fixture, created once.
    let a = fixture().dir.clone();
    let b = fixture().dir.clone();
    assert_eq!(a, b);
    assert_eq!(
        std::env::var("XERJ_CODE_HOME").unwrap(),
        a.join("code-home").to_string_lossy()
    );
}

/// `Ask` hands its answer flow an empty directory with a `.git` marker as the
/// `cwd`/`hub_home`. That directory sits under the data directory, where every
/// index's store is `<data_dir>/<index name>` — so it must carry a name no
/// index can take. It used to be `hub-routing`, a legal index name: an `Ask`
/// wrote `.git` into a user's `hub-routing` index on every request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_never_writes_into_a_user_index_named_like_its_routing_dir() {
    let h = hub(Opts::new(false, false)).await;
    let data = PathBuf::from(&h.state.config.server.data_dir);
    // The old name, and the shape of the new one: an index may take the first,
    // never the second.
    h.create("hub-routing", docs_mapping()).await;
    let r = h
        .admin("PUT", &format!("/{}", hub_api::ROUTING_DIR), None)
        .await;
    assert!(
        !r.status.is_success(),
        "{} must not be a legal index name: {:?}",
        hub_api::ROUTING_DIR,
        r.json()
    );

    let shared = hub_api::HubShared::new(h.state.clone());
    let ask = || hub_api::HubAsk {
        question: "what is the value of MAX_LEN".to_string(),
        corpus: Some("mini-c".to_string()),
        mode: None,
        max_tokens: None,
    };
    let _ = hub_api::ask_answer(&shared, ask(), None).await;

    let user = data.join("hub-routing");
    assert!(user.is_dir(), "the user's index is still there");
    assert!(
        !user.join(".git").exists(),
        "an Ask wrote a .git marker into the user's hub-routing index"
    );
    let marker = data.join(hub_api::ROUTING_DIR).join(".git");
    assert!(marker.is_dir(), "the routing dir is made on the first Ask");

    // Made once, not on every request: a second Ask does not put it back.
    std::fs::remove_dir(&marker).expect("remove marker");
    let _ = hub_api::ask_answer(&shared, ask(), None).await;
    assert!(!marker.exists(), "the routing dir is made once per node");
    assert!(!user.join(".git").exists());

    // And the user's index still answers.
    let r = h.admin("GET", "/hub-routing/_count", None).await;
    assert!(r.status.is_success(), "{:?}", r.json());
}
