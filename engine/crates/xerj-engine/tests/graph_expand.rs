//! Engine-level tests for `Index::graph_expand` (second brain, contract §3.6).
//!
//! Every test builds the contract §8 fixture — the five `notes/` files' edge
//! set (4 wikilink + 4 same_dir edges, all valid_at = created_at =
//! 1753600000000) — via `index_document` on a `.xerj-memory-notes-edges`
//! index, then asserts the §8.4 expansions:
//!
//! - pre-flush (memtable path) and post-flush (columnar path) results are
//!   identical modulo the two flush-state cost counters,
//! - soft invalidation is time-travelable via `as_of` (and exercises the
//!   ghost-bitmap integration: the stale pre-invalidation segment row must
//!   NOT resurface),
//! - the hop-2 block ordering, the result-edge cap, and the hops bounds.

use serde_json::{json, Value};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::graph::{
    GraphDirection, GraphExpandRequest, GraphExpandResult, GRAPH_HOPS_CAP_REASON,
};
use xerj_engine::{Engine, Index};

const EDGES_INDEX: &str = ".xerj-memory-notes-edges";
const T0: i64 = 1_753_600_000_000; // fixture valid_at / created_at
const AS_OF: i64 = 1_753_700_000_000; // fixture as-of instant

// §8.3 edge ids (computed with the contract's edge_id fn; pinned there).
const E_ALPHA_BETA_WIKI: &str = "bef814a75bd3d914c3e561f610154304";
const E_ALPHA_GAMMA_WIKI: &str = "11c2d0ef216cd6e99a3907a0b53c1452";
const E_BETA_GAMMA_WIKI: &str = "9bbf7d2068321ac0fa71d95e21fae2fd";
const E_DELTA_ALPHA_WIKI: &str = "cead55986c364ad5ff6f0894daf61f77";
const E_ALPHA_BETA_DIR: &str = "63b747655365aa16d38188aa49966f40";
const E_BETA_DELTA_DIR: &str = "a61e6caacb5e485baf6d45184f23ec67";
const E_DELTA_EPSILON_DIR: &str = "3efff61b58c978943e6fd2a1e4eeaee8";
const E_EPSILON_GAMMA_DIR: &str = "7c07cdc441f0a3faa29be8946df3e7a4";

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

/// One §2.4-shaped edge document. Timestamps are epoch-ms NUMBERS and
/// `invalid_at`/`expired_at` are omitted entirely (never null) — the omission
/// is what lands the row in the column null_bitmap, which is the "still valid"
/// signal the hop reads.
#[allow(clippy::too_many_arguments)]
fn edge_doc(
    edge_id: &str,
    src: &str,
    dst: &str,
    edge_type: &str,
    weight: f64,
    confidence: f64,
    detector: &str,
    quote: &str,
    source: &str,
    offset: u64,
) -> Value {
    json!({
        "edge_id": edge_id,
        "src": src,
        "dst": dst,
        "type": edge_type,
        "weight": weight,
        "valid_at": T0,
        "created_at": T0,
        "detector": detector,
        "confidence": confidence,
        "schema_version": 1,
        "src_file": source,
        "evidence": { "quote": quote, "source": source, "offset": offset }
    })
}

/// The eight §8.3 fixture edges, in table order.
fn fixture_edges() -> Vec<Value> {
    let alpha_line = "Alpha is the hub note. It links to [[beta]] and [[gamma]].";
    vec![
        edge_doc(
            E_ALPHA_BETA_WIKI,
            "note-alpha",
            "note-beta",
            "wikilink",
            1.0,
            0.95,
            "wikilink@1",
            alpha_line,
            "alpha.md",
            35,
        ),
        edge_doc(
            E_ALPHA_GAMMA_WIKI,
            "note-alpha",
            "note-gamma",
            "wikilink",
            1.0,
            0.95,
            "wikilink@1",
            alpha_line,
            "alpha.md",
            48,
        ),
        edge_doc(
            E_BETA_GAMMA_WIKI,
            "note-beta",
            "note-gamma",
            "wikilink",
            1.0,
            0.95,
            "wikilink@1",
            "Beta continues the thread and references [[gamma]].",
            "beta.md",
            41,
        ),
        edge_doc(
            E_DELTA_ALPHA_WIKI,
            "note-delta",
            "note-alpha",
            "wikilink",
            1.0,
            0.95,
            "wikilink@1",
            "Delta cites [[alpha]] as its source.",
            "delta.md",
            12,
        ),
        edge_doc(
            E_ALPHA_BETA_DIR,
            "note-alpha",
            "note-beta",
            "same_dir",
            0.3,
            0.4,
            "samedir@1",
            "alpha.md and beta.md share directory .",
            "alpha.md",
            0,
        ),
        edge_doc(
            E_BETA_DELTA_DIR,
            "note-beta",
            "note-delta",
            "same_dir",
            0.3,
            0.4,
            "samedir@1",
            "beta.md and delta.md share directory .",
            "beta.md",
            0,
        ),
        edge_doc(
            E_DELTA_EPSILON_DIR,
            "note-delta",
            "note-epsilon",
            "same_dir",
            0.3,
            0.4,
            "samedir@1",
            "delta.md and epsilon.md share directory .",
            "delta.md",
            0,
        ),
        edge_doc(
            E_EPSILON_GAMMA_DIR,
            "note-epsilon",
            "note-gamma",
            "same_dir",
            0.3,
            0.4,
            "samedir@1",
            "epsilon.md and gamma.md share directory .",
            "epsilon.md",
            0,
        ),
    ]
}

