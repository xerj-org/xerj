//! Saved views — named time-range + filter snapshots scoped to a
//! dashboard.
//!
//! Replaces `localStorage["xerj.views"]`.  Smaller surface than
//! dashboards: no etag concurrency (views are rarely co-edited), no
//! patch (clients DELETE-and-create instead).

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::sessions::AuthSession;
use crate::error::{ConsoleApiError, ConsoleResult};
use crate::indices;
use crate::response::{created, no_content, ok};
use crate::state::ConsoleState;
use crate::time::now_iso;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub owner: String,
    #[serde(default = "default_org")]
    pub org_id: String,
    pub dashboard_id: String,
    pub name: String,
    #[serde(default)]
    pub time: Option<Value>,
    #[serde(default)]
    pub filters: Option<Value>,
    pub updated_at: String,
}

fn default_org() -> String { "default".to_string() }

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub dashboard: Option<String>,
}

pub async fn list(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Query(q): Query<ListQuery>,
) -> ConsoleResult<Response> {
    let idx = state.engine.get_index(indices::VIEWS)?;
    let mut must: Vec<Value> = vec![json!({ "term": { "owner": sess.user.id } })];
    if let Some(dash) = q.dashboard {
        must.push(json!({ "term": { "dashboard_id": dash } }));
    }
    let body = json!({
        "query": { "bool": { "must": must } },
        "size": 200
    });
    let req = xerj_query::parser::parse_request(&body)
        .map_err(|e| ConsoleApiError::Internal(e.to_string()))?;
    let r = idx.search(&req).await?;
    let items: Vec<Value> = r.hits.into_iter().map(|h| h.source).collect();
    Ok(ok(json!({ "views": items, "total": items.len() }), None))
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub dashboard_id: String,
    pub name: String,
    #[serde(default)]
    pub time: Option<Value>,
    #[serde(default)]
    pub filters: Option<Value>,
}

pub async fn create(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Json(body): Json<CreateBody>,
) -> ConsoleResult<Response> {
    let id = uuid::Uuid::new_v4().to_string();
    let view = View {
        id: id.clone(),
        owner: sess.user.id.clone(),
        org_id: default_org(),
        dashboard_id: body.dashboard_id,
        name: body.name,
        time: body.time,
        filters: body.filters,
        updated_at: now_iso(),
    };
    let idx = state.engine.get_index(indices::VIEWS)?;
    idx.create_document(id.clone(), serde_json::to_value(&view)?).await?;
    let location = format!("/_xerj-console/api/v1/views/{id}");
    Ok(created(view, &location, None))
}

pub async fn get_one(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Path(id): Path<String>,
) -> ConsoleResult<Response> {
    let view = read(&state, &id).await?;
    if view.owner != sess.user.id {
        return Err(ConsoleApiError::NotFound("view".into()));
    }
    Ok(ok(view, None))
}

pub async fn delete(
    State(state): State<ConsoleState>,
    sess: AuthSession,
    Path(id): Path<String>,
) -> ConsoleResult<Response> {
    let view = read(&state, &id).await?;
    if view.owner != sess.user.id {
        return Err(ConsoleApiError::NotFound("view".into()));
    }
    let idx = state.engine.get_index(indices::VIEWS)?;
    let _ = idx.delete_document(&id).await;
    Ok(no_content())
}

async fn read(state: &ConsoleState, id: &str) -> ConsoleResult<View> {
    let idx = state.engine.get_index(indices::VIEWS)?;
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
        .ok_or_else(|| ConsoleApiError::NotFound(format!("view {id}")))?;
    Ok(serde_json::from_value(hit.source)?)
}
