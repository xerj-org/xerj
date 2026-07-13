//! First-launch seeding of the built-in dashboards as **editable backend data**.
//!
//! The SPA historically defined its 13 flagship dashboards in code
//! (`xerj-ux/src/dashboards/*.js` + `registry.js`).  That made them a fixed,
//! un-editable set: a layout tweak or a rename never survived a reload, and
//! there was no server object to attach a per-panel query to.  This module
//! materialises those same 13 dashboards as rows in `.xerj_dashboards` on
//! first launch, so from that point on they are *data* the operator can edit
//! (title, layout, panel geometry, per-panel query/viz) and that persists
//! through the CRUD surface in [`crate::dashboards`].
//!
//! ## What is seeded
//!
//! One [`Dashboard`](crate::dashboards::Dashboard) per registry entry, with a
//! **stable deterministic id** `default-<registry-id>` (e.g.
//! `default-ai-overview`) — *not* a uuid — so re-seeding is idempotent by id.
//! Each is `owner = "system"`, `visibility = "default"`, `managed = true`,
//! `version = 1`, carrying the `section`/`group` taxonomy from `registry.js`
//! and a panel skeleton generated from the dashboard's `.js` source
//! (`id` / `type` / `title` (from `eyebrow`) / `layout` (from `cols`) /
//! `builtin` provenance key).
//!
//! ## Honest scope of the skeletons
//!
//! Every flagship panel today binds to *computed mock data* in the browser,
//! so every seeded panel carries a `builtin: "<registry-id>/<panel-id>"`
//! provenance key and a `query: null` — the shipped renderer resolves its
//! data through that key.  Structural metadata + free-form layout become
//! true editable data immediately; full query-backed data-fication of each
//! panel is incremental and lands panel-by-panel in the frontend renderer
//! (which fills in `query` + `viz` and drops `builtin`).  This is the whole
//! point of the split: geometry/title/type are editable *now*, without
//! waiting on the data layer.
//!
//! ## Idempotency + the editable-default tension
//!
//! Seeding runs on **every** boot (mirroring [`crate::indices::ensure_all`]'s
//! create-if-missing), not just the first:
//!
//! * absent id → create the skeleton (covers fresh installs *and* defaults
//!   added in a later release);
//! * present id → **skip** — never clobber.  A user edit (which bumps
//!   `version` past 1) or an operator soft-delete is therefore permanent.
//!
//! The single exception is an **opt-in migration**: a `default-__seed_meta`
//! row records the shipped [`SEED_REVISION`].  Only when a new binary ships a
//! higher revision are *untouched* defaults (`version <= 1`, not deleted)
//! re-upserted onto the new skeleton — edited ones are still skipped.  This
//! gives "editable defaults, data not code" without a re-seed ever silently
//! reverting an operator's work.
//!
//! The matching write-side change lives in [`crate::dashboards`]: a `managed`
//! (or `default`-visibility) doc is editable by admin/owner (so layout/title
//! edits persist) but not deletable, and re-seed skips anything past
//! `version == 1`.

use std::collections::HashMap;

use serde_json::{json, Value};
use xerj_engine::Engine;

use crate::error::ConsoleResult;
use crate::indices;
use crate::time::now_iso;

/// Shipped revision of the default-dashboard skeletons.  Bump this **only**
/// when a change to the seeded skeletons should migrate onto installs that
/// have not forked them.  See the module docs for the migration contract.
pub const SEED_REVISION: u64 = 1;

/// Id of the bookkeeping row that stores [`SEED_REVISION`].  It lives in the
/// same index but is not a dashboard: it fails to deserialize into
/// [`crate::dashboards::Dashboard`], so `list`/`get` skip it naturally.
const SEED_META_ID: &str = "default-__seed_meta";

// ─────────────────────────────────────────────────────────────────────────────
// Skeleton specs (transcribed once from xerj-ux/src/dashboards/*.js)
// ─────────────────────────────────────────────────────────────────────────────

/// A single panel in a seed skeleton. `cols` is the historical 12-col width
/// which becomes `layout.w`; `x`/`y`/`h` are computed by the flow layout.
struct PanelSpec {
    id: &'static str,
    kind: &'static str,
    title: &'static str,
    cols: u32,
    drilldown_to: Option<&'static str>,
}