/// The edges index with its §2.2 mapping DECLARED.
///
/// Both production writers declare it (`graph_api::edge_index_mapping`,
/// `detect::edge_index_mapping`): keyword for the ids/type/detector/src_file,
/// numeric/date for the rest. `Index::graph_expand` reads `src`/`dst`/
/// `edge_id`/`type`/`weight`/`valid_at` exclusively from doc-values columns —
/// a segment without them is SKIPPED and reported (`segments_without_columns`,
/// graph.rs module docs: the hop never hydrates). This fixture used to pass
/// `Schema::empty()` and let dynamic mapping stand in; since #1329 a
/// dynamically inferred string is a `text` parent with NO doc-values (the ES
/// default), so the stand-in lost its keyword columns and every expansion
/// returned empty. Declare the mapping the contract actually specifies.
fn edges_schema() -> Schema {
    let mut schema = Schema::empty();
    for name in ["edge_id", "src", "dst", "type", "detector", "src_file"] {
        schema
            .fields
            .push(FieldConfig::new(name, FieldType::Keyword));
    }
    for name in ["weight", "confidence"] {
        schema
            .fields
            .push(FieldConfig::new(name, FieldType::Double));
    }
    for name in ["valid_at", "invalid_at", "created_at", "expired_at"] {
        schema.fields.push(FieldConfig::new(name, FieldType::Date));
    }
    schema
        .fields
        .push(FieldConfig::new("schema_version", FieldType::Long));
    schema
}

async fn fixture_index(engine: &Engine) -> std::sync::Arc<Index> {
    engine
        .create_index(EDGES_INDEX, edges_schema())
        .expect("create edges index");
    let idx = engine.get_index(EDGES_INDEX).expect("get edges index");
    for doc in fixture_edges() {
        let id = doc["edge_id"].as_str().unwrap().to_string();
        idx.index_document(Some(id), doc).await.expect("index edge");
    }
    idx
}

fn expand_req(frontier: &[&str], hops: u8) -> GraphExpandRequest {
    GraphExpandRequest {
        frontier: frontier.iter().map(|s| s.to_string()).collect(),
        hops,
        direction: GraphDirection::Both,
        types: None,
        as_of_ms: AS_OF,
        include_expired: false,
        max_result_edges: 1000,
    }
}

/// (edge_id, src, dst, type, weight, hop) tuples — the §8.4 comparison shape.
fn tuples(res: &GraphExpandResult) -> Vec<(String, String, String, String, f64, u8)> {
    res.edges
        .iter()
        .map(|e| {
            (
                e.edge_id.clone(),
                e.src.clone(),
                e.dst.clone(),
                e.edge_type.clone(),
                e.weight,
                e.hop,
            )
        })
        .collect()
}

fn assert_hop1_fixture(res: &GraphExpandResult) {
    // §8.4: EXACTLY this order (hop asc, weight desc, edge_id asc).
    let got = tuples(res);
    let want = vec![
        (
            E_ALPHA_GAMMA_WIKI.into(),
            "note-alpha".into(),
            "note-gamma".into(),
            "wikilink".into(),
            1.0,
            1u8,
        ),
        (
            E_ALPHA_BETA_WIKI.into(),
            "note-alpha".into(),
            "note-beta".into(),
            "wikilink".into(),
            1.0,
            1u8,
        ),
        (
            E_DELTA_ALPHA_WIKI.into(),
            "note-delta".into(),
            "note-alpha".into(),
            "wikilink".into(),
            1.0,
            1u8,
        ),
        (
            E_ALPHA_BETA_DIR.into(),
            "note-alpha".into(),
            "note-beta".into(),
            "same_dir".into(),
            0.3,
            1u8,
        ),
    ];
    assert_eq!(got, want, "hop-1 edge list must match §8.4 exactly");
    assert_eq!(
        res.reachable,
        vec!["note-alpha", "note-gamma", "note-beta", "note-delta"],
        "§8.4 reachable order: frontier first, then first-discovery over sorted edges"
    );
    // Every stats field except the two flush-state cost counters is exact.
    assert_eq!(res.stats.frontier_clipped, 0);
    assert_eq!(res.stats.edges_clipped, 0);
    assert_eq!(res.stats.expired_excluded, 0);
    assert_eq!(res.stats.type_filtered, 0);
    assert_eq!(res.stats.segments_without_columns, 0);
}

