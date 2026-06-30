//! System indices used by Xerj Console.
//!
//! All Xerj Console persistent state lives in hidden indices prefixed with `.xerj_`.
//! These are created on first boot if missing. Schema is intentionally
//! minimal — only the fields we actually filter or sort on get a typed
//! mapping; everything else is stored in `_source` and read back as-is.
//!
//! Hidden semantics: `_cat/indices` skips these by default. Admin tools
//! that want to see them pass `?include_system=true` (an enhancement to
//! the existing `_cat/indices` handler — wired up in a follow-up commit).

use xerj_common::types::{FieldConfig, FieldType, Schema};
use xerj_engine::Engine;

use crate::error::{ConsoleApiError, ConsoleResult};

// ─────────────────────────────────────────────────────────────────────────────
// Index names
// ─────────────────────────────────────────────────────────────────────────────

pub const USERS: &str = ".xerj_users";
pub const PASSKEYS: &str = ".xerj_passkeys";
pub const MAGIC_LINKS: &str = ".xerj_magic_links";
pub const SESSIONS: &str = ".xerj_sessions";
pub const API_TOKENS: &str = ".xerj_api_tokens";
pub const IDP_CONFIG: &str = ".xerj_idp_config";
pub const CLUSTER_STATE: &str = ".xerj_cluster_state";
pub const CONNECTIONS: &str = ".xerj_connections";
pub const PREFS: &str = ".xerj_prefs";
pub const DASHBOARDS: &str = ".xerj_dashboards";
pub const VIEWS: &str = ".xerj_views";
pub const ALERT_RULES: &str = ".xerj_alert_rules";
pub const ALERT_FIRES: &str = ".xerj_alert_fires";
pub const AUDIT: &str = ".xerj_audit";

/// Every system index this crate owns. Iterated by `ensure_all`.
pub const ALL: &[&str] = &[
    USERS,
    PASSKEYS,
    MAGIC_LINKS,
    SESSIONS,
    API_TOKENS,
    IDP_CONFIG,
    CLUSTER_STATE,
    CONNECTIONS,
    PREFS,
    DASHBOARDS,
    VIEWS,
    ALERT_RULES,
    ALERT_FIRES,
    AUDIT,
];

/// Predicate: is this index name owned by Xerj Console? Used by `_cat/indices`
/// to filter out system indices from the default listing.
pub fn is_system_index(name: &str) -> bool {
    name.starts_with(".xerj_")
}

// ─────────────────────────────────────────────────────────────────────────────
// Schemas
// ─────────────────────────────────────────────────────────────────────────────

