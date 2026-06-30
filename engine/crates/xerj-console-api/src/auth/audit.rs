//! Append a row to `.xerj_audit` for every auth-relevant write.
//!
//! Best-effort: a failed audit write returns Ok(()) and logs at WARN.
//! We never fail the user-facing request because audit was unavailable —
//! that would let a transient engine error block all logins.

use serde_json::json;
use uuid::Uuid;
use xerj_engine::Engine;

use crate::indices;
use crate::time::now_iso;

/// Append an audit row. `who` is the principal ("system" for unauthenticated
/// flows like login_begin), `action` is short kebab-case
/// (e.g. `"magic-redeemed"`, `"passkey-enrolled"`, `"session-minted"`).
pub async fn record(
    engine: &Engine,
    who: &str,
    action: &str,
    resource: &str,
    resource_id: Option<&str>,
    ip: Option<&str>,
    extra: Option<serde_json::Value>,
) {
    let id = Uuid::new_v4().to_string();
    let mut doc = json!({
        "who":         who,
        "when":        now_iso(),
        "action":      action,
        "resource":    resource,
        "resource_id": resource_id,
        "ip":          ip,
    });
    if let Some(extra) = extra {
        doc["extra"] = extra;
    }

    let idx = match engine.get_index(indices::AUDIT) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "audit: cannot open .xerj_audit index");
            return;
        }
    };
    if let Err(e) = idx.create_document(id, doc).await {
        tracing::warn!(error = %e, action, "audit write failed");
    }
}