/// One default dashboard.
struct DashboardSpec {
    /// registry.js id — the stable seed id is `default-<registry_id>`.
    registry_id: &'static str,
    name: &'static str,
    section: Option<&'static str>,
    group: Option<&'static str>,
    panels: Vec<PanelSpec>,
}

/// Panel without a drilldown.
fn p(id: &'static str, kind: &'static str, title: &'static str, cols: u32) -> PanelSpec {
    PanelSpec {
        id,
        kind,
        title,
        cols,
        drilldown_to: None,
    }
}

/// Panel that drills down into another dashboard on click.
fn pd(
    id: &'static str,
    kind: &'static str,
    title: &'static str,
    cols: u32,
    to: &'static str,
) -> PanelSpec {
    PanelSpec {
        id,
        kind,
        title,
        cols,
        drilldown_to: Some(to),
    }
}

/// The 13 built-in dashboards, in registry order.  Titles are the panel
/// `eyebrow` strings from the `.js` sources; dynamic eyebrows are captured as
/// a stable static string.
fn seed_specs() -> Vec<DashboardSpec> {
    vec![
        // ── AI group ────────────────────────────────────────────────────────
        DashboardSpec {
            registry_id: "ai-overview",
            name: "AI · Overview",
            section: Some("dashboards"),
            group: Some("ai"),
            panels: vec![
                p("queries", "metric", "LLM QUERIES", 4),
                p("tokens", "metric", "TOKENS · IN + OUT", 2),
                p("cost", "metric", "SPEND · USD", 2),
                p("savings", "metric", "vs. ES + PINECONE + SPLUNK", 2),
                p("cacheHit", "metric", "CACHE HIT", 2),
                p("queriesSeries", "line", "QUERIES OVER TIME", 12),
                p(
                    "latencyRibbons",
                    "ribbon3d",
                    "LATENCY · PER MODEL · AXONOMETRIC",
                    12,
                ),
                p("tokenFlow", "flowband", "TOKEN BUDGET", 12),
                p("models", "dist", "BY MODEL", 12),
                pd(
                    "topIntents",
                    "topn",
                    "TOP INTENTS · CLICK TO DRILL",
                    6,
                    "search-discover",
                ),
                p("topDocs", "topn", "TOP DOCUMENTS · CLICK TO FILTER", 6),
                p("costHeatmap", "heatmap", "SPEND · WEEKDAY × 2H", 12),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "rag-quality",
            name: "RAG · Quality",
            section: Some("dashboards"),
            group: Some("ai"),
            panels: vec![
                p("grounding", "metric", "GROUNDING SCORE", 4),
                p("halluc", "metric", "HALLUCINATION RATE", 3),
                p("hitRate", "metric", "RETRIEVAL HIT RATE", 3),
                p("citations", "metric", "AVG CITATIONS", 2),
                p("groundingSeries", "line", "GROUNDING OVER TIME", 12),
                p("flow", "chord", "RETRIEVAL FLOW · QUERY → CHUNK", 12),
                p(
                    "attention",
                    "attention",
                    "SAMPLE ANSWER · TOKEN ATTENTION",
                    12,
                ),
                p("retrievalSource", "dist", "BY RETRIEVAL SOURCE", 12),
                p("lowGrounding", "topn", "LOWEST GROUNDING PROMPTS", 6),
                p(
                    "chunkDensity",
                    "heatmap",
                    "CHUNK HIT DENSITY · QUERY TYPE × CHUNK",
                    6,
                ),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "vector-index",
            name: "Vector · Index",
            section: Some("dashboards"),
            group: Some("ai"),
            panels: vec![
                p("vectors", "metric", "VECTORS", 3),
                p("dim", "metric", "DIMENSIONS", 2),
                p("disk", "metric", "ON DISK", 2),
                p("qps", "metric", "QUERIES/s", 2),
                p("recall", "gauge", "RECALL @ 10", 3),
                p(
                    "embedSpace",
                    "embedspace",
                    "EMBEDDING SPACE · UMAP PROJECTION",
                    12,
                ),
                p(
                    "annLatency",
                    "ribbon3d",
                    "ANN LATENCY · P50 / P95 / P99",
                    12,
                ),
                p(
                    "pcoords",
                    "pcoords",
                    "QUERY PROFILE · PARALLEL COORDINATES",
                    12,
                ),
                p("models", "topn", "EMBEDDING MODELS", 6),
                p("p95Spark", "metric", "p95 LATENCY", 3),
                p("recallTimeline", "metric", "RECALL OVER TIME", 3),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "agent-memory",
            name: "Agent · Memory",
            section: Some("dashboards"),
            group: Some("ai"),
            panels: vec![
                p("entries", "metric", "MEMORY ENTRIES", 4),
                p("dedup", "metric", "DEDUP RATE", 2),
                p("recall", "metric", "RECALL P95", 2),
                p("growth", "metric", "GROWTH", 2),
                p("agents", "metric", "AGENTS", 2),
                p("sizeSeries", "line", "MEMORY SIZE OVER TIME", 12),
                p(
                    "embedSpace",
                    "embedspace",
                    "SEMANTIC MEMORY · ONCALL-TRIAGE · UMAP",
                    12,
                ),
                p("byAgent", "topn", "BY AGENT", 6),
                p("topMemories", "topn", "MOST-REFERENCED MEMORIES", 6),
                p("dedupSeries", "line", "DEDUP RATE OVER TIME", 6),
                p("recallSeries", "line", "RECALL P95 OVER TIME", 6),
                p("recentOps", "table", "RECENT OPERATIONS", 12),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        // ── Logs group ──────────────────────────────────────────────────────
        DashboardSpec {
            registry_id: "logs-overview",
            name: "Logs",
            section: Some("dashboards"),
            group: Some("logs"),
            panels: vec![
                p("total", "metric", "TOTAL EVENTS", 4),
                p("peak", "metric", "PEAK RATE", 3),
                p("errRate", "metric", "ERROR RATE", 2),
                p("sources", "metric", "SOURCES", 3),
                p("series", "line", "EVENTS OVER TIME", 12),
                p("levels", "dist", "BY LEVEL", 12),
                p("topServices", "topn", "TOP SERVICES · CLICK TO FILTER", 6),
                pd(
                    "topHosts",
                    "topn",
                    "TOP HOSTS · CLICK TO DRILL",
                    6,
                    "system",
                ),
                p("heatmap", "heatmap", "INTENSITY · WEEKDAY × 2H", 12),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "anomaly-detect",
            name: "Anomaly",
            section: Some("dashboards"),
            group: Some("logs"),
            panels: vec![
                p("detected", "metric", "ANOMALIES · LAST PERIOD", 4),
                p("covered", "metric", "SIGNALS SCORED", 2),
                p("falsePos", "metric", "FALSE-POSITIVE RATE", 3),
                p("recall", "metric", "RECALL vs HAND LABELS", 3),
                p(
                    "band",
                    "anomalyband",
                    "QUERY LATENCY · NORMAL BAND · μ ± 2.5σ",
                    12,
                ),
                p(
                    "topSignals",
                    "topn",
                    "MOST-ANOMALOUS SIGNALS · BY z-SCORE",
                    6,
                ),
                p("features", "topn", "FEATURE ATTRIBUTION · TOP ANOMALY", 6),
                p(
                    "pcoords",
                    "pcoords",
                    "SIGNAL PROFILE · PARALLEL COORDINATES",
                    12,
                ),
                p("cause", "treemap", "ROOT-CAUSE CANDIDATES · RANKED", 6),
                p(
                    "trace",
                    "attention",
                    "CORRELATED LOG · ATTENTION EXPLAIN",
                    6,
                ),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "ingest-pipeline",
            name: "Ingest · Pipeline",
            section: Some("dashboards"),
            group: Some("logs"),
            panels: vec![
                p("docsRate", "metric", "DOCS INDEXED/s", 3),
                p("bytesRate", "metric", "BYTES WRITTEN/s", 2),
                p("walLag", "metric", "WAL WRITE LATENCY", 2),
                p("segments", "metric", "SEGMENTS", 2),
                p("mem", "metric", "MEMORY USAGE", 2),
                p("pipeline", "flowband", "PIPELINE · END-TO-END FLOW", 12),
                p("docsSeries", "line", "INGEST THROUGHPUT", 12),
                p(
                    "latency",
                    "multiples",
                    "INDEX LATENCY · p50 / p95 / p99",
                    12,
                ),
                p("flushDur", "line", "FLUSH DURATION", 6),
                p("mergeDur", "line", "MERGE DURATION", 6),
                p("topIndices", "topn", "DOCS INDEXED · BY INDEX", 6),
                p(
                    "encodings",
                    "table",
                    "FIELD ENCODINGS · /v1/indices/:name/encodings",
                    6,
                ),
                p("compressionRatio", "gauge", "COMPRESSION RATIO", 6),
                p("memSeries", "line", "MEMORY OVER TIME", 6),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        // ── Infra group ─────────────────────────────────────────────────────
        DashboardSpec {
            registry_id: "system",
            name: "System",
            section: Some("dashboards"),
            group: Some("infra"),
            panels: vec![
                p("hosts", "metric", "HOSTS", 3),
                p("alerts", "metric", "ACTIVE ALERTS", 3),
                p("cpuMean", "metric", "MEAN CPU", 3),
                p("memMean", "metric", "MEAN MEM", 3),
                p("cpu", "line", "CPU", 6),
                p("mem", "line", "MEMORY", 6),
                p("disk", "line", "DISK I/O", 6),
                p("net", "line", "NETWORK I/O", 6),
                p("hostCpu", "multiples", "PER-HOST CPU · SMALL MULTIPLES", 12),
                p("topProcs", "topn", "TOP PROCESSES", 6),
                p("topHosts", "topn", "HOSTS BY LOAD", 6),
                p("authSeries", "line", "AUTH · FAILED LOGINS", 12),
                p("topFailUsers", "topn", "TOP FAILED USERS", 6),
                p("topFailIPs", "topn", "TOP ATTACKING IPS", 6),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        // ── Section views (one per top-level section) ───────────────────────
        DashboardSpec {
            registry_id: "search-discover",
            name: "Search · Discover",
            section: Some("discover"),
            group: None,
            panels: vec![
                p(
                    "searchbox",
                    "searchbox",
                    "QUERY · TYPE · INDEX · FILTERS",
                    12,
                ),
                p(
                    "hits",
                    "hits",
                    "RESULTS · CLICK A COLUMN TO SORT · CLICK INDEX TO FILTER",
                    8,
                ),
                p("facets", "facet", "FACETS · CLICK TO FILTER", 4),
                p("histogram", "bar", "DATE_HISTOGRAM · INTERVAL=1H", 8),
                p("searchMetrics", "metric", "INDEX · LIVE", 4),
                p("dsl", "markdown", "REQUEST · POST /v1/indices/*/search", 6),
                p("plan", "plan", "QUERY PLAN · FROM EXPLAIN-PLAN ENDPOINT", 6),
                p("qps", "line", "QUERIES/s OVER TIME", 6),
                p("latency", "line", "p95 LATENCY OVER TIME", 6),
                p(
                    "citations",
                    "citations",
                    "WHY THIS PANEL EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "alerts",
            name: "Alerts",
            section: Some("alerts"),
            group: None,
            panels: vec![
                p("active", "metric", "ACTIVE FIRES", 3),
                p("silenced", "metric", "SILENCED", 2),
                p("rules", "metric", "RULES DEFINED", 2),
                p("fires", "metric", "FIRES · 24H", 2),
                p("connectors", "metric", "CONNECTORS", 3),
                p("firesOverTime", "line", "FIRES OVER TIME", 12),
                p("bySev", "dist", "BY SEVERITY", 12),
                p("topNoisy", "topn", "TOP NOISY RULES · FIRES / 24H", 6),
                p("recent", "events", "RECENT · LAST 30 EVENTS", 6),
                p("ruleAsCode", "markdown", "RULES AS CODE · EXAMPLE", 12),
                p(
                    "citations",
                    "citations",
                    "WHY THIS SECTION EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "data",
            name: "Data",
            section: Some("data"),
            group: None,
            panels: vec![
                p(
                    "clusters",
                    "clusters",
                    "CLUSTERS · CLICK TO SET DEFAULT",
                    12,
                ),
                p(
                    "indices",
                    "indices",
                    "INDICES · CLICK AN INDEX TO INSPECT FIELDS",
                    6,
                ),
                p(
                    "fields",
                    "fields",
                    "FIELDS · FROM /v1/indices/:name/_mapping",
                    6,
                ),
                p("howTo", "markdown", "CONNECTING A NEW CLUSTER", 12),
                p(
                    "citations",
                    "citations",
                    "WHY THIS SECTION EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "users",
            name: "Users",
            section: Some("users"),
            group: None,
            panels: vec![
                p("users", "metric", "USERS", 3),
                p("roles", "metric", "ROLES", 2),
                p("apiKeys", "metric", "API KEYS", 2),
                p("sessions", "metric", "SESSIONS", 2),
                p("lastLogin", "metric", "LAST LOGIN", 3),
                p("userList", "topn", "USERS · MOST ACTIVE", 6),
                p("roles", "table", "ROLES · INDEX PREFIX × OPS", 6),
                p("recent", "events", "RECENT AUTH EVENTS", 12),
                p(
                    "model",
                    "markdown",
                    "THE PERMISSION MODEL · WHY IT IS LIKE THIS",
                    12,
                ),
                p(
                    "citations",
                    "citations",
                    "WHY THIS SECTION EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
        DashboardSpec {
            registry_id: "settings",
            name: "Settings",
            section: Some("settings"),
            group: None,
            panels: vec![
                p("defaults", "settings", "DEFAULTS", 12),
                p("mg-dashboards-head", "markdown", "", 12),
                p("mg-dashboards", "manage-dashboards", "", 12),
                p("mg-new", "manage-new", "", 12),
                p("mg-views-head", "markdown", "", 12),
                p("mg-views", "manage-views", "", 12),
                p("storage", "markdown", "PERSISTENT STATE INVENTORY", 12),
                p("danger", "danger", "DANGER · WIPE ALL STATE", 12),
                p(
                    "citations",
                    "citations",
                    "WHY THIS SECTION EXISTS · USER FEEDBACK",
                    12,
                ),
            ],
        },
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Skeleton → JSON
// ─────────────────────────────────────────────────────────────────────────────

/// Default row-unit height for a panel type. Small stat tiles are short;
/// full-bleed charts (embedding spaces, ribbons, heatmaps) are tall. Chosen
/// once so the seeded layout reads sensibly; the operator resizes freely
/// from there.
fn height_for(kind: &str) -> u32 {
    match kind {
        "metric" | "gauge" | "spark" => 2,
        "citations" => 3,
        "heatmap" | "ribbon3d" | "embedspace" | "chord" | "pcoords" | "attention" | "flowband"
        | "multiples" | "treemap" | "scatter" | "stacked" | "anomalyband" => 6,
        // line, topn, table, events, dist, bar, histogram, markdown, and the
        // section-specific view types (hits, facet, plan, clusters, …).
        _ => 4,
    }
}

/// Flow the panel specs into a free-form 12-col grid, left-to-right, wrapping
/// to a new row when the next panel would overflow.  Advancing `y` by the
/// tallest panel in the finished row guarantees no two panels overlap.  Also
/// de-duplicates panel ids *within* a dashboard (`registry.js` sources reuse
/// e.g. `citations` twice) by suffixing `-2`, `-3`, … so every panel id — and
/// therefore every `builtin` provenance key — is unique.
fn build_panels(registry_id: &str, panels: &[PanelSpec]) -> Vec<Value> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    let mut out = Vec::with_capacity(panels.len());
    let (mut x, mut y, mut row_h) = (0u32, 0u32, 0u32);

    for spec in panels {
        let w = spec.cols.clamp(1, 12);
        let h = height_for(spec.kind);
        if x + w > 12 {
            x = 0;
            y += row_h.max(1);
            row_h = 0;
        }

        let count = seen.entry(spec.id).or_insert(0);
        *count += 1;
        let unique_id = if *count == 1 {
            spec.id.to_string()
        } else {
            format!("{}-{}", spec.id, *count)
        };

        let drilldown = match spec.drilldown_to {
            Some(to) => json!({ "to": to }),
            None => Value::Null,
        };

        out.push(json!({
            "id": unique_id,
            "type": spec.kind,
            "title": spec.title,
            "layout": { "x": x, "y": y, "w": w, "h": h },
            "query": Value::Null,
            "viz": {},
            "drilldown": drilldown,
            "builtin": format!("{registry_id}/{unique_id}"),
        }));

        x += w;
        row_h = row_h.max(h);
    }
    out
}

/// Build the full [`crate::dashboards::Dashboard`]-shaped JSON for one spec.
fn build_seed_doc(spec: &DashboardSpec, now: &str) -> Value {
    let id = format!("default-{}", spec.registry_id);
    json!({
        "id": id,
        "owner": "system",
        "org_id": "default",
        "visibility": "default",
        "managed": true,
        "name": spec.name,
        "section": spec.section,
        "group": spec.group,
        "cloned_from": Value::Null,
        "panels": build_panels(spec.registry_id, &spec.panels),
        "filters_default": {},
        "time_default": Value::Null,
        "version": 1,
        "created_at": now,
        "updated_at": now,
        "deleted_at": Value::Null,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Seeding entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Read whether a seeded default row is still pristine (never edited, never
/// deleted).  Editing bumps `version` past 1; a soft-delete sets `deleted_at`
/// (and also bumps `version`).  Either makes it off-limits to re-seed.
fn is_untouched_default(doc: &Value) -> bool {
    let version = doc.get("version").and_then(Value::as_u64).unwrap_or(1);
    let deleted = doc.get("deleted_at").is_some_and(|v| !v.is_null());
    version <= 1 && !deleted
}

/// Idempotently materialise the built-in dashboards as editable backend data.
///
/// Call once from [`crate::bootstrap::run`], right after
/// [`crate::indices::ensure_all`].  Safe on every boot — see the module docs
/// for the create/skip/migrate contract.
pub async fn seed_default_dashboards(engine: &Engine) -> ConsoleResult<()> {
    let idx = engine.get_index(indices::DASHBOARDS)?;
    let now = now_iso();

    // A higher shipped revision than the recorded one authorises re-upserting
    // *untouched* defaults onto fresh skeletons. On a brand-new data dir the
    // recorded revision is absent (→ migrate), but every doc is absent too,
    // so this only ever matters for pre-existing installs after a bump.
    let stored_rev = match idx.get_document(SEED_META_ID).await? {
        Some(meta) => meta.get("seed_revision").and_then(Value::as_u64),
        None => None,
    };
    let migrate = stored_rev.is_none_or(|r| SEED_REVISION > r);

    let mut created = 0u32;
    let mut migrated = 0u32;
    for spec in seed_specs() {
        let id = format!("default-{}", spec.registry_id);
        match idx.get_document(&id).await? {
            None => {
                idx.index_document(Some(id), build_seed_doc(&spec, &now))
                    .await?;
                created += 1;
            }
            Some(existing) => {
                if migrate && is_untouched_default(&existing) {
                    idx.index_document(Some(id), build_seed_doc(&spec, &now))
                        .await?;
                    migrated += 1;
                }
                // Otherwise: present + (edited | deleted | same revision) →
                // never clobber.
            }
        }
    }

    // Record the shipped revision so the next boot is a no-op and a future
    // bump migrates exactly once.
    idx.index_document(
        Some(SEED_META_ID.to_string()),
        json!({
            "kind": "seed_meta",
            "seed_revision": SEED_REVISION,
            "updated_at": now,
        }),
    )
    .await?;

    // Make the seeded set durable + immediately searchable so the first
    // LIST after boot returns the defaults.
    idx.flush().await?;

    if created > 0 || migrated > 0 {
        tracing::info!(
            created,
            migrated,
            revision = SEED_REVISION,
            "seeded default dashboards"
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests (pure — no engine needed)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_the_thirteen_dashboards() {
        let specs = seed_specs();
        assert_eq!(
            specs.len(),
            13,
            "must seed exactly the 13 registry dashboards"
        );
        // Deterministic ids, all `default-` prefixed and unique.
        let mut ids: Vec<String> = specs
            .iter()
            .map(|s| format!("default-{}", s.registry_id))
            .collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "seed ids must be unique");
        assert!(ids.iter().all(|id| id.starts_with("default-")));
        assert!(ids.contains(&"default-ai-overview".to_string()));
        assert!(ids.contains(&"default-settings".to_string()));
    }

    #[test]
    fn taxonomy_matches_registry() {
        let by = |rid: &str| -> (Option<String>, Option<String>) {
            let s = seed_specs();
            let d = s.iter().find(|d| d.registry_id == rid).unwrap();
            (d.section.map(String::from), d.group.map(String::from))
        };
        assert_eq!(
            by("ai-overview"),
            (Some("dashboards".into()), Some("ai".into()))
        );
        assert_eq!(
            by("logs-overview"),
            (Some("dashboards".into()), Some("logs".into()))
        );
        assert_eq!(
            by("system"),
            (Some("dashboards".into()), Some("infra".into()))
        );
        assert_eq!(by("search-discover"), (Some("discover".into()), None));
        assert_eq!(by("alerts"), (Some("alerts".into()), None));
        assert_eq!(by("settings"), (Some("settings".into()), None));
    }

    #[test]
    fn panel_ids_unique_within_each_dashboard() {
        // registry.js reuses ids like `citations` / `roles`; the builder must
        // disambiguate so every panel id (and builtin key) is unique.
        for spec in seed_specs() {
            let panels = build_panels(spec.registry_id, &spec.panels);
            let mut ids: Vec<&str> = panels.iter().map(|p| p["id"].as_str().unwrap()).collect();
            let raw = ids.len();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(
                ids.len(),
                raw,
                "duplicate panel id in dashboard `{}`",
                spec.registry_id
            );
        }
        // Spot-check the known collisions were suffixed.
        let rag = build_panels("rag-quality", &spec_panels("rag-quality"));
        let rag_ids: Vec<&str> = rag.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert!(rag_ids.contains(&"citations"));
        assert!(rag_ids.contains(&"citations-2"));
    }

    #[test]
    fn builtin_key_tracks_panel_id() {
        for spec in seed_specs() {
            let panels = build_panels(spec.registry_id, &spec.panels);
            for panel in &panels {
                let id = panel["id"].as_str().unwrap();
                let builtin = panel["builtin"].as_str().unwrap();
                assert_eq!(builtin, format!("{}/{}", spec.registry_id, id));
                // Seed panels are mock-backed: query null, builtin present.
                assert!(panel["query"].is_null(), "seed panels carry no query");
            }
        }
    }

    #[test]
    fn layout_is_nonoverlapping_and_within_grid() {
        for spec in seed_specs() {
            let panels = build_panels(spec.registry_id, &spec.panels);
            let rects: Vec<(u32, u32, u32, u32)> = panels
                .iter()
                .map(|p| {
                    let l = &p["layout"];
                    (
                        l["x"].as_u64().unwrap() as u32,
                        l["y"].as_u64().unwrap() as u32,
                        l["w"].as_u64().unwrap() as u32,
                        l["h"].as_u64().unwrap() as u32,
                    )
                })
                .collect();
            for &(x, _, w, _) in &rects {
                assert!(
                    w >= 1 && x + w <= 12,
                    "panel out of 12-col grid in {}",
                    spec.registry_id
                );
            }
            for i in 0..rects.len() {
                for j in (i + 1)..rects.len() {
                    let (ax, ay, aw, ah) = rects[i];
                    let (bx, by, bw, bh) = rects[j];
                    let x_overlap = ax < bx + bw && bx < ax + aw;
                    let y_overlap = ay < by + bh && by < ay + ah;
                    assert!(
                        !(x_overlap && y_overlap),
                        "overlapping panels {i}/{j} in {}",
                        spec.registry_id
                    );
                }
            }
        }
    }

    #[test]
    fn seed_doc_deserializes_as_a_dashboard() {
        // The seeded JSON must round-trip through the CRUD struct, or list/get
        // would silently drop the defaults.
        for spec in seed_specs() {
            let doc = build_seed_doc(&spec, "2026-07-13T00:00:00.000Z");
            let dash: crate::dashboards::Dashboard =
                serde_json::from_value(doc).expect("seed doc must parse as Dashboard");
            assert_eq!(dash.owner, "system");
            assert_eq!(dash.visibility, "default");
            assert!(dash.managed);
            assert_eq!(dash.version, 1);
            assert!(!dash.panels.is_empty());
        }
    }

    #[test]
    fn seed_meta_is_not_a_dashboard() {
        // The bookkeeping row must fail Dashboard parse so list/get skip it.
        let meta = json!({ "kind": "seed_meta", "seed_revision": SEED_REVISION });
        assert!(serde_json::from_value::<crate::dashboards::Dashboard>(meta).is_err());
    }

    #[test]
    fn untouched_predicate() {
        assert!(is_untouched_default(
            &json!({ "version": 1, "deleted_at": Value::Null })
        ));
        assert!(!is_untouched_default(&json!({ "version": 2 })));
        assert!(!is_untouched_default(
            &json!({ "version": 1, "deleted_at": "2026-07-13T00:00:00.000Z" })
        ));
    }

    /// helper: fetch one dashboard's panel specs by registry id (test-only).
    fn spec_panels(rid: &str) -> Vec<PanelSpec> {
        seed_specs()
            .into_iter()
            .find(|s| s.registry_id == rid)
            .unwrap()
            .panels
    }
}