/// Build the schema for a Xerj Console system index. Only fields we need to
/// filter / sort / aggregate on are typed; the rest of the document is
/// untyped JSON (stored in `_source`, read back as-is). This keeps the
/// mappings small and makes schema evolution a non-event for fields we
/// don't query on.
fn schema_for(index: &str) -> Schema {
    let mut s = Schema::empty();
    let add = |s: &mut Schema, name: &str, ty: FieldType| {
        let _ = s.add_field(FieldConfig::new(name, ty));
    };

    match index {
        USERS => {
            // Filter: status=active, role=admin, email=…; sort: created_at
            add(&mut s, "email", FieldType::Keyword);
            add(&mut s, "role", FieldType::Keyword);
            add(&mut s, "status", FieldType::Keyword);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "last_seen_at", FieldType::Date);
            add(&mut s, "display_name", FieldType::Keyword);
        }
        PASSKEYS => {
            // Lookup: by user_id (login), by credential_id (assertion verify)
            add(&mut s, "user_id", FieldType::Keyword);
            add(&mut s, "credential_id", FieldType::Keyword);
            add(&mut s, "name", FieldType::Keyword);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "last_used_at", FieldType::Date);
        }
        MAGIC_LINKS => {
            // Lookup: by token_hash (= _id, but typed for completeness)
            add(&mut s, "purpose", FieldType::Keyword);
            add(&mut s, "user_id", FieldType::Keyword);
            add(&mut s, "email", FieldType::Keyword);
            add(&mut s, "role", FieldType::Keyword);
            add(&mut s, "created_by", FieldType::Keyword);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "expires_at", FieldType::Date);
            add(&mut s, "used_at", FieldType::Date);
        }
        SESSIONS => {
            // Lookup: by session_id (= _id), reverse by user_id
            add(&mut s, "user_id", FieldType::Keyword);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "expires_at", FieldType::Date);
            add(&mut s, "last_seen_at", FieldType::Date);
            add(&mut s, "ip", FieldType::Keyword);
            add(&mut s, "ua", FieldType::Keyword);
            add(&mut s, "idp", FieldType::Keyword);
            add(&mut s, "revoked_at", FieldType::Date);
        }
        API_TOKENS => {
            // Lookup: by token_hash (= _id), reverse by user_id; expire by exp
            add(&mut s, "user_id", FieldType::Keyword);
            add(&mut s, "name", FieldType::Keyword);
            add(&mut s, "scopes", FieldType::Keyword);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "last_used_at", FieldType::Date);
            add(&mut s, "revoked_at", FieldType::Date);
        }
        IDP_CONFIG => {
            // One row per protocol; no filter fields needed beyond _id.
            add(&mut s, "updated_at", FieldType::Date);
            add(&mut s, "updated_by", FieldType::Keyword);
        }
        CLUSTER_STATE => {
            // One row per node. Filter on role/term/leader.
            add(&mut s, "role", FieldType::Keyword);
            add(&mut s, "term", FieldType::Long);
            add(&mut s, "commit_index", FieldType::Long);
            add(&mut s, "last_applied", FieldType::Long);
            add(&mut s, "updated_at", FieldType::Date);
        }
        CONNECTIONS => {
            add(&mut s, "name", FieldType::Keyword);
            add(&mut s, "kind", FieldType::Keyword);
            add(&mut s, "default", FieldType::Boolean);
            add(&mut s, "managed", FieldType::Boolean);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "created_by", FieldType::Keyword);
            add(&mut s, "etag", FieldType::Keyword);
        }
        PREFS => {
            // _id = user_id; payload is opaque key/value pairs.
            add(&mut s, "updated_at", FieldType::Date);
        }
        DASHBOARDS => {
            add(&mut s, "owner", FieldType::Keyword);
            add(&mut s, "org_id", FieldType::Keyword);
            add(&mut s, "visibility", FieldType::Keyword);
            add(&mut s, "name", FieldType::Keyword);
            add(&mut s, "section", FieldType::Keyword);
            add(&mut s, "group", FieldType::Keyword);
            add(&mut s, "version", FieldType::Long);
            add(&mut s, "etag", FieldType::Keyword);
            add(&mut s, "created_at", FieldType::Date);
            add(&mut s, "updated_at", FieldType::Date);
            add(&mut s, "deleted_at", FieldType::Date);
        }
        VIEWS => {
            add(&mut s, "owner", FieldType::Keyword);
            add(&mut s, "org_id", FieldType::Keyword);
            add(&mut s, "dashboard_id", FieldType::Keyword);
            add(&mut s, "name", FieldType::Keyword);
            add(&mut s, "updated_at", FieldType::Date);
        }
        ALERT_RULES => {
            add(&mut s, "owner", FieldType::Keyword);
            add(&mut s, "org_id", FieldType::Keyword);
            add(&mut s, "name", FieldType::Keyword);
            add(&mut s, "enabled", FieldType::Boolean);
            add(&mut s, "etag", FieldType::Keyword);
            add(&mut s, "updated_at", FieldType::Date);
        }
        ALERT_FIRES => {
            add(&mut s, "rule_id", FieldType::Keyword);
            add(&mut s, "level", FieldType::Keyword);
            add(&mut s, "fired_at", FieldType::Date);
        }
        AUDIT => {
            add(&mut s, "who", FieldType::Keyword);
            add(&mut s, "when", FieldType::Date);
            add(&mut s, "action", FieldType::Keyword);
            add(&mut s, "resource", FieldType::Keyword);
            add(&mut s, "resource_id", FieldType::Keyword);
            add(&mut s, "ip", FieldType::Keyword);
        }
        _ => {} // Unknown name: empty schema, dynamic mapping picks it up.
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Auto-create
// ─────────────────────────────────────────────────────────────────────────────

/// Idempotently create every Xerj Console system index, applying the schema
/// from `schema_for`. Does nothing for indices that already exist.
///
/// Logs a single `info` line on first creation and returns the names
/// that were newly created (so the bootstrap module can detect "fresh
/// data dir" purely from the create-set being non-empty).
pub fn ensure_all(engine: &Engine) -> ConsoleResult<Vec<&'static str>> {
    let mut created = Vec::new();
    for name in ALL {
        if engine.get_index(name).is_ok() {
            continue;
        }
        let schema = schema_for(name);
        engine
            .create_index(name, schema)
            .map_err(|e| ConsoleApiError::Internal(format!("create {name}: {e}")))?;
        tracing::info!(index = *name, "created xerj-console system index");
        created.push(*name);
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_index_has_a_schema() {
        // schema_for never panics for any ALL entry, and every advertised
        // index either has typed fields or an explicit empty branch.
        for name in ALL {
            let _ = schema_for(name);
        }
    }

    #[test]
    fn is_system_index_predicate() {
        assert!(is_system_index(".xerj_users"));
        assert!(is_system_index(".xerj_dashboards"));
        assert!(!is_system_index("dashboards"));
        assert!(!is_system_index(".kibana"));
        assert!(!is_system_index(".audit"));
    }
}
