//! Router construction for both API surfaces.
//!
//! Two separate routers are built:
//! - [`build_native_router`] — mounted on `:8080`, native xerj API.
//! - [`build_es_compat_router`] — mounted on `:9200`, ES-compatible API.
//!
//! Both routers share the same [`AppState`] instance via `Arc`. Common
//! middleware (request-ID injection, per-request tracing, CORS) is applied
//! to both.

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, Method},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post, put},
    Router,
};
use tower_http::{
    classify::{ServerErrorsAsFailures, SharedClassifier},
    cors::{AllowOrigin, Any, CorsLayer},
    decompression::RequestDecompressionLayer,
    limit::RequestBodyLimitLayer,
    trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer},
};
use tracing::Level;
use uuid::Uuid;
use xerj_common::config::CorsConfig;

use crate::{
    auth::auth_middleware, authz, es_compat, graph_api, memory_api, native, state::AppState,
};

// ─────────────────────────────────────────────────────────────────────────────
// Native xerj router (:8080)
// ─────────────────────────────────────────────────────────────────────────────

/// Build the native xerj router.
///
/// Routes:
/// ```text
/// POST   /v1/indices                        create_index
/// GET    /v1/indices/:name                  get_index
/// DELETE /v1/indices/:name                  delete_index
/// POST   /v1/indices/:name/docs             ingest_docs
/// GET    /v1/indices/:name/docs/:id         get_doc
/// DELETE /v1/indices/:name/docs/:id         delete_doc
/// POST   /v1/indices/:name/docs/_bulk       bulk_ingest
/// POST   /v1/indices/:name/turbo-ingest     turbo_ingest (opt-in high-throughput)
/// GET    /v1/indices/:name/encodings        get_index_encodings (per-field compression stats)
/// POST   /v1/indices/:name/search           search
/// POST   /v1/indices/:name/_flush           flush_index
/// GET    /v1/health                         health
/// GET    /v1/health/ready                   readiness probe (200 unless red)
/// GET    /v1/cluster/health                 cluster_health
/// POST   /v1/admin/flush                    admin_flush (flush all indices)
/// POST   /v1/admin/backup                   admin_backup (snapshot to disk)
/// GET    /v1/metrics                        metrics (Prometheus text)
/// GET    /v1/schema/:name                   get_schema
/// POST   /v1/schema/:name/evolve            evolve_schema
/// ```
pub fn build_native_router(state: AppState) -> Router {
    let body_limit = state.config.limits.max_body_bytes;
    let access_log = state.config.logging.access_log;
    // Build the CORS layer before `state` is moved into the auth middleware.
    let cors = cors_layer(&state.config.cors);
    // Note: this router does NOT serve `/_xerj-console/*` — that namespace is
    // owned by the `xerj-console-api` crate, mounted by `xerj-server` as a
    // peer router via `Router::merge`. Engine API and Xerj Console API are
    // independent surfaces sharing one TCP listener.
    Router::new()
        // Index management
        .route("/v1/indices", post(native::create_index))
        .route(
            "/v1/indices/:name",
            get(native::get_index).delete(native::delete_index),
        )
        // Document operations
        .route("/v1/indices/:name/docs", post(native::ingest_docs))
        .route(
            "/v1/indices/:name/docs/:id",
            get(native::get_doc).delete(native::delete_doc),
        )
        // Bulk ingest
        .route("/v1/indices/:name/docs/_bulk", post(native::bulk_ingest))
        // Turbo ingest — high-throughput batched parallel ingest (opt-in)
        .route("/v1/indices/:name/turbo-ingest", post(native::turbo_ingest))
        // Log ingest (auto-ID for every record)
        .route("/v1/indices/:name/logs", post(native::ingest_logs))
        // OTLP log ingest (OpenTelemetry JSON format)
        .route("/v1/indices/:name/otlp", post(native::ingest_otlp))
        // Syslog ingest (RFC 5424 / RFC 3164 plain-text lines)
        .route("/v1/indices/:name/syslog", post(native::ingest_syslog))
        // Search
        .route("/v1/indices/:name/search", post(native::search))
        // Per-field smart encoding analysis
        .route(
            "/v1/indices/:name/encodings",
            get(native::get_index_encodings),
        )
        // Flush memtable to durable segment
        .route("/v1/indices/:name/_flush", post(native::flush_index))
        // Cluster / observability
        .route("/v1/health", get(native::health))
        .route("/v1/health/ready", get(native::readiness))
        .route("/v1/cluster/health", get(native::cluster_health))
        .route(
            "/v1/embedding/identity",
            get(native::embedding_execution_identity),
        )
        // k8s probes — v0.8 8-P4 — see comment in `native.rs::liveness`.
        .route("/health/live", get(native::liveness))
        .route("/health/ready", get(native::readiness))
        .route("/v1/metrics", get(native::metrics))
        // Admin: cluster-wide flush + backup (snapshot to disk)
        .route("/v1/admin/flush", post(native::admin_flush))
        .route("/v1/admin/backup", post(native::admin_backup))
        // Admin: slow query log — v0.8 8-P6
        .route(
            "/v1/admin/slow_queries",
            get(native::admin_slow_queries).delete(native::admin_slow_queries_clear),
        )
        .route(
            "/v1/admin/slow_queries/threshold/:ms",
            put(native::admin_slow_queries_set_threshold),
        )
        // v0.9 9-P4 — Audit log
        .route("/_audit/_search", get(native::audit_search))
        .route("/_audit/_verify", get(native::audit_verify))
        // v0.9 9-P2 — RBAC roles
        .route("/_security/roles", get(native::rbac_list_roles))
        .route(
            "/_security/role/:name",
            get(native::rbac_get_role)
                .put(native::rbac_put_role)
                .delete(native::rbac_delete_role),
        )
        // (Xerj Console SPA + API — `/_xerj-console/*` — is served by xerj-console-api,
        // merged at the server level. See xerj-server/src/main.rs.)
        // Dashboard — overview of all indices (first step toward built-in UI)
        .route("/v1/dashboard/summary", get(native::dashboard_summary))
        // Enrich policies — register lookup table for field enrichment at ingest
        .route("/v1/indices/:name/enrich", post(native::enrich_index))
        // Explain plan — return query execution plan without executing it
        .route("/v1/indices/:name/explain-plan", post(native::explain_plan))
        // Schema management
        .route("/v1/schema/:name", get(native::get_schema))
        .route("/v1/schema/:name/evolve", post(native::evolve_schema))
        // Transform pipeline management
        .route("/v1/pipelines/:name", put(native::put_pipeline))
        // Ingest with pipeline transformation
        .route(
            "/v1/indices/:name/ingest",
            post(native::ingest_with_pipeline),
        )
        // Shared state
        .with_state(state.clone())
        // Middleware stack (applied outermost-last)
        //
        // The native router reaches the same indices under a different
        // spelling (`/v1/indices/{name}/…`), so it carries the same
        // authorization layer — otherwise the reserved `.xerj-memory-*`
        // namespace would be closed on :9200 and open on :8080.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authz::authz_middleware,
        ))
        .layer(middleware::from_fn_with_state(state, auth_middleware))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(trace_layer(access_log))
        .layer(cors)
        // Reject requests with a body larger than the configured limit (OOM guard).
        // Disable axum's built-in 2MB default so RequestBodyLimitLayer is the only
        // gate — bulk ingests legitimately exceed 2MB.
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(body_limit))
        // Outermost: transparently decompress gzip-encoded request bodies
        // (Content-Encoding: gzip) before the size limit above sees them, so
        // the limit bounds decompressed size, not wire size. Real ES clients
        // routinely compress writes by default (Beats, Logstash, official
        // client libraries) — without this every such request 400s/500s
        // trying to JSON-parse raw gzip bytes.
        .layer(RequestDecompressionLayer::new().gzip(true))
}