/// §3.6 (a) + (b): the §8.4 hop-1 expansion is exact on the memtable path and
/// identical after `flush()` on the columnar path.
#[tokio::test]
async fn graph_expand_fixture_hop1_pre_and_post_flush() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = fixture_index(&engine).await;
    let req = expand_req(&["note-alpha"], 1);

    // Pre-flush: everything comes from the memtable walk.
    let pre = idx.graph_expand(&req).expect("pre-flush expand");
    assert_hop1_fixture(&pre);
    assert_eq!(pre.stats.segments_scanned, 0, "no segments exist pre-flush");
    assert!(pre.stats.memtable_docs_scanned >= 8);

    // Post-flush: everything comes from doc-values columns.
    idx.flush().await.expect("flush");
    let post = idx.graph_expand(&req).expect("post-flush expand");
    assert_hop1_fixture(&post);
    assert!(post.stats.segments_scanned >= 1, "columnar path must run");
    assert_eq!(
        post.stats.memtable_docs_scanned, 0,
        "flush drained the memtable"
    );
    assert_eq!(tuples(&pre), tuples(&post), "flush must not change results");
    assert_eq!(pre.reachable, post.reachable);
}

/// §8.4 hop-2: the hop-2 block (frontier = gamma, beta, delta) appends the
/// remaining four edges in (weight desc, edge_id asc) order and reachable
/// gains note-epsilon.
#[tokio::test]
async fn graph_expand_fixture_hop2_block_order() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = fixture_index(&engine).await;
    let req = expand_req(&["note-alpha"], 2);

    for flushed in [false, true] {
        if flushed {
            idx.flush().await.expect("flush");
        }
        let res = idx.graph_expand(&req).expect("expand hops=2");
        let ids: Vec<&str> = res.edges.iter().map(|e| e.edge_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                // hop-1 block (§8.4)
                E_ALPHA_GAMMA_WIKI,
                E_ALPHA_BETA_WIKI,
                E_DELTA_ALPHA_WIKI,
                E_ALPHA_BETA_DIR,
                // hop-2 block: 9bbf (w 1.0), then w-0.3 ties by edge_id asc
                E_BETA_GAMMA_WIKI,
                E_DELTA_EPSILON_DIR,
                E_EPSILON_GAMMA_DIR,
                E_BETA_DELTA_DIR,
            ],
            "hop-2 block order (flushed={flushed})"
        );
        assert!(res.edges[..4].iter().all(|e| e.hop == 1));
        assert!(res.edges[4..].iter().all(|e| e.hop == 2));
        assert_eq!(
            res.reachable,
            vec![
                "note-alpha",
                "note-gamma",
                "note-beta",
                "note-delta",
                "note-epsilon"
            ],
            "hop 2 reaches epsilon (flushed={flushed})"
        );
    }
}

