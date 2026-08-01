//! The four proven bypasses of per-brain authorization, fired at a **real
//! running node** over a real socket — the way they were found.
//!
//! Every case below was measured against a live server on the first cut of
//! issue #79's fix and came back `200 OK` with another tenant's data. They are
//! not hypotheses, so they are pinned here as regressions rather than argued
//! about. The common cause was one sentence long: authorization was decided
//! against the index named in the URL **path**, while these handlers take the
//! index they actually operate on from the request **body**, where the path
//! index is only a default the body overrides.
//!
//! | # | Verb | How the body overrode the path |
//! |---|---|---|
//! | 1 | `_msearch` | an NDJSON header line's `index` |
//! | 2 | `_bulk` | an action line's `_index` (forge **and** delete) |
//! | 3 | `_mget` | `docs[]._index` |
//! | 4 | `_aliases` | an alias pointed at the victim, then read through |
//!
//! This runs over a TCP listener rather than `tower::ServiceExt::oneshot`
//! deliberately. `oneshot` calls the router directly; the bypasses live in the
//! interaction between middleware, body buffering and handlers, and a body
//! that has to survive being read once for authorization and again by the
//! handler only behaves the same over a real connection. The client below is
//! deliberately dumb (write bytes, read bytes) so nothing in it can smooth over
//! a difference.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use xerj_api::{
    router::{build_es_compat_router, build_native_router},
    state::AppState,
};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_engine::Engine;

const ADMIN_KEY: &str = "admin-secret-key-for-bypass-test";
const T0: i64 = 1_753_600_000_000;

/// A live node — both listeners, one engine — plus the data dir it must
/// outlive. The native router is served too because it reaches the same
/// indices under a different spelling, so a boundary that holds on `:9200` and
/// falls open on `:8080` is not a boundary.
struct Node {
    addr: SocketAddr,
    native_addr: SocketAddr,
    _data_dir: tempfile::TempDir,
}

/// Bind an ephemeral port and serve `app` on it.
async fn serve(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

async fn start_node() -> Node {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.auth.enabled = true;
    config.auth.admin_api_key = ADMIN_KEY.to_string();
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    let state = AppState::new(config, engine, metrics);
    Node {
        addr: serve(build_es_compat_router(state.clone())).await,
        native_addr: serve(build_native_router(state)).await,
        _data_dir: dir,
    }
}

/// One HTTP/1.1 request over a fresh connection. Returns (status, body).
async fn http(node: &Node, method: &str, path: &str, auth: &str, body: &str) -> (u16, String) {
    request(node.addr, method, path, auth, body).await
}

/// The same, against the native `:8080`-shaped listener.
async fn http_native(
    node: &Node,
    method: &str,
    path: &str,
    auth: &str,
    body: &str,
) -> (u16, String) {
    request(node.native_addr, method, path, auth, body).await
}

async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    auth: &str,
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !auth.is_empty() {
        request.push_str(&format!("Authorization: {auth}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    stream.flush().await.expect("flush");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line in response: {text}"));
    // `Connection: close` means the body runs to EOF; chunked framing markers
    // are stripped so assertions can look at the payload.
    let body = match text.split_once("\r\n\r\n") {
        Some((_, rest)) => rest
            .lines()
            .filter(|l| !l.trim().is_empty() && u32::from_str_radix(l.trim(), 16).is_err())
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    };
    (status, body)
}

fn admin() -> String {
    format!("ApiKey {ADMIN_KEY}")
}

/// Mint a key and return its `Authorization` header value.
async fn mint(node: &Node, body: &str) -> String {
    let (status, resp) = http(node, "POST", "/_security/api_key", &admin(), body).await;
    assert_eq!(status, 200, "minting a key: {resp}");
    let v: serde_json::Value = serde_json::from_str(&resp).expect("key json");
    format!("ApiKey {}", v["encoded"].as_str().expect("encoded"))
}

/// A key scoped to exactly one brain — the documented grant shape.
fn brain_grant(brain: &str) -> String {
    format!(
        r#"{{"name":"{brain}-agent","role_descriptors":{{"{brain}":{{"indices":[
             {{"names":[".xerj-memory-{brain}-edges",".xerj-memory-{brain}"],
               "privileges":["read","write"]}}]}}}}}}"#
    )
}