// ─────────────────────────────────────────────────────────────────────────────
// ES-compatible router (:9200)
// ─────────────────────────────────────────────────────────────────────────────

/// Build the Elasticsearch-compatible router.
///
/// Routes mirror the ES 8.x REST API surface:
/// ```text
/// GET    /                                  es_info
/// GET    /_cluster/health                   cluster_health
/// GET    /_cat/indices                      cat_indices
/// GET    /_cat/indices/:pattern             cat_indices_pattern
/// POST   /_bulk                             global_bulk
/// PUT    /:index                            create_index
/// DELETE /:index                            delete_index
/// GET    /:index                            get_index
/// PUT    /:index/_mapping                   put_mapping
/// GET    /:index/_mapping                   get_mapping
/// GET    /:index/_settings                  get_settings
/// POST   /:index/_doc                       index_doc_auto (auto ID)
/// PUT    /:index/_doc/:id                   index_doc (explicit ID)
/// GET    /:index/_doc/:id                   get_doc
/// DELETE /:index/_doc/:id                   delete_doc
/// POST   /:index/_search                    search
/// GET    /:index/_search                    search (GET with body)
/// POST   /:index/_bulk                      bulk_ops
/// ```
pub fn build_es_compat_router(state: AppState) -> Router {
    let body_limit = state.config.limits.max_body_bytes;
    let access_log = state.config.logging.access_log;
    // Build the CORS layer before `state` is moved into the auth middleware.
    let cors = cors_layer(&state.config.cors);
    Router::new()
        // (Xerj Console SPA + API — `/_xerj-console/*` — is served by xerj-console-api,
        // merged at the server level. See xerj-server/src/main.rs.)
        // ── k8s probes (mounted here too so prod operators can hit them
        // ── on the ES-compat port without knowing about the native router).
        // ── See `native.rs::liveness` for design rationale.
        .route("/health/live", get(native::liveness))
        .route("/health/ready", get(native::readiness))
        // ── Prometheus scrape (RC4-W4 item 4) ──────────────────────────────
        // Mounted on the ES-compat port too so `:9200`-only deployments (the
        // common Elastic-drop-in posture, where the native `:8080` listener is
        // firewalled off) can still scrape metrics. Same handler as the native
        // router; gated by the optional read-only `auth.metrics_token`.
        .route("/v1/metrics", get(native::metrics))
        // Autoindex defaults to the ES-compatible listener, so the native
        // embedding identity contract must be available here as well as on
        // the dedicated native listener.
        .route(
            "/v1/embedding/identity",
            get(native::embedding_execution_identity),
        )
        // ── Cluster-level ──────────────────────────────────────────────────
        .route("/", get(es_compat::es_info))
        .route("/_cluster/health", get(es_compat::cluster_health))
        .route(
            "/_cluster/health/:index",
            get(es_compat::cluster_health_for_index),
        )
        .route("/_cat/indices", get(es_compat::cat_indices))
        .route(
            "/_cat/indices/:pattern",
            get(es_compat::cat_indices_pattern),
        )
        // XERJ extension (RC4-W4 item 7): per-index ANN/HNSW health as a _cat
        // table so a brute-force-degraded (stale / partially-covered) vector
        // index stops being silent. No ES equivalent, so it can't collide with
        // the parity surface.
        .route("/_cat/ann", get(es_compat::cat_ann))
        .route("/_cat/ann/:index", get(es_compat::cat_ann_index))
        .route("/_cat/health", get(es_compat::cat_health))
        .route("/_cat/nodes", get(es_compat::cat_nodes))
        .route("/_cat/aliases", get(es_compat::cat_aliases))
        .route("/_cat/count/:index", get(es_compat::cat_count))
        .route("/_cat/shards", get(es_compat::cat_shards))
        .route("/_bulk", post(es_compat::global_bulk))
        .route("/_mget", post(es_compat::mget))
        // Index-scoped multi-get — the path index defaults entries that omit `_index`.
        .route(
            "/:index/_mget",
            get(es_compat::mget_index).post(es_compat::mget_index),
        )
        // ── Index management ───────────────────────────────────────────────
        .route(
            "/:index",
            put(es_compat::create_index)
                .delete(es_compat::delete_index)
                .get(es_compat::get_index)
                .head(es_compat::head_index),
        )
        // ── Mapping & settings ─────────────────────────────────────────────
        .route("/_mapping", get(es_compat::get_mapping_all))
        .route("/_settings", get(es_compat::get_settings_all))
        .route(
            "/:index/_mapping",
            put(es_compat::put_mapping).get(es_compat::get_mapping),
        )
        .route(
            "/:index/_mapping/field/:field",
            get(es_compat::get_mapping_field),
        )
        .route(
            "/:index/_settings",
            get(es_compat::get_settings).put(es_compat::put_settings),
        )
        // ── Index blocks ───────────────────────────────────────────────────
        .route(
            "/:index/_block/:block",
            put(es_compat::put_index_block).delete(es_compat::delete_index_block),
        )
        // ── Explain ────────────────────────────────────────────────────────
        .route(
            "/:index/_explain/:id",
            get(es_compat::explain_doc).post(es_compat::explain_doc),
        )
        .route("/:index/_stats", get(es_compat::index_stats))
        .route("/:index/_disk_usage", post(es_compat::index_disk_usage))
        .route(
            "/:index/_count",
            get(es_compat::count_docs).post(es_compat::count_docs),
        )
        .route(
            "/_count",
            get(es_compat::count_docs_global).post(es_compat::count_docs_global),
        )
        .route(
            "/:index/_refresh",
            post(es_compat::refresh_index).get(es_compat::refresh_index),
        )
        .route(
            "/:index/_analyze",
            post(es_compat::analyze_text).get(es_compat::analyze_text),
        )
        .route(
            "/_analyze",
            post(es_compat::analyze_text_global).get(es_compat::analyze_text_global),
        )
        // ── Document operations ────────────────────────────────────────────
        .route("/:index/_doc", post(es_compat::index_doc_auto))
        // POST with an explicit id is the same index op as PUT (auto-create
        // included) in ES — it was missing here and returned 405.
        .route(
            "/:index/_doc/:id",
            put(es_compat::index_doc)
                .post(es_compat::index_doc)
                .get(es_compat::get_doc)
                .delete(es_compat::delete_doc)
                .head(es_compat::head_doc),
        )
        // PUT /{index}/_create/{id} — create-only (fail with 409 if doc exists)
        .route("/:index/_create/:id", put(es_compat::create_doc))
        .route("/:index/_update/:id", post(es_compat::update_doc))
        // ── Delete/Update by query ─────────────────────────────────────────
        .route("/:index/_delete_by_query", post(es_compat::delete_by_query))
        .route("/:index/_update_by_query", post(es_compat::update_by_query))
        // ── Search ─────────────────────────────────────────────────────────
        .route(
            "/_search",
            post(es_compat::search_all).get(es_compat::search_all),
        )
        .route(
            "/:index/_search",
            post(es_compat::search).get(es_compat::search),
        )
        // ── Validate query ─────────────────────────────────────────────────
        .route(
            "/:index/_validate/query",
            post(es_compat::validate_query).get(es_compat::validate_query),
        )
        // ── Bulk ───────────────────────────────────────────────────────────
        .route("/:index/_bulk", post(es_compat::bulk_ops))
        // ── Aliases ────────────────────────────────────────────────────────
        .route(
            "/_aliases",
            post(es_compat::post_aliases).get(es_compat::get_aliases),
        )
        // ── Index Templates ────────────────────────────────────────────────
        .route(
            "/_index_template/:name",
            put(es_compat::put_index_template)
                .get(es_compat::get_index_template)
                .delete(es_compat::delete_index_template),
        )
        // ── Scroll ─────────────────────────────────────────────────────────
        .route(
            "/:index/_search_scroll",
            post(es_compat::search_with_scroll).get(es_compat::search_with_scroll),
        )
        .route(
            "/_search/scroll",
            post(es_compat::next_scroll).delete(es_compat::clear_scroll),
        )
        // ── Reindex ────────────────────────────────────────────────────────
        .route("/_reindex", post(es_compat::reindex))
        // ── Field Capabilities ─────────────────────────────────────────────
        .route(
            "/:index/_field_caps",
            get(es_compat::field_caps).post(es_compat::field_caps),
        )
        // ── Multi-search ───────────────────────────────────────────────────
        .route(
            "/_msearch",
            post(es_compat::msearch).get(es_compat::msearch),
        )
        // Index-scoped multi-search — the path index defaults header lines
        // that omit `index`.
        .route(
            "/:index/_msearch",
            get(es_compat::msearch_index).post(es_compat::msearch_index),
        )
        // ── Resolve index (wildcards) ──────────────────────────────────────
        .route("/_resolve/index/:name", get(es_compat::resolve_index))
        // ── Node & cluster stats ───────────────────────────────────────────
        .route("/_nodes/stats", get(es_compat::nodes_stats))
        .route("/_cluster/stats", get(es_compat::cluster_stats))
        // ── Tasks ──────────────────────────────────────────────────────────
        .route("/_tasks", get(es_compat::get_tasks))
        .route("/_tasks/:task_id", get(es_compat::get_task_by_id))
        .route("/_tasks/:task_id/_cancel", post(es_compat::cancel_task))
        // ── Cat templates ──────────────────────────────────────────────────
        .route("/_cat/templates", get(es_compat::cat_templates))
        .route(
            "/_cat/templates/:pattern",
            get(es_compat::cat_templates_pattern),
        )
        // ── Ingest pipelines ───────────────────────────────────────────────
        .route(
            "/_ingest/pipeline",
            get(es_compat::get_all_ingest_pipelines),
        )
        .route(
            "/_ingest/pipeline/_simulate",
            post(es_compat::simulate_inline_pipeline),
        )
        .route(
            "/_ingest/pipeline/:id",
            put(es_compat::put_ingest_pipeline)
                .get(es_compat::get_ingest_pipeline)
                .delete(es_compat::delete_ingest_pipeline),
        )
        .route(
            "/_ingest/pipeline/:id/_simulate",
            post(es_compat::simulate_ingest_pipeline),
        )
        // ── Index open/close/forcemerge/flush/cache ────────────────────────
        .route("/:index/_close", post(es_compat::close_index))
        .route("/:index/_open", post(es_compat::open_index))
        .route("/:index/_forcemerge", post(es_compat::forcemerge))
        .route("/:index/_flush", post(es_compat::flush_index))
        .route("/_flush", post(es_compat::flush_all))
        .route("/:index/_cache/clear", post(es_compat::clear_cache))
        // ── Admin: per-section CRC32C re-validation across every segment ──
        // 200 with `corrupt_sections: 0` on healthy index; 500 on any
        // corruption so external monitors fire on hits.
        .route(
            "/:index/_admin/segments/fsck",
            post(es_compat::admin_segments_fsck),
        )
        // ── Data streams ───────────────────────────────────────────────────
        .route(
            "/_data_stream/:name",
            put(es_compat::put_data_stream)
                .get(es_compat::get_data_stream)
                .delete(es_compat::delete_data_stream),
        )
        .route("/:name/_rollover", post(es_compat::rollover_data_stream))
        // ── ILM ────────────────────────────────────────────────────────────
        // Policies here are executed, not just stored (issue #199): the
        // engine's ILM pass ages every managed index and applies the phases
        // it can genuinely perform, `_ilm/explain` shows its decisions, and
        // `_ilm/start` / `_ilm/stop` are the operator's kill switch.
        .route(
            "/_ilm/policy/:name",
            put(es_compat::put_ilm_policy)
                .get(es_compat::get_ilm_policy)
                .delete(es_compat::delete_ilm_policy),
        )
        .route("/_ilm/policy", get(es_compat::get_all_ilm_policies))
        .route("/_ilm/status", get(es_compat::get_ilm_status))
        .route("/_ilm/start", post(es_compat::start_ilm))
        .route("/_ilm/stop", post(es_compat::stop_ilm))
        .route("/:index/_ilm/explain", get(es_compat::explain_ilm))
        // Detaching has to be a first-class verb, not a settings side effect:
        // it is how an operator says "stop deleting my data", and it is
        // honoured ahead of whatever the index's own settings still say.
        .route(
            "/:index/_ilm/remove",
            post(es_compat::remove_ilm_policy_from_index),
        )
        // ── Component templates ────────────────────────────────────────────
        .route(
            "/_component_template/:name",
            put(es_compat::put_component_template)
                .get(es_compat::get_component_template)
                .delete(es_compat::delete_component_template),
        )
        // ── Cluster state ──────────────────────────────────────────────────
        .route("/_cluster/state", get(es_compat::cluster_state))
        // ── Cluster allocation explain ─────────────────────────────────────
        .route(
            "/_cluster/allocation/explain",
            get(es_compat::cluster_allocation_explain).post(es_compat::cluster_allocation_explain),
        )
        // ── Cat allocation ─────────────────────────────────────────────────
        .route("/_cat/allocation", get(es_compat::cat_allocation))
        // ── Index alias checks ─────────────────────────────────────────────
        .route(
            "/_alias",
            get(es_compat::get_all_aliases_all_indices)
                .head(es_compat::head_all_aliases_all_indices),
        )
        .route(
            "/_alias/:alias",
            get(es_compat::get_alias_all_indices).head(es_compat::head_alias_all_indices),
        )
        .route(
            "/:index/_alias",
            get(es_compat::get_index_aliases).head(es_compat::head_index_aliases),
        )
        .route(
            "/:index/_alias/:alias",
            put(es_compat::put_alias)
                .delete(es_compat::delete_alias)
                .get(es_compat::get_index_alias)
                .head(es_compat::head_index_alias),
        )
        // ── Snapshot API ───────────────────────────────────────────────────
        .route(
            "/_snapshot/:repo",
            put(es_compat::put_snapshot_repo)
                .get(es_compat::get_snapshot_repo)
                .delete(es_compat::delete_snapshot_repo),
        )
        .route(
            "/_snapshot/:repo/:snapshot",
            put(es_compat::create_snapshot).get(es_compat::get_snapshot),
        )
        .route(
            "/_snapshot/:repo/:snapshot/_restore",
            post(es_compat::restore_snapshot),
        )
        // ── Cluster settings & reroute ─────────────────────────────────────
        .route(
            "/_cluster/settings",
            get(es_compat::get_cluster_settings).put(es_compat::put_cluster_settings),
        )
        .route("/_cluster/reroute", post(es_compat::cluster_reroute))
        .route(
            "/_cluster/pending_tasks",
            get(es_compat::cluster_pending_tasks),
        )
        // ── More _cat APIs ──────────────────────────────────────────────────
        .route("/_cat/recovery", get(es_compat::cat_recovery))
        .route("/_cat/segments/:index", get(es_compat::cat_segments))
        .route("/_cat/thread_pool", get(es_compat::cat_thread_pool))
        .route("/_cat/fielddata", get(es_compat::cat_fielddata))
        .route("/_cat/pending_tasks", get(es_compat::cat_pending_tasks))
        .route("/_cat/plugins", get(es_compat::cat_plugins))
        .route("/_cat/nodeattrs", get(es_compat::cat_nodeattrs))
        .route("/_cat/master", get(es_compat::cat_master))
        // ── _nodes info ─────────────────────────────────────────────────────
        .route("/_nodes", get(es_compat::nodes_info))
        .route("/_nodes/:node_id/stats", get(es_compat::node_stats_by_id))
        // ── Index clone / shrink / split ────────────────────────────────────
        .route(
            "/:index/_clone/:target",
            post(es_compat::clone_index).put(es_compat::clone_index),
        )
        .route(
            "/:index/_shrink/:target",
            post(es_compat::shrink_index).put(es_compat::shrink_index),
        )
        .route(
            "/:index/_split/:target",
            post(es_compat::split_index).put(es_compat::split_index),
        )
        // ── Enrich Policies ─────────────────────────────────────────────────
        .route(
            "/_enrich/policy/:name",
            put(es_compat::put_enrich_policy)
                .get(es_compat::get_enrich_policy)
                .delete(es_compat::delete_enrich_policy),
        )
        .route(
            "/_enrich/policy/:name/_execute",
            post(es_compat::execute_enrich_policy),
        )
        // ── Watcher APIs ────────────────────────────────────────────────────
        .route(
            "/_watcher/watch/:id",
            put(es_compat::put_watch)
                .get(es_compat::get_watch)
                .delete(es_compat::delete_watch),
        )
        .route("/_watcher/_start", post(es_compat::start_watcher))
        .route("/_watcher/_stop", post(es_compat::stop_watcher))
        // ── Search template ─────────────────────────────────────────────────
        .route("/:index/_search/template", post(es_compat::search_template))
        // ── Multi-search template ────────────────────────────────────────────
        .route("/_msearch/template", post(es_compat::msearch_template))
        // Index-scoped multi-search template — the path index defaults header
        // lines that omit `index`.
        .route(
            "/:index/_msearch/template",
            get(es_compat::msearch_template_index).post(es_compat::msearch_template_index),
        )
        // ── Render template ──────────────────────────────────────────────────
        .route("/_render/template", post(es_compat::render_template_api))
        // ── Stored scripts/templates ─────────────────────────────────────────
        .route(
            "/_scripts/:id",
            put(es_compat::put_script)
                .get(es_compat::get_script)
                .delete(es_compat::delete_script),
        )
        .route(
            "/_scripts/painless/_execute",
            post(es_compat::painless_execute),
        )
        // ── Terms enum ───────────────────────────────────────────────────────
        .route("/:index/_terms_enum", post(es_compat::terms_enum))
        // ── X-Pack APIs (needed for Kibana compatibility) ─────────────────────
        .route("/_xpack", get(es_compat::xpack_info))
        .route("/_xpack/usage", get(es_compat::xpack_usage))
        // ── Security APIs ─────────────────────────────────────────────────────
        .route(
            "/_security/_authenticate",
            get(es_compat::security_authenticate),
        )
        .route(
            "/_security/user/_has_privileges",
            get(es_compat::security_has_privileges).post(es_compat::security_has_privileges),
        )
        .route(
            "/_security/user/:user/_has_privileges",
            get(es_compat::security_has_privileges).post(es_compat::security_has_privileges),
        )
        .route(
            "/_security/profile/_activate",
            post(es_compat::security_activate_user_profile),
        )
        .route(
            "/_security/profile/:uid",
            get(es_compat::security_get_user_profile),
        )
        .route(
            "/_security/api_key",
            post(es_compat::security_create_api_key)
                .get(es_compat::security_get_api_keys)
                .delete(es_compat::security_invalidate_api_key),
        )
        .route(
            "/_security/privilege",
            get(es_compat::security_get_all_privileges).put(es_compat::security_put_privileges),
        )
        .route(
            "/_security/privilege/:application",
            get(es_compat::security_get_application_privileges),
        )
        .route(
            "/_security/privilege/:application/:name",
            get(es_compat::security_get_privilege).delete(es_compat::security_delete_privilege),
        )
        // ── License ───────────────────────────────────────────────────────────
        .route(
            "/_license",
            get(es_compat::get_license).put(es_compat::put_license),
        )
        // ── Point-in-Time ─────────────────────────────────────────────────────
        .route("/:index/_pit", post(es_compat::open_pit))
        .route("/_pit", delete(es_compat::close_pit))
        // ── Internal cluster APIs ────────────────────────────────────────────
        .route(
            "/_internal/desired_balance",
            get(es_compat::get_desired_balance),
        )
        // ── EQL search ────────────────────────────────────────────────────────
        .route("/:index/_eql/search", post(es_compat::eql_search))
        // ── Global field_caps (across all indices) ────────────────────────────
        .route(
            "/_field_caps",
            get(es_compat::global_field_caps).post(es_compat::global_field_caps),
        )
        // ── Async search ──────────────────────────────────────────────────────
        .route(
            "/:index/_async_search",
            post(es_compat::async_search_submit),
        )
        .route(
            "/_async_search/:id",
            get(es_compat::async_search_get).delete(es_compat::async_search_delete),
        )
        // ── SQL ───────────────────────────────────────────────────────────────
        .route("/_sql", post(es_compat::sql_query))
        // ── Rank eval ─────────────────────────────────────────────────────────
        .route("/:index/_rank_eval", post(es_compat::rank_eval))
        // ── Recovery & Segments ───────────────────────────────────────────────
        .route("/:index/_recovery", get(es_compat::index_recovery))
        .route("/:index/_segments", get(es_compat::index_segments))
        // ── Freeze / Unfreeze ─────────────────────────────────────────────────
        .route("/:index/_freeze", post(es_compat::freeze_index))
        .route("/:index/_unfreeze", post(es_compat::unfreeze_index))
        // ── _ml anomaly detection APIs ────────────────────────────────────────
        .route(
            "/_ml/anomaly_detectors",
            get(es_compat::list_ml_anomaly_detectors),
        )
        .route(
            "/_ml/anomaly_detectors/:id",
            put(es_compat::put_ml_anomaly_detector)
                .get(es_compat::get_ml_anomaly_detector)
                .delete(es_compat::delete_ml_anomaly_detector),
        )
        .route(
            "/_ml/anomaly_detectors/:id/_score",
            post(es_compat::score_ml_anomaly_detector),
        )
        .route(
            "/_ml/anomaly_detectors/:id/results/records",
            get(es_compat::get_ml_records),
        )
        // ── _ml continuous datafeeds ──────────────────────────────────────────
        .route("/_ml/datafeeds", get(es_compat::list_ml_datafeeds))
        .route(
            "/_ml/datafeeds/:id",
            put(es_compat::put_ml_datafeed)
                .get(es_compat::get_ml_datafeed)
                .delete(es_compat::delete_ml_datafeed),
        )
        .route(
            "/_ml/datafeeds/:id/_start",
            post(es_compat::start_ml_datafeed),
        )
        .route(
            "/_ml/datafeeds/:id/_stop",
            post(es_compat::stop_ml_datafeed),
        )
        // ── _cat/ml APIs ──────────────────────────────────────────────────────
        .route(
            "/_cat/ml/anomaly_detectors",
            get(es_compat::cat_ml_anomaly_detectors),
        )
        .route("/_cat/ml/datafeeds", get(es_compat::cat_ml_datafeeds))
        .route(
            "/_cat/ml/trained_models",
            get(es_compat::cat_ml_trained_models),
        )
        // ── Monitoring ────────────────────────────────────────────────────────
        .route("/_monitoring/bulk", post(es_compat::monitoring_bulk))
        // ── Transform APIs ────────────────────────────────────────────────────
        .route(
            "/_transform/:id",
            put(es_compat::put_transform)
                .get(es_compat::get_transform)
                .delete(es_compat::delete_transform),
        )
        .route("/_transform/:id/_start", post(es_compat::start_transform))
        .route("/_transform/:id/_stop", post(es_compat::stop_transform))
        // ── Rollup APIs ───────────────────────────────────────────────────────────
        .route(
            "/_rollup/job/:id",
            put(es_compat::put_rollup_job)
                .get(es_compat::get_rollup_job)
                .delete(es_compat::delete_rollup_job),
        )
        .route("/_rollup/job/:id/_start", post(es_compat::start_rollup_job))
        .route("/_rollup/job/:id/_stop", post(es_compat::stop_rollup_job))
        .route("/_rollup/data/:index", get(es_compat::get_rollup_data))
        // ── Cross-Cluster Replication (CCR) ────────────────────────────────────────
        .route("/_ccr/stats", get(es_compat::ccr_stats))
        .route(
            "/_ccr/auto_follow/:name",
            put(es_compat::put_ccr_auto_follow)
                .get(es_compat::get_ccr_auto_follow)
                .delete(es_compat::delete_ccr_auto_follow),
        )
        .route("/:index/_ccr/follow", put(es_compat::ccr_follow))
        .route(
            "/:index/_ccr/pause_follow",
            post(es_compat::ccr_pause_follow),
        )
        .route(
            "/:index/_ccr/resume_follow",
            post(es_compat::ccr_resume_follow),
        )
        .route("/:index/_ccr/unfollow", post(es_compat::ccr_unfollow))
        .route("/:index/_ccr/info", get(es_compat::ccr_info))
        // ── Legacy index templates (v1) ────────────────────────────────────────────
        .route(
            "/_template/:name",
            put(es_compat::put_legacy_template)
                .get(es_compat::get_legacy_template)
                .delete(es_compat::delete_legacy_template),
        )
        // ── Simulate index template ────────────────────────────────────────────────
        .route(
            "/_index_template/_simulate_index/:name",
            post(es_compat::simulate_index_template),
        )
        // Body-only variant: previews an unsaved template definition (no
        // index name) — this is what Kibana's index template wizard calls
        // to validate a template before it's created.
        .route(
            "/_index_template/_simulate",
            post(es_compat::simulate_index_template_body),
        )
        // ── More _cat endpoints ────────────────────────────────────────────────────
        .route("/_cat/tasks", get(es_compat::cat_tasks))
        .route("/_cat/repositories", get(es_compat::cat_repositories))
        // ── Agent-Memory API ────────────────────────────────────────────────────────
        // Namespaced semantic memory for AI agents, backed by reserved
        // `.xerj-memory-{namespace}` indices (reuses the doc/dense_vector/BM25
        // machinery). See `memory_api.rs`.
        .route(
            "/_memory/:namespace",
            post(memory_api::store)
                .get(memory_api::list)
                .delete(memory_api::forget_namespace),
        )
        .route("/_memory/:namespace/_recall", post(memory_api::recall))
        .route("/_memory/:namespace/:id", delete(memory_api::forget_one))
        // ── Second-Brain Graph API ─────────────────────────────────────────────
        // Edges are ordinary documents in reserved `.xerj-memory-{brain}-edges`
        // indices; traversal is a bounded columnar expansion
        // (Index::graph_expand), NOT a graph query language. See graph_api.rs.
        .route("/_graph/:brain/link", post(graph_api::link))
        .route("/_graph/:brain/link/:edge_id", delete(graph_api::unlink))
        .route("/_graph/:brain/ego", get(graph_api::ego))
        .route("/_graph/:brain/overview", get(graph_api::overview))
        // Shared state
        .with_state(state.clone())
        // Middleware stack (applied outermost-last)
        //
        // Authorization sits INSIDE authentication — added first here, so it
        // runs second: `auth_middleware` decides whether the caller is anyone
        // at all, then `authz_middleware` decides what that caller may reach.
        // It is the enforcement point that makes a brain a real boundary
        // rather than a naming convention (issue #79); see `authz.rs` for the
        // full table of doors it closes. A superuser — `--insecure`, or the
        // configured admin key — short-circuits it on the first line.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authz::authz_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn_with_state(state, es_headers_middleware))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(trace_layer(access_log))
        .layer(cors)
        // Reject requests with a body larger than the configured limit (OOM guard).
        // Disable axum's built-in 2MB default so RequestBodyLimitLayer is the only
        // gate — bulk ingests legitimately exceed 2MB.
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(body_limit))
        // Outermost: transparently decompress gzip-encoded request bodies
        // (Content-Encoding: gzip) before the size limit above sees them, so
        // the limit bounds decompressed size, not wire size. Real ES clients
        // routinely compress writes by default (Beats, Logstash, official
        // client libraries) — without this every such request 400s/500s
        // trying to JSON-parse raw gzip bytes.
        .layer(RequestDecompressionLayer::new().gzip(true))
}

