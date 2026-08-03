//! Pluggable data sources.
//!
//! Phase-3 scope: the **read** half — list connections, list indices on
//! a connection, list fields on an index — backed by a single
//! auto-provisioned `built-in` connection that talks straight to the
//! in-process Engine.  This is enough for the SPA to drop its
//! hand-rolled `data-sources.js` overlay.
//!
//! **External-connection disclosure.** Only the `built-in` connection is
//! backed by a live adapter; its index and field listings are read
//! straight from the running Engine's real schema, never a canned map.
//! No external adapter subsystem exists yet — the adapter trait,
//! encrypted credentials, write paths (POST/PATCH/DELETE) and the
//! HTTP-shaped adapters (xerj-remote, elasticsearch, opensearch,
//! prometheus, postgres) all land in a follow-up commit.  Until then,
//! listing indices or fields for any non-`built-in` connection id
//! returns `501 Not Implemented` rather than a fabricated answer, and a
//! connection row that is not `built-in` is reported with status
//! `"unknown"` (no live probe is performed) so the UI never shows a
//! green light it cannot stand behind.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::auth::sessions::AuthSession;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::ok;
use crate::state::ConsoleState;
use crate::time::now_iso;

/// Indices the data-sources surface must pretend do not exist: the Console's
/// own `.xerj_*` system indices, and the reserved `.xerj-memory-*` namespace
/// holding per-tenant brains / agent memories. The latter is enforced on the
/// data plane by `xerj-api`'s authz middleware, but the Console router is
/// merged onto the engine routers *after* their layers, so that middleware
/// never runs here and the in-process engine calls below carry no visibility
/// scope (absent guard = engine-internal work, allow). This proxy is
/// therefore its own boundary and must refuse the namespace itself — with
/// the same NotFound as a system index, so existence doesn't leak.
fn is_hidden_index(name: &str) -> bool {
    indices::is_system_index(name) || xerj_common::types::is_reserved_index(name)
}

/// Connection record — one row in `.xerj_connections`. Phase-3 only
/// surfaces the built-in adapter; adding new connections lands once the
/// AEAD-at-rest path for `auth.secret` is implemented.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_true")]
    pub default: bool,
    #[serde(default = "default_true")]
    pub managed: bool,
    pub created_at: String,
    pub created_by: String,
    pub etag: String,
}

fn default_true() -> bool {
    true
}

const BUILTIN_ID: &str = "built-in";

// ─────────────────────────────────────────────────────────────────────────────
// LIST
// ─────────────────────────────────────────────────────────────────────────────

pub async fn list_connections(
    State(state): State<ConsoleState>,
    _sess: AuthSession,
) -> ConsoleResult<Response> {
    ensure_builtin(&state).await?;
    let idx = state.engine.get_index(indices::CONNECTIONS)?;
    let body = json!({
        "query": { "match_all": {} },
        "size": 100
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;

    let mut conns: Vec<Connection> = Vec::with_capacity(r.hits.len());
    for h in r.hits {
        if let Ok(c) = serde_json::from_value::<Connection>(h.source) {
            conns.push(c);
        }
    }

    // Decorate with live status — for now, only `built-in` is live, and
    // it's always green if the engine itself is responding (we're inside
    // it).  External adapters add real probing in a later commit.
    let mut out: Vec<Value> = Vec::with_capacity(conns.len());
    for c in conns {
        let status = if c.id == BUILTIN_ID {
            "green"
        } else {
            "unknown"
        };
        let mut v = serde_json::to_value(&c)?;
        v["status"] = Value::String(status.into());
        v["last_checked_at"] = Value::String(now_iso());
        out.push(v);
    }

    Ok(ok(json!({ "connections": out, "total": out.len() }), None))
}

// ─────────────────────────────────────────────────────────────────────────────
// INDICES
// ─────────────────────────────────────────────────────────────────────────────

pub async fn list_indices(
    State(state): State<ConsoleState>,
    _sess: AuthSession,
    Path(conn_id): Path<String>,
) -> ConsoleResult<Response> {
    ensure_builtin(&state).await?;

    if conn_id != BUILTIN_ID {
        // External adapters land in a later commit; everything that
        // isn't built-in is a stub for now.
        return Err(ConsoleApiError::NotImplemented(format!(
            "connection '{conn_id}' adapter not yet implemented"
        )));
    }

    // Walk the engine's indices via its public listing surface.  Skip
    // `.xerj_*` system indices and reserved `.xerj-memory-*` brains (see
    // `is_hidden_index`) — neither is a user data source.
    let mut items: Vec<Value> = Vec::new();
    for name in state.engine.index_name_list() {
        if is_hidden_index(&name) {
            continue;
        }
        let idx = match state.engine.get_index(&name) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let stats = idx.stats().await;
        items.push(json!({
            "name": name,
            "docs": stats.doc_count,
            "segments": stats.segment_count,
            "fields": stats.field_count,
            "shards": 1,
            "replicas": 0,
        }));
    }
    items.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    Ok(ok(json!({ "indices": items, "total": items.len() }), None))
}

// ─────────────────────────────────────────────────────────────────────────────
// FIELDS
// ─────────────────────────────────────────────────────────────────────────────

pub async fn list_fields(
    State(state): State<ConsoleState>,
    _sess: AuthSession,
    Path((conn_id, index)): Path<(String, String)>,
) -> ConsoleResult<Response> {
    if conn_id != BUILTIN_ID {
        return Err(ConsoleApiError::NotImplemented(format!(
            "connection '{conn_id}' adapter not yet implemented"
        )));
    }
    if is_hidden_index(&index) {
        return Err(ConsoleApiError::NotFound(format!("index {index}")));
    }
    let idx = state
        .engine
        .get_index(&index)
        .map_err(|_| ConsoleApiError::NotFound(format!("index {index}")))?;
    let schema = idx.schema().await;
    let fields: Vec<Value> = schema
        .fields
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "type": f.field_type.to_string(),
                "indexed": f.options.indexed,
                "doc_values": f.options.doc_values,
            })
        })
        .collect();
    Ok(ok(json!({ "fields": fields, "total": fields.len() }), None))
}

