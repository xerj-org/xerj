//! Dashboards CRUD.
//!
//! Replaces `localStorage["xerj.dashboards"]` and
//! `localStorage["xerj.layout.<id>"]` with engine-backed durable state.
//! Every shared dashboard write expects an `If-Match: W/"<version>"`
//! etag for optimistic concurrency; private dashboards skip that check
//! since only the owner ever writes them.
//!
//! Real-time push (SSE `/_stream`) is NOT implemented in this release:
//! there is no `/_stream` endpoint on dashboards or views, and none is
//! registered in the router.  Concurrent edits are surfaced only on the
//! next read — an SE who wants to see "another user just renamed this
//! dashboard" must re-fetch (poll) rather than receive a live event.
//! The If-Match etag on writes still makes a stale update fail loudly
//! with 409 instead of silently clobbering, so correctness never
//! depends on live push; it only trades a lower-latency notification.
//! CRUD is complete and correct; live push is deferred (tracked in the
//! crate-level "Coming after RC" list in `lib.rs`).

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::sessions::AuthSession;
use crate::auth::store::UserStatus;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::{created, no_content, ok};
use crate::state::ConsoleState;
use crate::time::now_iso;

/// A durable dashboard.
///
/// # Panel schema (convention inside `panels[]`)
///
/// The engine stores `panels` **opaquely** — it is a `Vec<Value>` here and an
/// untyped array in the mapping (deliberately *not* typed in
/// [`crate::indices`]).  The frontend panel-builder and the seeding module
/// ([`crate::seed`]) agree on this shape, and it round-trips through
/// create/replace/patch byte-for-byte:
///
/// ```jsonc
/// {
///   "id": "queries",                 // stable & unique within the dashboard
///   "type": "metric|line|topn|heatmap|table|events|markdown|ribbon3d|…",
///   "title": "LLM QUERIES",          // supersedes the old `eyebrow`
///   "layout": { "x": 0, "y": 0, "w": 4, "h": 2 },
///        // FREE-FORM grid: x/w in 12-col units, y/h in row units. Subsumes
///        // the old bare `cols` (== w) and enables drag / move / resize /
///        // height.
///   "query": {                       // null for static panels (markdown)
///     "index": "logs-*",
///     "time_field": "@timestamp",
///     "dsl":  { /* ES query DSL — reuses the ES-compat search port */ },
///     "aggs": { /* ES aggs */ }
///   },
///   "viz":  { "unit": "queries", "value_field": "…", "spark": true, … },
///        // type-specific display config
///   "drilldown": { "to": "<dashId>", "filter_field": "intent" } | null,
///   "builtin":   "ai-overview/queries" | null
///        // provenance key for seeded panels still resolved through the
///        // shipped mock renderer; null for user-authored data-driven panels.
/// }
/// ```
///
/// Per-panel geometry, query, and viz all live here; the dashboard-level time
/// / filter context lives in the top-level `time_default` + `filters_default`.
/// There is intentionally **no per-panel PATCH**: `panels` replaces wholesale
/// (dashboards are small — the frontend sends the full array on save).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dashboard {
    pub id: String,
    pub owner: String,
    #[serde(default = "default_org")]
    pub org_id: String,
    /// `private` (only owner sees / writes), `shared` (every user in
    /// the org reads, only `editor` and up writes), `default` (engine-
    /// provisioned built-in dashboards — editable by admin/owner, never
    /// deletable; see `managed`).
    pub visibility: String,
    /// `true` for engine-seeded built-in dashboards ([`crate::seed`]). A
    /// managed doc is editable by admin/owner (so layout/title/panel edits
    /// persist) but cannot be deleted, and re-seed leaves it alone once it has
    /// been edited (`version > 1`). User-authored dashboards are always
    /// `managed: false` — clients cannot mint a managed doc through `create`.
    #[serde(default)]
    pub managed: bool,
    pub name: String,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub cloned_from: Option<String>,
    #[serde(default)]
    pub panels: Vec<Value>,
    #[serde(default)]
    pub filters_default: Value,
    #[serde(default)]
    pub time_default: Option<String>,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub deleted_at: Option<String>,
}