// ─────────────────────────────────────────────────────────────────────────────
// Middleware helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Middleware that sets ES-compatible product headers on every response.
///
/// Kibana and other Elastic clients verify the presence of `X-Elastic-Product`
/// before trusting the response. A real OpenSearch server never sends this
/// header at all, so a caller detected (or forced, see
/// [`es_compat::is_opensearch_caller`]) as OpenSearch must NOT receive it —
/// some OpenSearch-ecosystem clients treat an unexpected product header as
/// just another response header, but there's no reason to send an
/// Elastic-specific identity marker to a client we've already decided isn't
/// Elastic. A `Warning` header is included for Elastic callers only, for the
/// same reason.
///
/// A real OpenSearch server also sends `X-OpenSearch-Version: OpenSearch/<n>
/// (opensearch)` on every response, verified against a real OpenSearch 3.7.0
/// container — an OpenSearch-detected caller gets the equivalent header
/// here, built from the same version xerj already reports in `GET /`'s
/// `version` block (`es_compat::resolve_compat_version`), so the two stay in
/// sync by construction. Found missing while chasing an OpenSearch
/// Dashboards Timeline (Timelion) panel that rendered empty against xerj but
/// correctly against real OpenSearch for the byte-identical aggregation
/// response body — this header was the one structural difference between
/// the two.
async fn es_headers_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let is_opensearch = es_compat::is_opensearch_caller(&state, req.headers());
    let opensearch_version =
        is_opensearch.then(|| es_compat::resolve_compat_version(&state, req.headers()).number);
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    if is_opensearch {
        headers.remove("x-elastic-product");
        headers.remove("warning");
        if let Some(number) = opensearch_version {
            if let Ok(v) = HeaderValue::from_str(&format!("OpenSearch/{number} (opensearch)")) {
                headers.insert("x-opensearch-version", v);
            }
        }
    } else {
        headers.insert(
            "x-elastic-product",
            HeaderValue::from_static("Elasticsearch"),
        );
        // RFC 7234 warning header — signals no specific deprecation for now.
        if let Ok(v) = HeaderValue::from_str("299 Elasticsearch-8.13.0 \"\"") {
            headers.insert("warning", v);
        }
    }
    resp
}