// ─────────────────────────────────────────────────────────────────────────────
// SEARCH
// ─────────────────────────────────────────────────────────────────────────────

/// Run an ES-compat `_search` against a single index, authenticated by the
/// caller's Console session instead of a data-plane API key.
///
/// **Why this exists.** The SPA's declarative dashboard panels
/// (`xerj-ux/src/data/panel-query.js`) and the built-in dashboard adapters
/// (`xerj-ux/src/data/backends/xerj.js`) used to `fetch()` `/{index}/_search`
/// directly, same-origin, with no `Authorization` header at all. That is
/// silently correct only when the engine is started with `--insecure`; on
/// any auth-enabled deployment — the default, recommended posture — every
/// such call gets a 401 the caller doesn't even look at (`backends/xerj.js`
/// treats any failure as "no live adapter, fall back to mock"), so the
/// entire "real dashboards" feature quietly regresses to demo data with no
/// visible error. This endpoint reuses the session-cookie gate every other
/// `data-sources` route already has (see `list_indices` / `list_fields`
/// above) and talks to the engine in-process — no data-plane credential is
/// needed at all, the same way `list_indices`/`list_fields` already read
/// the schema/stats in-process.
///
/// **Scope.** Single, exact index names only, matching `list_fields`'s
/// existing scope — no wildcard/`_all` resolution or multi-index merge
/// here (that logic is non-trivial and already lives, correctly, in
/// `xerj-api::es_compat`; duplicating a second copy risks exactly the kind
/// of subtle merge bug that logic has already been hardened against).
/// Callers that need a pattern get a clear `501`, not a silent wrong
/// answer — consistent with how this file already treats non-`built-in`
/// connections.
pub async fn search(
    State(state): State<ConsoleState>,
    _sess: AuthSession,
    Path((conn_id, index)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ConsoleResult<Response> {
    if conn_id != BUILTIN_ID {
        return Err(ConsoleApiError::NotImplemented(format!(
            "connection '{conn_id}' adapter not yet implemented"
        )));
    }
    if index.contains('*') || index == "_all" || index.split(',').count() > 1 {
        return Err(ConsoleApiError::NotImplemented(
            "panel search proxy supports a single exact index name only; \
             wildcard/_all/multi-index queries are not yet implemented"
                .into(),
        ));
    }
    if is_hidden_index(&index) {
        return Err(ConsoleApiError::NotFound(format!("index {index}")));
    }
    let idx = state
        .engine
        .get_index(&index)
        .map_err(|_| ConsoleApiError::NotFound(format!("index {index}")))?;

    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::BadRequest(e.to_string()))?;
    let result = idx
        .search(&req)
        .await
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;

    let hits: Vec<Value> = result
        .hits
        .iter()
        .map(|h| {
            json!({
                "_index": index,
                "_id": h.id,
                "_score": h.score,
                "_source": h.source,
            })
        })
        .collect();
    let wire = json!({
        "took": result.took_ms,
        "timed_out": result.timed_out,
        "hits": {
            "total": { "value": result.total.value, "relation": result.total.relation },
            "max_score": result.max_score,
            "hits": hits,
        },
        "aggregations": result.aggs,
    });
    Ok(Json(wire).into_response())
}

// ─────────────────────────────────────────────────────────────────────────────
// Built-in auto-provisioning
// ─────────────────────────────────────────────────────────────────────────────

async fn ensure_builtin(state: &ConsoleState) -> ConsoleResult<()> {
    let idx = state.engine.get_index(indices::CONNECTIONS)?;
    let body = json!({
        "query": { "ids": { "values": [BUILTIN_ID] } },
        "size": 1
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    if !r.hits.is_empty() {
        return Ok(());
    }
    let conn = Connection {
        id: BUILTIN_ID.into(),
        name: "Local Xerj".into(),
        kind: "xerj-local".into(),
        url: None,
        default: true,
        managed: true,
        created_at: now_iso(),
        created_by: "system".into(),
        etag: "1".into(),
    };
    let _ = idx.delete_document(BUILTIN_ID).await;
    idx.create_document(BUILTIN_ID.into(), serde_json::to_value(&conn)?)
        .await?;
    Ok(())
}
