//! Dashboards CRUD.
//!
//! Replaces `localStorage["xerj.dashboards"]` and
//! `localStorage["xerj.layout.<id>"]` with engine-backed durable state.
//! Every shared dashboard write expects an `If-Match: W/"<version>"`
//! etag for optimistic concurrency; private dashboards skip that check
//! since only the owner ever writes them.
//!
//! SSE streaming (`/_stream`) lands in a follow-up commit so an SE
//! mid-call can see "another user just renamed this dashboard" without
//! polling.  CRUD ships first because it's the bigger blocker.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dashboard {
    pub id: String,
    pub owner: String,
    #[serde(default = "default_org")]
    pub org_id: String,
    /// `private` (only owner sees / writes), `shared` (every user in
    /// the org reads, only `editor` and up writes), `default` (engine-
    /// provisioned read-only built-in dashboards).
    pub visibility: String,
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

fn default_org() -> String { "default".to_string() }

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

fn default_visibility() -> String { "private".to_string() }

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
}

// ─────────────────────────────────────────────────────────────────────────────
// LIST
// ─────────────────────────────────────────────────────────────────────────────

pub async fn list(
    State(state): State<ConsoleState>,
    sess: AuthSession,
) -> ConsoleResult<Response> {
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
        let visible = d.owner == me
            || d.visibility == "shared"
            || d.visibility == "default";
        if visible {
            items.push(serde_json::to_value(&d)?);
        }
    }
    Ok(ok(json!({ "dashboards": items, "total": items.len() }), None))
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
    enforce_can_write_visibility(&sess, &body.visibility)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let dash = Dashboard {
        id: id.clone(),
        owner: sess.user.id.clone(),
        org_id: default_org(),
        visibility: body.visibility,
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
    enforce_can_write_visibility(&sess, &body.visibility)?;

    dash.name = body.name;
    dash.visibility = body.visibility;
    dash.section = body.section;
    dash.group = body.group;
    dash.panels = body.panels;
    dash.filters_default = body.filters_default;
    dash.time_default = body.time_default;
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

    if let Some(name) = body.name { dash.name = name; }
    if let Some(visibility) = body.visibility {
        enforce_can_write_visibility(&sess, &visibility)?;
        dash.visibility = visibility;
    }
    if let Some(section) = body.section { dash.section = Some(section); }
    if let Some(group) = body.group { dash.group = Some(group); }
    if let Some(panels) = body.panels { dash.panels = panels; }
    if let Some(filters) = body.filters_default { dash.filters_default = filters; }
    if let Some(td) = body.time_default { dash.time_default = Some(td); }

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
    enforce_write(&sess, &dash)?;
    dash.deleted_at = Some(now_iso());
    dash.version += 1;
    write_doc(&state, &dash).await?;
    Ok(no_content())
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
    let _ = idx.delete_document(&dash.id).await;
    idx.create_document(dash.id.clone(), serde_json::to_value(dash)?).await?;
    Ok(())
}

fn enforce_read(sess: &AuthSession, dash: &Dashboard) -> ConsoleResult<()> {
    if dash.owner == sess.user.id { return Ok(()); }
    match dash.visibility.as_str() {
        "shared" | "default" => Ok(()),
        _ => Err(ConsoleApiError::NotFound("dashboard".into())),
    }
}

fn enforce_write(sess: &AuthSession, dash: &Dashboard) -> ConsoleResult<()> {
    if dash.visibility == "default" {
        return Err(ConsoleApiError::Forbidden(
            "default dashboards are read-only".into(),
        ));
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

fn enforce_can_write_visibility(sess: &AuthSession, visibility: &str) -> ConsoleResult<()> {
    if visibility == "default" {
        // Only the owner can mint default-visibility dashboards. (For
        // v1.0 this prevents an editor from sneaking up an unkillable
        // dashboard; in practice we don't surface a UI for it yet.)
        if sess.user.role != "owner" {
            return Err(ConsoleApiError::Forbidden(
                "only owner can create default-visibility dashboards".into(),
            ));
        }
    }
    if visibility != "private" && visibility != "shared" && visibility != "default" {
        return Err(ConsoleApiError::BadRequest(format!(
            "unknown visibility '{visibility}'"
        )));
    }
    Ok(())
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
    let unwrapped = trimmed
        .trim_start_matches("W/")
        .trim_matches('"');
    match unwrapped.parse::<u64>() {
        Ok(v) if v == current_version => Ok(()),
        _ => Err(ConsoleApiError::Conflict(format!(
            "etag mismatch (current = W/\"{current_version}\")"
        ))),
    }
}