/// Build the CORS layer from config. **Default restrictive** (item 5).
///
/// The pre-RC4 layer hard-coded `allow_origin(Any) / allow_methods(Any) /
/// allow_headers(Any)` with no knob, so `Access-Control-Allow-Origin: *` was
/// returned to every browser — any page on the web could script authenticated
/// cross-origin requests against a reachable node. This reads
/// [`CorsConfig`]:
///
/// - `allow_any_origin = true` → the legacy wide-open policy (opt-in, dev only).
/// - non-empty `allowed_origins` → reflect only those exact origins, with a
///   fixed method set; other origins get no `Access-Control-Allow-Origin`.
/// - default (empty list, `allow_any_origin = false`) → emit **no** CORS
///   headers at all, so browsers permit same-origin requests only. Non-browser
///   clients (curl, SDKs, Kibana) are unaffected because they ignore CORS.
fn cors_layer(cfg: &CorsConfig) -> CorsLayer {
    if cfg.allow_any_origin {
        // Opt-in dev escape hatch — restores the legacy wide-open behavior.
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
    }
    if cfg.allowed_origins.is_empty() {
        // Restrictive default: no `allow_origin` configured → tower-http emits
        // no `Access-Control-Allow-Origin`, so cross-origin browser reads fail.
        return CorsLayer::new();
    }
    // Reflect only the explicitly allow-listed origins. Entries that don't
    // parse as a header value are dropped (logged once at build time) rather
    // than silently widening the policy.
    let origins: Vec<HeaderValue> = cfg
        .allowed_origins
        .iter()
        .filter_map(|o| match o.parse::<HeaderValue>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(origin = %o, "ignoring un-parseable cors.allowed_origins entry");
                None
            }
        })
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
            Method::PATCH,
        ])
        .allow_headers(Any)
}