/// §3.6 (c) + §8.6: soft invalidation is a re-index under the same `_id` with
/// `invalid_at`/`expired_at` added. Expanding at `as_of < invalid_at` still
/// sees the edge (belief last Tuesday intact); at `as_of >= invalid_at` it is
/// gone with `expired_excluded == 1`; `include_expired` brings it back.
///
/// The first round runs with the STALE row in a flushed segment and the
/// invalidated row in the memtable — the mixed state that only works when the
/// ghost-bitmap machinery suppresses the superseded segment row.
#[tokio::test]
async fn graph_expand_soft_invalidate_time_travel() {
    const INVALID_AT: i64 = 1_753_650_000_000;
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = fixture_index(&engine).await;
    idx.flush().await.expect("flush fixture");

    // Soft-invalidate alpha→beta wikilink: same doc, same `_id`, two added
    // fields — exactly what `DELETE /_graph/{brain}/link/{edge_id}` re-indexes.
    let mut invalidated = fixture_edges()[0].clone();
    invalidated["invalid_at"] = json!(INVALID_AT);
    invalidated["expired_at"] = json!(INVALID_AT);
    idx.index_document(Some(E_ALPHA_BETA_WIKI.to_string()), invalidated)
        .await
        .expect("re-index invalidated edge");

    for (round, flushed) in [(1, false), (2, true)] {
        if flushed {
            idx.flush().await.expect("flush invalidated version");
        }

        // Belief BEFORE the invalidation instant: all four §8.4 edges.
        let mut req = expand_req(&["note-alpha"], 1);
        req.as_of_ms = 1_753_640_000_000;
        let before = idx.graph_expand(&req).expect("expand before invalid_at");
        assert_hop1_fixture(&before);

        // Belief AFTER: the invalidated edge is excluded and counted.
        req.as_of_ms = AS_OF;
        let after = idx.graph_expand(&req).expect("expand after invalid_at");
        let ids: Vec<&str> = after.edges.iter().map(|e| e.edge_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![E_ALPHA_GAMMA_WIKI, E_DELTA_ALPHA_WIKI, E_ALPHA_BETA_DIR],
            "round {round}: invalidated edge must disappear at as_of >= invalid_at"
        );
        assert_eq!(
            after.stats.expired_excluded, 1,
            "round {round}: the exclusion is counted, not silent"
        );

        // include_expired=true: the edge returns, carrying its invalid_at.
        req.include_expired = true;
        let all = idx.graph_expand(&req).expect("expand include_expired");
        let inv = all
            .edges
            .iter()
            .find(|e| e.edge_id == E_ALPHA_BETA_WIKI)
            .expect("include_expired returns the invalidated edge");
        assert_eq!(inv.invalid_at_ms, Some(INVALID_AT));
        assert_eq!(all.edges.len(), 4);
    }
}

/// §3.6 (d) + §3.5: result cap reports clipping; hops out of bounds and an
/// empty frontier are loud `InvalidQuery` errors carrying the §4.6 sentence.
#[tokio::test]
async fn graph_expand_caps_and_bounds() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = fixture_index(&engine).await;

    for flushed in [false, true] {
        if flushed {
            idx.flush().await.expect("flush");
        }
        let mut req = expand_req(&["note-alpha"], 2);
        req.max_result_edges = 2;
        let res = idx.graph_expand(&req).expect("capped expand");
        assert_eq!(
            res.edges.len(),
            2,
            "cap bounds the result (flushed={flushed})"
        );
        assert!(
            res.stats.edges_clipped > 0,
            "overflow must be counted, never silent (flushed={flushed})"
        );
    }

    for bad_hops in [0u8, 3] {
        let err = idx
            .graph_expand(&expand_req(&["note-alpha"], bad_hops))
            .expect_err("hops out of bounds must fail loud");
        let msg = err.to_string();
        assert!(
            msg.contains(GRAPH_HOPS_CAP_REASON),
            "hops={bad_hops} error must carry the §4.6 sentence, got: {msg}"
        );
    }

    let err = idx
        .graph_expand(&expand_req(&[], 1))
        .expect_err("empty frontier must fail loud");
    assert!(err.to_string().contains("non-empty frontier"));
}

/// Direction and type filters: `Out`/`In` scan a single column; the `types`
/// allowlist counts what it drops.
#[tokio::test]
async fn graph_expand_direction_and_type_filter() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    let idx = fixture_index(&engine).await;
    idx.flush().await.expect("flush");

    let mut req = expand_req(&["note-alpha"], 1);
    req.direction = GraphDirection::Out;
    let out = idx.graph_expand(&req).expect("out expand");
    let ids: Vec<&str> = out.edges.iter().map(|e| e.edge_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![E_ALPHA_GAMMA_WIKI, E_ALPHA_BETA_WIKI, E_ALPHA_BETA_DIR],
        "direction=out follows src → dst only"
    );

    req.direction = GraphDirection::In;
    let inn = idx.graph_expand(&req).expect("in expand");
    let ids: Vec<&str> = inn.edges.iter().map(|e| e.edge_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![E_DELTA_ALPHA_WIKI],
        "direction=in follows dst → src only"
    );

    req.direction = GraphDirection::Both;
    req.types = Some(vec!["wikilink".to_string()]);
    let typed = idx.graph_expand(&req).expect("typed expand");
    let ids: Vec<&str> = typed.edges.iter().map(|e| e.edge_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![E_ALPHA_GAMMA_WIKI, E_ALPHA_BETA_WIKI, E_DELTA_ALPHA_WIKI],
        "types allowlist keeps only wikilink edges"
    );
    assert_eq!(
        typed.stats.type_filtered, 1,
        "the dropped same_dir edge is counted"
    );
}