fn default_org() -> String {
    "default".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub name: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub cloned_from: Option<String>,
    #[serde(default)]
    pub panels: Vec<Value>,
    #[serde(default)]
    pub filters_default: Value,
    #[serde(default)]
    pub time_default: Option<String>,
}

fn default_visibility() -> String {
    "private".to_string()
}

#[derive(Debug, Deserialize, Default)]
pub struct PatchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub panels: Option<Vec<Value>>,
    #[serde(default)]
    pub filters_default: Option<Value>,
    #[serde(default)]
    pub time_default: Option<String>,
    /// Accepted for a full-object round-trip, but only applied when the caller
    /// is admin/owner (see `apply_managed`). `owner` / `org_id` / `created_at`
    /// are intentionally absent — they are create-only and never patchable.
    #[serde(default)]
    pub managed: Option<bool>,
}

// ─────────────────────────────────────────────────────────────────────────────
// LIST
// ─────────────────────────────────────────────────────────────────────────────

pub async fn list(State(state): State<ConsoleState>, sess: AuthSession) -> ConsoleResult<Response> {
    let idx = state.engine.get_index(indices::DASHBOARDS)?;
    // Pull everything; filter by owner / visibility in code. The
    // `.xerj_dashboards` index is small (≤ low thousands) so the
    // simpler approach beats fighting bool-query semantics in tests.
    let body = json!({
        "query": { "match_all": {} },
        "size": 1000
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;

    let me = sess.user.id.as_str();
    let mut items: Vec<Value> = Vec::with_capacity(r.hits.len());
    for h in r.hits {
        let d: Dashboard = match serde_json::from_value(h.source) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if d.deleted_at.is_some() {
            continue;
        }
        let visible = d.owner == me || d.visibility == "shared" || d.visibility == "default";
        if visible {
            items.push(serde_json::to_value(&d)?);
        }
    }
    Ok(ok(
        json!({ "dashboards": items, "total": items.len() }),
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// GET ONE
// ─────────────────────────────────────────────────────────────────────────────

pub async fn get_one(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Path(id): Path<String>,
) -> ConsoleResult<Response> {
    let dash = read_required(&state, &id).await?;
    enforce_read(&sess, &dash)?;
    let etag = format!("{}", dash.version);
    Ok(ok(dash, Some(&etag)))
}

// ─────────────────────────────────────────────────────────────────────────────
// CREATE
// ─────────────────────────────────────────────────────────────────────────────

pub async fn create(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Json(body): Json<CreateBody>,
) -> ConsoleResult<Response> {
    enforce_can_set_visibility(&sess, &body.visibility, None)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let dash = Dashboard {
        id: id.clone(),
        owner: sess.user.id.clone(),
        org_id: default_org(),
        visibility: body.visibility,
        // Clients never mint managed docs — only the seeder (and the
        // admin/owner-only `_bulk` path) can set `managed: true`.
        managed: false,
        name: body.name,
        section: body.section,
        group: body.group,
        cloned_from: body.cloned_from,
        panels: body.panels,
        filters_default: body.filters_default,
        time_default: body.time_default,
        version: 1,
        created_at: now.clone(),
        updated_at: now,
        deleted_at: None,
    };
    write_doc(&state, &dash).await?;
    let etag = format!("{}", dash.version);
    let location = format!("/_xerj-console/api/v1/dashboards/{id}");
    Ok(created(dash, &location, Some(&etag)))
}

// ─────────────────────────────────────────────────────────────────────────────
// REPLACE (PUT)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ReplaceBody {
    pub name: String,
    pub visibility: String,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub panels: Vec<Value>,
    #[serde(default)]
    pub filters_default: Value,
    #[serde(default)]
    pub time_default: Option<String>,
    /// See [`PatchBody::managed`] — applied only for admin/owner; create-only
    /// fields (`owner`/`org_id`/`created_at`) are never accepted here.
    #[serde(default)]
    pub managed: Option<bool>,
}

/// Body for `POST /dashboards/_bulk` — whole-set save + uniform seeding path.
#[derive(Debug, Deserialize, Default)]
pub struct BulkBody {
    /// Full dashboard objects (each MUST carry an `id`); upserted in place.
    #[serde(default)]
    pub upserts: Vec<Value>,
    /// Ids to soft-delete.
    #[serde(default)]
    pub deletes: Vec<String>,
}

pub async fn replace(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ReplaceBody>,
) -> ConsoleResult<Response> {
    let mut dash = read_required(&state, &id).await?;
    enforce_write(&sess, &dash)?;
    enforce_etag(&headers, dash.version)?;
    enforce_can_set_visibility(&sess, &body.visibility, Some(&dash.visibility))?;

    dash.name = body.name;
    dash.visibility = body.visibility;
    dash.section = body.section;
    dash.group = body.group;
    dash.panels = body.panels;
    dash.filters_default = body.filters_default;
    dash.time_default = body.time_default;
    apply_managed(&sess, &mut dash, body.managed);
    dash.version += 1;
    dash.updated_at = now_iso();

    write_doc(&state, &dash).await?;
    let etag = format!("{}", dash.version);
    Ok(ok(dash, Some(&etag)))
}

// ─────────────────────────────────────────────────────────────────────────────
// PATCH
// ─────────────────────────────────────────────────────────────────────────────

pub async fn patch(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PatchBody>,
) -> ConsoleResult<Response> {
    let mut dash = read_required(&state, &id).await?;
    enforce_write(&sess, &dash)?;
    enforce_etag(&headers, dash.version)?;

    if let Some(name) = body.name {
        dash.name = name;
    }
    if let Some(visibility) = body.visibility {
        enforce_can_set_visibility(&sess, &visibility, Some(&dash.visibility))?;
        dash.visibility = visibility;
    }
    if let Some(section) = body.section {
        dash.section = Some(section);
    }
    if let Some(group) = body.group {
        dash.group = Some(group);
    }
    if let Some(panels) = body.panels {
        dash.panels = panels;
    }
    if let Some(filters) = body.filters_default {
        dash.filters_default = filters;
    }
    if let Some(td) = body.time_default {
        dash.time_default = Some(td);
    }
    apply_managed(&sess, &mut dash, body.managed);

    dash.version += 1;
    dash.updated_at = now_iso();

    write_doc(&state, &dash).await?;
    let etag = format!("{}", dash.version);
    Ok(ok(dash, Some(&etag)))
}

// ─────────────────────────────────────────────────────────────────────────────
// DELETE (soft)
// ─────────────────────────────────────────────────────────────────────────────

pub async fn delete(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Path(id): Path<String>,
) -> ConsoleResult<Response> {
    let mut dash = read_required(&state, &id).await?;
    // Seeded / managed defaults are editable but not deletable, so a re-seed
    // always has a canonical row to keep in sync and an operator can't nuke a
    // shipped dashboard by accident.
    if dash.managed || dash.visibility == "default" {
        return Err(ConsoleApiError::Forbidden(
            "managed default dashboards cannot be deleted".into(),
        ));
    }
    enforce_write(&sess, &dash)?;
    dash.deleted_at = Some(now_iso());
    dash.version += 1;
    write_doc(&state, &dash).await?;
    Ok(no_content())
}

// ─────────────────────────────────────────────────────────────────────────────
// BULK (whole-set save + uniform seeding path)
// ─────────────────────────────────────────────────────────────────────────────

/// `POST /dashboards/_bulk` — upsert a set of full dashboards and/or
/// soft-delete a set of ids in one call.  This is a **privileged** operation
/// (it can set `owner` / `managed` / `visibility` freely), so it is limited to
/// admin/owner roles.  The frontend uses it for a whole-set save; seeding uses
/// the same shape server-side.
pub async fn bulk(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Json(body): Json<BulkBody>,
) -> ConsoleResult<Response> {
    match sess.user.role.as_str() {
        "admin" | "owner" => require_active(&sess)?,
        _ => {
            return Err(ConsoleApiError::Forbidden(
                "bulk dashboard write requires admin or owner".into(),
            ))
        }
    }

    let now = now_iso();
    let mut upserted: Vec<String> = Vec::with_capacity(body.upserts.len());
    for raw in body.upserts {
        let mut dash: Dashboard = serde_json::from_value(raw).map_err(|e| {
            ConsoleApiError::BadRequest(format!("invalid dashboard in upsert: {e}"))
        })?;
        if dash.id.trim().is_empty() {
            return Err(ConsoleApiError::BadRequest(
                "upsert entry missing id".into(),
            ));
        }
        validate_visibility_value(&dash.visibility)?;
        if dash.org_id.trim().is_empty() {
            dash.org_id = default_org();
        }
        if dash.created_at.trim().is_empty() {
            dash.created_at = now.clone();
        }
        dash.version = dash.version.max(1);
        dash.updated_at = now.clone();
        write_doc(&state, &dash).await?;
        upserted.push(dash.id);
    }

    let mut deleted: Vec<String> = Vec::with_capacity(body.deletes.len());
    for id in body.deletes {
        // Ignore ids that are already gone (idempotent).
        if let Ok(mut dash) = read_required(&state, &id).await {
            dash.deleted_at = Some(now.clone());
            dash.version += 1;
            write_doc(&state, &dash).await?;
            deleted.push(id);
        }
    }

    Ok(ok(
        json!({ "upserted": upserted, "deleted": deleted }),
        None,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// helpers
// ─────────────────────────────────────────────────────────────────────────────

async fn read_required(state: &ConsoleState, id: &str) -> ConsoleResult<Dashboard> {
    let idx = state.engine.get_index(indices::DASHBOARDS)?;
    let body = json!({
        "query": { "ids": { "values": [id] } },
        "size": 1
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    let hit = r
        .hits
        .into_iter()
        .next()
        .ok_or_else(|| ConsoleApiError::NotFound(format!("dashboard {id}")))?;
    let dash: Dashboard = serde_json::from_value(hit.source)?;
    if dash.deleted_at.is_some() {
        return Err(ConsoleApiError::NotFound(format!("dashboard {id}")));
    }
    Ok(dash)
}

async fn write_doc(state: &ConsoleState, dash: &Dashboard) -> ConsoleResult<()> {
    let idx = state.engine.get_index(indices::DASHBOARDS)?;
    // Single in-place upsert (one WAL append) instead of delete-then-create,
    // so a crash between the two operations can never leave the id missing
    // (durability, design C.2). `index_document` overwrites an existing id.
    idx.index_document(Some(dash.id.clone()), serde_json::to_value(dash)?)
        .await?;
    Ok(())
}

fn enforce_read(sess: &AuthSession, dash: &Dashboard) -> ConsoleResult<()> {
    if dash.owner == sess.user.id {
        return Ok(());
    }
    match dash.visibility.as_str() {
        "shared" | "default" => Ok(()),
        _ => Err(ConsoleApiError::NotFound("dashboard".into())),
    }
}

fn enforce_write(sess: &AuthSession, dash: &Dashboard) -> ConsoleResult<()> {
    // Managed / default-visibility dashboards used to be flatly read-only.
    // They are now *editable* by admin/owner so the seeded defaults are true
    // editable data — layout, title, and panel edits persist. Lower roles
    // still can't touch them, and DELETE is blocked for everyone (see
    // `delete`). Re-seed leaves any edited default alone (version > 1).
    if dash.managed || dash.visibility == "default" {
        return match sess.user.role.as_str() {
            "admin" | "owner" => require_active(sess),
            _ => Err(ConsoleApiError::Forbidden(
                "managed dashboards are editable only by admin or owner".into(),
            )),
        };
    }
    if dash.owner == sess.user.id {
        return require_active(sess);
    }
    if dash.visibility == "shared" {
        match sess.user.role.as_str() {
            "editor" | "admin" | "owner" => return require_active(sess),
            _ => {}
        }
    }
    Err(ConsoleApiError::Forbidden("not your dashboard".into()))
}

/// Reject an unknown visibility string (used on every write path).
fn validate_visibility_value(visibility: &str) -> ConsoleResult<()> {
    match visibility {
        "private" | "shared" | "default" => Ok(()),
        other => Err(ConsoleApiError::BadRequest(format!(
            "unknown visibility '{other}'"
        ))),
    }
}

/// Validate the requested visibility and guard *escalation* to `default` by a
/// non-owner. The escalation guard fires only when the new visibility is
/// `default` **and differs** from the doc's current visibility — so an admin
/// editing an already-`default` seeded dashboard isn't blocked from keeping it
/// default; only minting a fresh unkillable default is owner-only. Pass
/// `current = None` on create.
fn enforce_can_set_visibility(
    sess: &AuthSession,
    new_visibility: &str,
    current: Option<&str>,
) -> ConsoleResult<()> {
    validate_visibility_value(new_visibility)?;
    if new_visibility == "default" && current != Some("default") && sess.user.role != "owner" {
        return Err(ConsoleApiError::Forbidden(
            "only owner can create default-visibility dashboards".into(),
        ));
    }
    Ok(())
}

/// Apply a requested `managed` flag change. `managed` is a privileged
/// provenance marker: only admin/owner may change it (this prevents an editor
/// from making a shared dashboard un-deletable). Any other caller's value is
/// ignored and the existing flag is preserved, so a full-object round-trip
/// from a normal user is safe.
fn apply_managed(sess: &AuthSession, dash: &mut Dashboard, requested: Option<bool>) {
    if let Some(m) = requested {
        if matches!(sess.user.role.as_str(), "admin" | "owner") {
            dash.managed = m;
        }
    }
}

fn require_active(sess: &AuthSession) -> ConsoleResult<()> {
    if sess.user.status != UserStatus::Active {
        return Err(ConsoleApiError::Forbidden("user disabled".into()));
    }
    Ok(())
}

fn enforce_etag(headers: &HeaderMap, current_version: u64) -> ConsoleResult<()> {
    // If-Match optional but recommended; if present, must match the current
    // version (in either weak `W/"7"` or unweak `"7"` form).
    let Some(if_match) = headers.get("if-match").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let trimmed = if_match.trim();
    let unwrapped = trimmed.trim_start_matches("W/").trim_matches('"');
    match unwrapped.parse::<u64>() {
        Ok(v) if v == current_version => Ok(()),
        _ => Err(ConsoleApiError::Conflict(format!(
            "etag mismatch (current = W/\"{current_version}\")"
        ))),
    }
}

#[cfg(test)]
mod tests {
    /// Locks the module-doc disclosure that SSE live push (`/_stream`)
    /// is NOT implemented: no route registered by this crate may expose
    /// a `_stream` (server-sent-events) endpoint. If real-time push is
    /// ever added, this assertion fails on purpose so the disclosure at
    /// the top of this module — and the crate-level "Coming after RC"
    /// list in `lib.rs` — is updated in the same change instead of
    /// silently drifting out of date.
    #[test]
    fn no_sse_stream_endpoint_is_registered() {
        let offenders: Vec<&str> = crate::router::known_routes()
            .iter()
            .copied()
            .filter(|route| route.contains("_stream"))
            .collect();
        assert!(
            offenders.is_empty(),
            "module doc states SSE /_stream is not implemented, \
             but these routes exist: {offenders:?}"
        );
    }
}