/// Build the request-tracing layer (RC4-W4 item 6).
///
/// When `access_log` is true, the span + request/response events are emitted at
/// INFO, so an operator gets one access line per request (method, path, status,
/// latency) under the default `info` filter. When false they stay at DEBUG —
/// silent by default, preserving today's quiet output — without forcing the
/// global filter down to debug. Both arms produce the same concrete type (the
/// level is a runtime field on the `Default*` responders), so this returns one
/// layer regardless of the flag.
fn trace_layer(access_log: bool) -> TraceLayer<SharedClassifier<ServerErrorsAsFailures>> {
    let level = if access_log {
        Level::INFO
    } else {
        Level::DEBUG
    };
    TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(level))
        .on_request(DefaultOnRequest::new().level(level))
        .on_response(DefaultOnResponse::new().level(level))
}

/// Middleware that injects a UUID request ID into extensions and response headers.
async fn request_id_middleware(mut req: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    req.extensions_mut().insert(RequestId(request_id.clone()));
    let mut resp = next.run(req).await;
    if let Ok(v) = request_id.parse() {
        resp.headers_mut().insert("x-request-id", v);
    }
    resp
}

/// Type-safe wrapper stored in request extensions.
///
/// Handlers can extract this via `Extension<RequestId>` if they need
/// the request ID without regenerating it.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

