//! Cluster awareness endpoints.
//!
//! Phase 1 ships only `/cluster/info` — the standalone-vs-raft probe the
//! Xerj Console SPA reads at boot to decide whether to render the topology
//! widget. Full RAFT endpoints (`/cluster/raft`, `/cluster/peers`,
//! `/cluster/replication`, admin actions) land in phase 4 once the
//! identity and prefs paths have been proven end-to-end.

use axum::{extract::State, response::Response};
use serde_json::json;

use crate::error::ConsoleResult;
use crate::response::ok;
use crate::state::ConsoleState;

/// `GET /_xerj-console/api/v1/cluster/info`
///
/// Returns enough for the SPA to:
/// - Show "node up since…" without a separate /uptime fetch.
/// - Render `mode == "standalone"` (single-node card, hide topology
///   widget) or `mode == "raft"` (will light up `/cluster/raft` etc.
///   in phase 4 — until then the SPA falls back to standalone view).
pub async fn info(State(state): State<ConsoleState>) -> ConsoleResult<Response> {
    use crate::state::ClusterMode;

    let mode = match state.cluster_mode {
        ClusterMode::Standalone => "standalone",
        ClusterMode::Raft => "raft",
    };

    let body = json!({
        "mode":          mode,
        "node_id":       state.node_id.as_str(),
        "version":       env!("CARGO_PKG_VERSION"),
        "started_at_ms": state.started_at.0,
    });

    Ok(ok(body, None))
}