/// Two brains, seeded as the admin. Bob's edge carries the marker the
/// verifier used, so a leak is unmistakable in an assertion message.
async fn seed(node: &Node) {
    for (brain, dst) in [("alice", "doc:2"), ("bob", "TOPSECRET:2")] {
        let (status, resp) = http(
            node,
            "POST",
            &format!("/_graph/{brain}/link"),
            &admin(),
            &format!(
                r#"{{"src":"doc:1","type":"mentions","dst":"{dst}","valid_at":{T0},"created_at":{T0}}}"#
            ),
        )
        .await;
        assert_eq!(status, 201, "seeding {brain}: {resp}");
    }
    for index in [".xerj-memory-alice-edges", ".xerj-memory-bob-edges"] {
        let (status, _) = http(node, "POST", &format!("/{index}/_refresh"), &admin(), "").await;
        assert_eq!(status, 200);
    }
}

/// The `_id` of bob's real edge, read as the admin — bypass 2's delete and
/// bypass 3's read both need a genuine document id, not a guess.
async fn bobs_edge_id(node: &Node) -> String {
    let (status, resp) = http(
        node,
        "POST",
        "/.xerj-memory-bob-edges/_search",
        &admin(),
        r#"{"query":{"match_all":{}},"size":50}"#,
    )
    .await;
    assert_eq!(status, 200, "reading bob as admin: {resp}");
    let v: serde_json::Value = serde_json::from_str(&resp).expect("search json");
    v["hits"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .find(|h| h["_source"].get("edge_id").is_some())
        .map(|h| h["_id"].as_str().expect("_id").to_string())
        .expect("bob must have a real edge")
}

/// Bob's brain, verbatim, as the admin sees it. The point of comparison for
/// "the attack changed nothing".
async fn bobs_edges(node: &Node) -> String {
    let (status, resp) = http(
        node,
        "POST",
        "/.xerj-memory-bob-edges/_search",
        &admin(),
        r#"{"query":{"match_all":{}},"size":50}"#,
    )
    .await;
    assert_eq!(status, 200, "reading bob as admin: {resp}");
    resp
}

// ─────────────────────────────────────────────────────────────────────────────

/// BYPASS 1 — read, scoped key, via `_msearch`.
///
/// PROVED: a key granting only alice's two indices sent
/// `POST /.xerj-memory-alice-edges/_msearch` with a header line naming
/// `.xerj-memory-bob-edges`, and got 200 with bob's documents including
/// `{"src":"doc:1","dst":"TOPSECRET:2","type":"mentions"}`.
#[tokio::test]
async fn msearch_header_cannot_override_the_authorized_index() {
    let node = start_node().await;
    seed(&node).await;
    let alice = mint(&node, &brain_grant("alice")).await;

    let attack = "{\"index\":\".xerj-memory-bob-edges\"}\n{\"query\":{\"match_all\":{}}}\n";
    for path in [
        "/.xerj-memory-alice-edges/_msearch",
        "/_msearch",
        "/.xerj-memory-alice-edges/_msearch/template",
    ] {
        let (status, resp) = http(&node, "POST", path, &alice, attack).await;
        assert_eq!(status, 403, "POST {path} must be refused, got {status}");
        assert!(
            !resp.contains("TOPSECRET"),
            "POST {path} leaked bob: {resp}"
        );
    }

    // The same verb against her own brain still works — a fix that breaks
    // _msearch is not a fix.
    let own = "{\"index\":\".xerj-memory-alice-edges\"}\n{\"query\":{\"match_all\":{}}}\n";
    let (status, resp) = http(&node, "POST", "/_msearch", &alice, own).await;
    assert_eq!(status, 200, "alice's own _msearch: {resp}");
    assert!(resp.contains("doc:2"), "alice's own edge missing: {resp}");
}

/// BYPASS 2 — write and destroy, scoped key, via `_bulk`.
///
/// PROVED: alice sent `POST /.xerj-memory-alice-edges/_bulk` with an action
/// line naming `.xerj-memory-bob-edges` and got `"result":"created"`; an
/// admin `GET` of that id returned `found:true`. A `delete` action against a
/// real edge id then DELETED bob's genuine edge — the two doors
/// (`authz.rs`'s own module header calls them "forge an edge around the
/// derived-edge_id invariant" and "destroy the brain") it claimed to have shut.
#[tokio::test]
async fn bulk_action_line_cannot_override_the_authorized_index() {
    let node = start_node().await;
    seed(&node).await;
    let alice = mint(&node, &brain_grant("alice")).await;
    let victim_id = bobs_edge_id(&node).await;
    let before = bobs_edges(&node).await;

    // Forge.
    let forge = "{\"index\":{\"_index\":\".xerj-memory-bob-edges\",\"_id\":\"forged-s\"}}\n\
                 {\"src\":\"doc:1\",\"dst\":\"attacker:payload\"}\n";
    for path in ["/.xerj-memory-alice-edges/_bulk", "/_bulk"] {
        let (status, resp) = http(&node, "POST", path, &alice, forge).await;
        assert_eq!(status, 403, "POST {path} forge must be refused: {resp}");
    }
    let (status, resp) = http(
        &node,
        "GET",
        "/.xerj-memory-bob-edges/_doc/forged-s",
        &admin(),
        "",
    )
    .await;
    assert!(
        status == 404 || resp.contains("\"found\":false"),
        "the forged document reached bob's brain: {status} {resp}"
    );

    // Destroy.
    let destroy = format!(
        "{{\"delete\":{{\"_index\":\".xerj-memory-bob-edges\",\"_id\":\"{victim_id}\"}}}}\n"
    );
    for path in ["/.xerj-memory-alice-edges/_bulk", "/_bulk"] {
        let (status, resp) = http(&node, "POST", path, &alice, &destroy).await;
        assert_eq!(status, 403, "POST {path} delete must be refused: {resp}");
    }
    let after = bobs_edges(&node).await;
    assert_eq!(before, after, "bob's brain was modified by the attack");
    assert!(after.contains("TOPSECRET"), "bob's edge is gone: {after}");

    // Alice's own bulk, by both spellings, still works.
    let own = "{\"index\":{\"_index\":\".xerj-memory-alice-edges\",\"_id\":\"own-1\"}}\n\
               {\"src\":\"a\",\"dst\":\"b\"}\n";
    let (status, resp) = http(&node, "POST", "/_bulk", &alice, own).await;
    assert_eq!(status, 200, "alice's own global _bulk: {resp}");
    assert!(
        !resp.contains("\"errors\":true"),
        "own bulk errored: {resp}"
    );
    let scoped = "{\"index\":{\"_id\":\"own-2\"}}\n{\"src\":\"a\",\"dst\":\"c\"}\n";
    let (status, resp) = http(
        &node,
        "POST",
        "/.xerj-memory-alice-edges/_bulk",
        &alice,
        scoped,
    )
    .await;
    assert_eq!(status, 200, "alice's own index-scoped _bulk: {resp}");
    assert!(
        !resp.contains("\"errors\":true"),
        "own bulk errored: {resp}"
    );
}

/// BYPASS 3 — read, scoped key, via `_mget`.
///
/// PROVED: `POST /.xerj-memory-alice-edges/_mget` with
/// `{"docs":[{"_index":".xerj-memory-bob-edges","_id":"<real id>"}]}` returned
/// 200 with bob's edge source containing `"dst":"TOPSECRET:2"`.
#[tokio::test]
async fn mget_docs_cannot_override_the_authorized_index() {
    let node = start_node().await;
    seed(&node).await;
    let alice = mint(&node, &brain_grant("alice")).await;
    let victim_id = bobs_edge_id(&node).await;

    let attack =
        format!(r#"{{"docs":[{{"_index":".xerj-memory-bob-edges","_id":"{victim_id}"}}]}}"#);
    for path in ["/.xerj-memory-alice-edges/_mget", "/_mget"] {
        let (status, resp) = http(&node, "POST", path, &alice, &attack).await;
        assert_eq!(status, 403, "POST {path} must be refused, got {status}");
        assert!(
            !resp.contains("TOPSECRET"),
            "POST {path} leaked bob: {resp}"
        );
    }

    // The short form against her own index is untouched.
    let (status, resp) = http(
        &node,
        "POST",
        "/.xerj-memory-alice-edges/_mget",
        &alice,
        r#"{"ids":["__xerj-brain-meta"]}"#,
    )
    .await;
    assert_eq!(status, 200, "alice's own _mget: {resp}");
}

/// BYPASS 4 — read, unscoped key, via aliases.
///
/// PROVED: a key with no `role_descriptors` sent `POST /_aliases` adding an
/// alias onto `.xerj-memory-bob-edges` (200 acknowledged), then searched the
/// alias and got bob's edges. `PUT /.xerj-memory-bob-edges/_alias/pwned` was
/// already refused, so this was precisely the "one missed shape" the module
/// documentation warned about. The second half matters as much: an alias an
/// operator had already pointed into the namespace made it readable by every
/// unscoped key.
#[tokio::test]
async fn aliases_cannot_launder_a_reserved_index() {
    let node = start_node().await;
    seed(&node).await;
    let unscoped = mint(&node, r#"{"name":"plain"}"#).await;

    // Creating the alias is refused, by both spellings.
    let add = r#"{"actions":[{"add":{"index":".xerj-memory-bob-edges","alias":"pwned-u"}}]}"#;
    let (status, resp) = http(&node, "POST", "/_aliases", &unscoped, add).await;
    assert_eq!(status, 403, "POST /_aliases must be refused: {resp}");
    let (status, resp) = http(
        &node,
        "PUT",
        "/.xerj-memory-bob-edges/_alias/pwned-p",
        &unscoped,
        "",
    )
    .await;
    assert_eq!(status, 403, "PUT .../_alias must be refused: {resp}");
    let (status, resp) = http(&node, "POST", "/pwned-u/_search", &unscoped, "{}").await;
    assert!(
        !resp.contains("TOPSECRET"),
        "the alias was created after all: {status} {resp}"
    );

    // Second half: an alias the OPERATOR already pointed into the namespace
    // must not become a back door either. Only the admin can make one.
    let operator_alias =
        r#"{"actions":[{"add":{"index":".xerj-memory-bob-edges","alias":"ops-view"}}]}"#;
    let (status, resp) = http(&node, "POST", "/_aliases", &admin(), operator_alias).await;
    assert_eq!(status, 200, "admin must still manage aliases: {resp}");

    for path in ["/ops-view/_search", "/ops-view/_count"] {
        let (status, resp) = http(&node, "POST", path, &unscoped, "{}").await;
        assert_eq!(
            status, 403,
            "reading through an operator alias must be refused: {resp}"
        );
        assert!(!resp.contains("TOPSECRET"), "alias leaked bob: {resp}");
    }
    // …including through a body-named door.
    let (status, resp) = http(
        &node,
        "POST",
        "/_msearch",
        &unscoped,
        "{\"index\":\"ops-view\"}\n{\"query\":{\"match_all\":{}}}\n",
    )
    .await;
    assert_eq!(status, 403, "aliased _msearch must be refused: {resp}");
    assert!(
        !resp.contains("TOPSECRET"),
        "aliased _msearch leaked: {resp}"
    );

    // An alias over an ordinary index is ordinary work and still allowed.
    let (status, resp) = http(
        &node,
        "PUT",
        "/logs-2026",
        &unscoped,
        r#"{"mappings":{"properties":{"m":{"type":"integer"}}}}"#,
    )
    .await;
    assert_eq!(status, 200, "creating an ordinary index: {resp}");
    let ordinary = r#"{"actions":[{"add":{"index":"logs-2026","alias":"logs"}}]}"#;
    let (status, resp) = http(&node, "POST", "/_aliases", &unscoped, ordinary).await;
    assert_eq!(status, 200, "an ordinary alias must still work: {resp}");
    // But an alias NAME inside the reserved namespace is squatting.
    let squat = r#"{"actions":[{"add":{"index":"logs-2026","alias":".xerj-memory-victim"}}]}"#;
    let (status, resp) = http(&node, "POST", "/_aliases", &unscoped, squat).await;
    assert_eq!(status, 403, "squatting a reserved alias name: {resp}");
}

/// The fifth shape, found by auditing the tree rather than by being attacked:
/// a `terms` lookup names an index to read terms out of, at arbitrary depth
/// inside a query body that the URL says is about something else.
#[tokio::test]
async fn terms_lookup_cannot_read_an_unauthorized_index() {
    let node = start_node().await;
    seed(&node).await;
    let alice = mint(&node, &brain_grant("alice")).await;
    let victim_id = bobs_edge_id(&node).await;

    let attack = format!(
        r#"{{"query":{{"bool":{{"filter":[{{"terms":{{"src":{{"index":".xerj-memory-bob-edges","id":"{victim_id}","path":"dst"}}}}}}]}}}}}}"#
    );
    let (status, resp) = http(
        &node,
        "POST",
        "/.xerj-memory-alice-edges/_search",
        &alice,
        &attack,
    )
    .await;
    assert_eq!(status, 403, "a terms lookup into bob must be refused");
    assert!(!resp.contains("TOPSECRET"), "terms lookup leaked: {resp}");

    // The same shape one layer over: a `lookup` runtime field joins rows out
    // of another index into the hits.
    let runtime = r#"{"runtime_mappings":{"u":{"type":"lookup","target_index":".xerj-memory-bob-edges","input_field":"src","target_field":"_id","fetch_fields":["dst"]}},"fields":["u"]}"#;
    let (status, resp) = http(
        &node,
        "POST",
        "/.xerj-memory-alice-edges/_search",
        &alice,
        runtime,
    )
    .await;
    assert_eq!(status, 403, "a lookup runtime field into bob: {resp}");
    assert!(!resp.contains("TOPSECRET"), "runtime lookup leaked: {resp}");
}

/// The reason this fix is two layers and not a patch list.
///
/// `_sql` names its index in the FROM clause of a SQL string, an ingest
/// pipeline names one in a `route` processor it stored earlier, an index
/// template names a *pattern* that decides the mapping of brains created
/// later. None of those is a JSON field the middleware parses — and none of
/// them has to be, because the name still has to become an index through
/// `Engine::get_index` / `get_or_create_index` / `create_index`, where the
/// request's principal is installed as a visibility rule. This is the property
/// that a per-handler patch list cannot have.
#[tokio::test]
async fn shapes_the_middleware_never_parses_still_fail_closed() {
    let node = start_node().await;
    seed(&node).await;
    let alice = mint(&node, &brain_grant("alice")).await;
    let unscoped = mint(&node, r#"{"name":"plain"}"#).await;
    let before = bobs_edges(&node).await;

    // `_sql` — the table name is inside the statement.
    let (status, resp) = http(
        &node,
        "POST",
        "/_sql",
        &alice,
        r#"{"query":"SELECT * FROM \".xerj-memory-bob-edges\""}"#,
    )
    .await;
    assert_ne!(status, 200, "_sql reached bob's brain: {resp}");
    assert!(!resp.contains("TOPSECRET"), "_sql leaked bob: {resp}");

    // An index template that would own the mapping of every brain created
    // from here on.
    let (status, resp) = http(
        &node,
        "PUT",
        "/_index_template/grab",
        &unscoped,
        r#"{"index_patterns":[".xerj-memory-*"],"template":{"mappings":{"properties":{"dst":{"type":"keyword"}}}}}"#,
    )
    .await;
    assert_eq!(status, 403, "a namespace-wide template: {resp}");

    // `POST /v1/pipelines/{name}` can register a `route` processor whose
    // target index is chosen at ingest time, not at request time.
    let (status, resp) = http_native(
        &node,
        "PUT",
        "/v1/pipelines/exfil",
        &unscoped,
        r#"{"steps":[{"route":{"index":".xerj-memory-bob-edges"}}]}"#,
    )
    .await;
    // Whether the pipeline definition is accepted is the pipeline API's
    // business; what matters is that ingesting through it cannot reach bob.
    let _ = (status, resp);
    let (status, resp) = http_native(
        &node,
        "POST",
        "/v1/indices/logs-pipe/ingest?pipeline=exfil",
        &unscoped,
        r#"{"documents":[{"src":"a","dst":"b"}]}"#,
    )
    .await;
    assert!(
        !resp.contains("TOPSECRET"),
        "pipeline ingest leaked bob: {status} {resp}"
    );

    // Nothing above touched bob's brain.
    let after = bobs_edges(&node).await;
    assert_eq!(before, after, "bob's brain changed: {before} vs {after}");
}

/// COMPATIBILITY — the regression the same branch introduced, also measured.
///
/// Every non-superuser credential, including a plain minted key, got 403 on
/// `POST /_bulk`, `/_msearch`, `/_mget`, `/_search` and `/_count`. The global
/// `_bulk` is the default write path for essentially every ES client, so that
/// broke ordinary usage rather than securing it. A scoped key additionally got
/// 403 on all cluster metadata and on every wildcard, including one it had
/// been granted, which Kibana cannot survive.
#[tokio::test]
async fn ordinary_credentials_keep_the_global_surface() {
    let node = start_node().await;
    seed(&node).await;
    let unscoped = mint(&node, r#"{"name":"plain"}"#).await;
    let alice = mint(&node, &brain_grant("alice")).await;
    let wide = mint(
        &node,
        r#"{"name":"wide","role_descriptors":{"w":{"indices":[
             {"names":["logs-*"],"privileges":["all"]}]}}}"#,
    )
    .await;

    let (status, resp) = http(
        &node,
        "PUT",
        "/logs-2026",
        &unscoped,
        r#"{"mappings":{"properties":{"m":{"type":"integer"}}}}"#,
    )
    .await;
    assert_eq!(status, 200, "creating an ordinary index: {resp}");

    // (a) The five global verbs answer for an ordinary credential.
    for (method, path, body) in [
        ("POST", "/_search", "{}"),
        ("POST", "/_count", "{}"),
        ("POST", "/_msearch", "{}\n{\"query\":{\"match_all\":{}}}\n"),
        (
            "POST",
            "/_mget",
            r#"{"docs":[{"_index":"logs-2026","_id":"1"}]}"#,
        ),
        (
            "POST",
            "/_bulk",
            "{\"index\":{\"_index\":\"logs-2026\",\"_id\":\"1\"}}\n{\"m\":1}\n",
        ),
    ] {
        for (who, cred) in [("unscoped", &unscoped), ("wide", &wide)] {
            let (status, resp) = http(&node, method, path, cred, body).await;
            assert_eq!(
                status, 200,
                "{who} {method} {path} must work, got {status}: {resp}"
            );
        }
    }

    // (b) Cluster metadata answers for a SCOPED key, filtered rather than
    // refused. Kibana polls all of these on every page load.
    for path in [
        "/_cluster/health",
        "/_nodes",
        "/_mapping",
        "/_settings",
        "/_alias",
        "/_cat/indices",
        "/_cat/health",
        "/_cat/nodes",
        "/_cat/aliases",
        "/_cluster/state",
        "/_cluster/stats",
        "/_resolve/index/*",
        "/_xpack",
    ] {
        let (status, resp) = http(&node, "GET", path, &alice, "").await;
        assert_eq!(
            status, 200,
            "a scoped key must read {path}, got {status}: {resp}"
        );
        assert!(
            !resp.contains("TOPSECRET") && !resp.contains("bob"),
            "{path} leaked another tenant: {resp}"
        );
    }
    // …and the native router's own metadata, which lives on the other listener.
    for path in ["/v1/health", "/v1/cluster/health", "/v1/dashboard/summary"] {
        let (status, resp) = http_native(&node, "GET", path, &alice, "").await;
        assert_eq!(
            status, 200,
            "a scoped key must read native {path}, got {status}: {resp}"
        );
        assert!(!resp.contains("bob"), "native {path} leaked: {resp}");
    }

    // (c) A granted wildcard actually works, and the data it returns is the
    // grant's, not the cluster's.
    let (status, resp) = http(
        &node,
        "POST",
        "/_bulk",
        &wide,
        "{\"index\":{\"_index\":\"logs-2026\",\"_id\":\"w1\"}}\n{\"m\":7}\n",
    )
    .await;
    assert_eq!(status, 200, "granted wildcard write: {resp}");
    let (status, _) = http(&node, "POST", "/logs-2026/_refresh", &admin(), "").await;
    assert_eq!(status, 200);
    let (status, resp) = http(&node, "POST", "/logs-*/_search", &wide, "{}").await;
    assert_eq!(status, 200, "granted wildcard read: {resp}");
    assert!(resp.contains("logs-2026"), "wildcard found nothing: {resp}");
    assert!(!resp.contains("TOPSECRET"), "wildcard leaked bob: {resp}");

    // (d) An UNgranted wildcard resolves to nothing instead of leaking what it
    // would have matched — and instead of a 403 that would tell the caller the
    // pattern was interesting.
    for path in ["/*/_search", "/_all/_search", "/.xerj-memory-*/_search"] {
        let (status, resp) = http(&node, "POST", path, &wide, "{}").await;
        assert_eq!(status, 200, "POST {path}: {resp}");
        assert!(!resp.contains("TOPSECRET"), "POST {path} leaked: {resp}");
    }
}

/// The zero-configuration local path — `xerj --insecure`, point-at-a-folder —
/// is one user with no credentials and must be completely unaffected.
#[tokio::test]
async fn insecure_single_user_mode_is_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config.auth.enabled = false;
    config.auth.admin_api_key = String::new();
    let metrics = Metrics::new().expect("metrics");
    let engine = Engine::new(config.clone()).expect("engine");
    let state = AppState::new(config, engine, metrics);
    let node = Node {
        addr: serve(build_es_compat_router(state.clone())).await,
        native_addr: serve(build_native_router(state)).await,
        _data_dir: dir,
    };

    let (status, resp) = http(
        &node,
        "POST",
        "/_graph/solo/link",
        "",
        &format!(r#"{{"src":"a","type":"mentions","dst":"b","valid_at":{T0},"created_at":{T0}}}"#),
    )
    .await;
    assert_eq!(status, 201, "no-credential link must work: {resp}");
    for (method, path, body) in [
        ("GET", "/_graph/solo/overview", ""),
        ("POST", "/_search", "{}"),
        (
            "POST",
            "/_bulk",
            "{\"index\":{\"_index\":\"anything\"}}\n{\"a\":1}\n",
        ),
        ("GET", "/_cat/indices", ""),
        ("GET", "/_mapping", ""),
        ("POST", "/.xerj-memory-solo-edges/_search", "{}"),
        (
            "POST",
            "/_aliases",
            r#"{"actions":[{"add":{"index":".xerj-memory-solo-edges","alias":"solo"}}]}"#,
        ),
        ("POST", "/solo/_search", "{}"),
    ] {
        let (status, resp) = http(&node, method, path, "", body).await;
        assert_eq!(
            status, 200,
            "{method} {path} must work with no credential: {resp}"
        );
    }
    // The native router's body-named create, unrestricted for the single user.
    let (status, resp) = http_native(
        &node,
        "POST",
        "/v1/indices",
        "",
        r#"{"name":"solo-notes","fields":[]}"#,
    )
    .await;
    assert_eq!(status, 201, "native create with no credential: {resp}");
}

/// The native router's `POST /v1/indices` names its index in the body. The
/// first cut refused it outright for every non-superuser, which broke the
/// native create path; it is authorized against the name it carries instead.
#[tokio::test]
async fn native_body_named_create_is_authorized_not_refused() {
    let node = start_node().await;
    let unscoped = mint(&node, r#"{"name":"plain"}"#).await;

    let (status, resp) = http_native(
        &node,
        "POST",
        "/v1/indices",
        &unscoped,
        r#"{"name":"logs-native","fields":[]}"#,
    )
    .await;
    assert_eq!(status, 201, "an ordinary native create must work: {resp}");

    // Squatting a brain name the caller could then not read is refused.
    let (status, resp) = http_native(
        &node,
        "POST",
        "/v1/indices",
        &unscoped,
        r#"{"name":".xerj-memory-victim-edges","fields":[]}"#,
    )
    .await;
    assert_eq!(status, 403, "squatting the namespace: {resp}");
    let (status, resp) = http(&node, "GET", "/.xerj-memory-victim-edges", &admin(), "").await;
    assert_eq!(status, 404, "the squatted index was created: {resp}");
}