// ─────────────────────────────────────────────────────────────────────────────
// Tests — CORS policy (item 5)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt; // oneshot

    fn app(cfg: CorsConfig) -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(cors_layer(&cfg))
    }

    /// The `Access-Control-Allow-Origin` value the layer returns for a browser
    /// request carrying `Origin: <origin>` (None = header absent).
    async fn acao(cfg: CorsConfig, origin: &str) -> Option<String> {
        let req = Request::builder()
            .uri("/")
            .header("origin", origin)
            .body(Body::empty())
            .unwrap();
        let resp = app(cfg).oneshot(req).await.unwrap();
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap().to_string())
    }

    /// DEFAULT RESTRICTIVE: an out-of-the-box node emits no
    /// `Access-Control-Allow-Origin`, so a browser blocks the cross-origin read.
    #[tokio::test]
    async fn cors_default_restrictive_emits_no_acao() {
        assert_eq!(
            acao(CorsConfig::default(), "https://evil.example").await,
            None,
            "default config must not emit Access-Control-Allow-Origin"
        );
    }

    /// An explicit allow-list reflects ONLY the listed origins; anything else
    /// gets no ACAO header (so the browser blocks it).
    #[tokio::test]
    async fn cors_allowlist_reflects_only_listed_origins() {
        let cfg = CorsConfig {
            allowed_origins: vec!["https://good.example".into()],
            allow_any_origin: false,
        };
        assert_eq!(
            acao(cfg.clone(), "https://good.example").await.as_deref(),
            Some("https://good.example"),
            "listed origin is reflected"
        );
        assert_eq!(
            acao(cfg, "https://evil.example").await,
            None,
            "un-listed origin must get no ACAO"
        );
    }

    /// The `allow_any_origin` dev escape hatch restores the wildcard policy.
    #[tokio::test]
    async fn cors_any_origin_optin_is_wildcard() {
        let cfg = CorsConfig {
            allowed_origins: vec![],
            allow_any_origin: true,
        };
        assert_eq!(
            acao(cfg, "https://anything.example").await.as_deref(),
            Some("*"),
            "allow_any_origin must restore Access-Control-Allow-Origin: *"
        );
    }

    fn test_state(distribution: &str) -> AppState {
        test_state_with(distribution, |_| {})
    }

    fn test_state_with(
        distribution: &str,
        configure: impl FnOnce(&mut xerj_common::config::Config),
    ) -> AppState {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.to_string_lossy().into_owned();
        config.storage.wal_sync = xerj_common::config::WalSync::Async;
        config.compat.distribution = distribution.to_string();
        configure(&mut config);
        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");
        AppState::new(config, engine, metrics)
    }

    async fn headers_for(distribution: &str) -> axum::http::HeaderMap {
        let app =
            Router::new()
                .route("/", get(|| async { "ok" }))
                .layer(middleware::from_fn_with_state(
                    test_state(distribution),
                    es_headers_middleware,
                ));
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        app.oneshot(req).await.unwrap().headers().clone()
    }

    /// Regression: a real OpenSearch server sends `X-OpenSearch-Version` on
    /// every response — verified against a real OpenSearch 3.7.0 container
    /// while chasing an OpenSearch Dashboards Timeline panel that rendered
    /// empty against xerj but correctly against real OpenSearch for a
    /// byte-identical aggregation response body. This header was the one
    /// structural difference between the two.
    #[tokio::test]
    async fn opensearch_caller_gets_x_opensearch_version_header() {
        let headers = headers_for("opensearch").await;
        assert_eq!(
            headers
                .get("x-opensearch-version")
                .and_then(|v| v.to_str().ok()),
            Some("OpenSearch/2.11.0 (opensearch)"),
            "an OpenSearch-detected caller must get the real-OpenSearch-shaped version header"
        );
        assert!(
            !headers.contains_key("x-elastic-product"),
            "must not also send the Elastic-specific product header"
        );
    }

    /// An Elastic-shaped caller gets neither the OpenSearch header (never
    /// sent by real OpenSearch) nor is affected by this change — it keeps
    /// the pre-existing `x-elastic-product` marker.
    #[tokio::test]
    async fn elastic_caller_does_not_get_opensearch_version_header() {
        let headers = headers_for("elasticsearch").await;
        assert!(
            !headers.contains_key("x-opensearch-version"),
            "an Elastic-shaped caller must never see the OpenSearch identity header"
        );
        assert_eq!(
            headers
                .get("x-elastic-product")
                .and_then(|v| v.to_str().ok()),
            Some("Elasticsearch")
        );
    }

    #[tokio::test]
    async fn embedding_identity_is_sanitized_machine_readable_native_data() {
        for app in [
            build_native_router(test_state("elasticsearch")),
            build_es_compat_router(test_state("elasticsearch")),
        ] {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/v1/embedding/identity")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let data = value["data"].as_object().unwrap();
            assert_eq!(data["backend"], "lexical");
            assert_eq!(data["dimensions"], 384);
            assert_eq!(data["resumable"], true);
            assert_eq!(data["identity_sha256"].as_str().unwrap().len(), 64);
            let encoded = String::from_utf8(bytes.to_vec()).unwrap();
            for secret_field in ["api_key", "endpoint", "model_path", "tokenizer_path"] {
                assert!(!encoded.contains(secret_field), "{secret_field} leaked");
            }
        }
    }

    #[cfg(feature = "onnx-experimental")]
    #[tokio::test]
    async fn embedding_identity_asset_failures_do_not_leak_local_paths() {
        const MODEL: &str = "/private/customer-a/models/missing-model.onnx";
        const TOKENIZER: &str = "/private/customer-a/models/missing-tokenizer.json";
        for app in [
            build_native_router(test_state_with("elasticsearch", |config| {
                config.embedding.mode = "onnx-experimental".into();
                config.embedding.onnx_model_path = MODEL.into();
                config.embedding.onnx_tokenizer_path = TOKENIZER.into();
            })),
            build_es_compat_router(test_state_with("elasticsearch", |config| {
                config.embedding.mode = "onnx-experimental".into();
                config.embedding.onnx_model_path = MODEL.into();
                config.embedding.onnx_tokenizer_path = TOKENIZER.into();
            })),
        ] {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/v1/embedding/identity")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(!response.status().is_success());
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let encoded = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(encoded.contains("verify the configured embedding backend"));
            assert!(encoded.contains("restart the server after fixing the assets"));
            for private in [
                MODEL,
                TOKENIZER,
                "missing-model.onnx",
                "missing-tokenizer.json",
                "No such file",
                "os error",
            ] {
                assert!(!encoded.contains(private), "{private} leaked: {encoded}");
            }
        }
    }
}
