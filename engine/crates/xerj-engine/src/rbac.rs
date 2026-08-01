//! Role-based access control — data model, plus the per-key grants that the
//! reserved-namespace authorization in `xerj-api::authz` enforces.
//!
//! ⚠️ HONEST-SURFACE WARNING, NARROWED (was RC4 item 6): the named
//! [`RoleStore`] below — the thing `PUT /_security/role/{name}` writes — is
//! still **data only**. Nothing in the auth path consults it, which is why the
//! `/_security/role*` handlers stamp every response with `"enforced": false`.
//! Broad RBAC over the general ES-compat surface (FLS/DLS, cluster privileges,
//! named-role binding) remains DEFERRED.
//!
//! What *is* enforced: the [`Role`] values attached to a **minted API key**
//! (`ApiKeyRecord::roles`, parsed from `role_descriptors` by
//! [`roles_from_role_descriptors`]). Those gate the reserved `.xerj-memory-*`
//! namespace — agent-memory namespaces and second brains — and confine a
//! scoped key to the indices it names. See `xerj-api::authz` for the decision
//! function and the enumeration of enforcement points.
//!
//! What ships today:
//! - `Privilege` enum covering the seven core ops (read / write / admin
//!   index, snapshot create / restore, security admin, audit read).
//! - `Role` — name + privileges + index-pattern allow list.
//! - `RoleStore` — in-memory map of roles, default seeded with
//!   `admin`, `write`, `read`, `read_only_index`, `snapshot_admin`, `auditor`.
//! - `roles_from_role_descriptors` — ES `role_descriptors` → `Vec<Role>`.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Privilege {
    /// Read documents and run searches/aggs.
    ReadIndex,
    /// Write (index, update, delete, bulk).
    WriteIndex,
    /// Admin (create / delete / settings / mappings).
    AdminIndex,
    /// Take snapshots.
    SnapshotCreate,
    /// Restore from snapshot.
    SnapshotRestore,
    /// Manage roles, users, API keys.
    SecurityAdmin,
    /// Read the audit log.
    AuditRead,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub privileges: HashSet<Privilege>,
    /// Glob patterns of index names the role applies to ("*" = all).
    pub indices: Vec<String>,
}

impl Role {
    pub fn new(
        name: impl Into<String>,
        privileges: HashSet<Privilege>,
        indices: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            privileges,
            indices,
        }
    }

    /// Does this role apply to the named index?  Glob: "*" matches
    /// everything; literal names must match exactly; suffix-`*` (e.g.
    /// `logs-*`) matches by prefix.
    pub fn applies_to(&self, idx: &str) -> bool {
        for pat in &self.indices {
            if pat == "*" || pat == idx {
                return true;
            }
            if let Some(prefix) = pat.strip_suffix('*') {
                if idx.starts_with(prefix) {
                    return true;
                }
            }
        }
        false
    }

    pub fn allows(&self, idx: &str, p: Privilege) -> bool {
        self.applies_to(idx) && self.privileges.contains(&p)
    }
}

/// Map one Elasticsearch index-privilege name onto this crate's [`Privilege`]s.
///
/// Unknown names map to **nothing** — an unrecognized privilege grants no
/// access rather than being waved through, so a typo (or an ES privilege xerj
/// has not implemented) fails closed.
fn es_index_privilege(name: &str) -> &'static [Privilege] {
    use Privilege::*;
    match name {
        "all" => &[ReadIndex, WriteIndex, AdminIndex],
        "read" | "read_cross_cluster" | "view_index_metadata" | "monitor" => &[ReadIndex],
        "write" | "index" | "create" | "create_doc" | "delete" | "maintenance" => &[WriteIndex],
        "manage" | "create_index" | "delete_index" | "manage_ilm" | "manage_follow_index" => {
            &[AdminIndex]
        }
        _ => &[],
    }
}

/// Parse an Elasticsearch-shaped `role_descriptors` object — exactly what
/// `POST /_security/api_key` already accepts on the wire — into the roles this
/// crate enforces:
///
/// ```json
/// { "alice-brain": { "indices": [
///     { "names": [".xerj-memory-alice-edges", ".xerj-memory-alice"],
///       "privileges": ["read", "write"] } ] } }
/// ```
///
/// One [`Role`] is emitted per `indices[]` entry (named `"{descriptor}[{i}]"`),
/// because a `Role` carries a single privilege set while one descriptor may
/// list several entries with different privileges.
///
/// Deliberate omissions, all of which fail closed (they grant nothing):
/// - `cluster` privileges are ignored — xerj has no cluster-privilege
///   enforcement, so honouring them would be theatre.
/// - `field_security` / `query` (FLS/DLS) are ignored; a descriptor that only
///   makes sense with FLS/DLS therefore over-grants at the field level within
///   an index it was already granted. Callers that need FLS must not rely on
///   this.
/// - Only a trailing `*` globs (see [`Role::applies_to`]); a mid-pattern `*`
///   matches nothing.
pub fn roles_from_role_descriptors(descriptors: &Value) -> Vec<Role> {
    let Some(map) = descriptors.as_object() else {
        return Vec::new();
    };
    let mut roles = Vec::new();
    for (descriptor_name, descriptor) in map {
        let Some(entries) = descriptor.get("indices").and_then(|v| v.as_array()) else {
            continue;
        };
        for (i, entry) in entries.iter().enumerate() {
            let names: Vec<String> = entry
                .get("names")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let privileges: HashSet<Privilege> = entry
                .get("privileges")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .flat_map(|p| es_index_privilege(p).iter().copied())
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() || privileges.is_empty() {
                // Nothing to grant. Skipping keeps `roles.is_empty()` an
                // accurate "this key was given no usable grant" signal.
                continue;
            }
            roles.push(Role::new(
                format!("{descriptor_name}[{i}]"),
                privileges,
                names,
            ));
        }
    }
    roles
}

pub struct RoleStore {
    roles: RwLock<HashMap<String, Role>>,
}

impl RoleStore {
    pub fn new() -> Arc<Self> {
        let mut roles = HashMap::new();
        // Seed the canonical roles operators expect.
        for r in default_roles() {
            roles.insert(r.name.clone(), r);
        }
        Arc::new(Self {
            roles: RwLock::new(roles),
        })
    }

    pub fn put(&self, role: Role) {
        self.roles.write().insert(role.name.clone(), role);
    }

    pub fn get(&self, name: &str) -> Option<Role> {
        self.roles.read().get(name).cloned()
    }

    pub fn delete(&self, name: &str) -> Option<Role> {
        self.roles.write().remove(name)
    }

    pub fn list(&self) -> Vec<Role> {
        self.roles.read().values().cloned().collect()
    }
}

fn default_roles() -> Vec<Role> {
    use Privilege::*;
    vec![
        Role::new(
            "admin",
            [
                ReadIndex,
                WriteIndex,
                AdminIndex,
                SnapshotCreate,
                SnapshotRestore,
                SecurityAdmin,
                AuditRead,
            ]
            .into_iter()
            .collect(),
            vec!["*".into()],
        ),
        Role::new(
            "write",
            [ReadIndex, WriteIndex].into_iter().collect(),
            vec!["*".into()],
        ),
        Role::new("read", [ReadIndex].into_iter().collect(), vec!["*".into()]),
        Role::new(
            "read_only_index",
            [ReadIndex].into_iter().collect(),
            vec![], // operator must add patterns explicitly
        ),
        Role::new(
            "snapshot_admin",
            [SnapshotCreate, SnapshotRestore].into_iter().collect(),
            vec!["*".into()],
        ),
        Role::new(
            "auditor",
            [AuditRead].into_iter().collect(),
            vec!["*".into()],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_seeded() {
        let s = RoleStore::new();
        assert!(s.get("admin").is_some());
        assert!(s.get("read").is_some());
        assert!(s.get("write").is_some());
        assert!(s.get("auditor").is_some());
    }

    #[test]
    fn admin_allows_all_on_all_indices() {
        let s = RoleStore::new();
        let admin = s.get("admin").unwrap();
        assert!(admin.allows("anything", Privilege::WriteIndex));
        assert!(admin.allows("logs-prod", Privilege::AdminIndex));
    }

    #[test]
    fn read_only_index_default_denies_all() {
        let s = RoleStore::new();
        let r = s.get("read_only_index").unwrap();
        assert!(!r.allows("logs-prod", Privilege::ReadIndex));
        assert!(!r.allows("*", Privilege::ReadIndex));
    }

    #[test]
    fn glob_index_pattern_matches() {
        let r = Role::new(
            "logs-reader",
            [Privilege::ReadIndex].into_iter().collect(),
            vec!["logs-*".into()],
        );
        assert!(r.allows("logs-prod", Privilege::ReadIndex));
        assert!(r.allows("logs-stage", Privilege::ReadIndex));
        assert!(!r.allows("metrics-prod", Privilege::ReadIndex));
        assert!(!r.allows("logs-prod", Privilege::WriteIndex));
    }

    #[test]
    fn role_descriptors_parse_into_index_grants() {
        let roles = roles_from_role_descriptors(&serde_json::json!({
            "alice-brain": {
                "cluster": ["all"],
                "indices": [
                    { "names": [".xerj-memory-alice-edges"], "privileges": ["read", "write"] },
                    { "names": [".xerj-memory-alice"], "privileges": ["read"] }
                ]
            }
        }));
        assert_eq!(roles.len(), 2, "one Role per indices[] entry");
        assert!(roles
            .iter()
            .any(|r| r.allows(".xerj-memory-alice-edges", Privilege::WriteIndex)));
        assert!(roles
            .iter()
            .any(|r| r.allows(".xerj-memory-alice", Privilege::ReadIndex)));
        // The read-only entry does not silently grant writes, and nothing
        // grants anything on a brain that was never named.
        assert!(!roles
            .iter()
            .any(|r| r.allows(".xerj-memory-alice", Privilege::WriteIndex)));
        assert!(!roles
            .iter()
            .any(|r| r.allows(".xerj-memory-bob-edges", Privilege::ReadIndex)));
        // `cluster` privileges are ignored, so "all" there granted no index.
        assert!(!roles.iter().any(|r| r.allows("anything", Privilege::ReadIndex)));
    }

    #[test]
    fn unusable_role_descriptors_grant_nothing() {
        // Not an object, no `indices`, empty names, empty/unknown privileges —
        // every one of these must produce zero roles, so `roles.is_empty()`
        // stays a truthful "no grant" signal for the fail-closed path.
        for d in [
            serde_json::json!("all"),
            serde_json::json!({ "r": { "cluster": ["all"] } }),
            serde_json::json!({ "r": { "indices": [{ "names": [], "privileges": ["all"] }] } }),
            serde_json::json!({ "r": { "indices": [{ "names": ["x"], "privileges": [] }] } }),
            serde_json::json!({ "r": { "indices": [{ "names": ["x"], "privileges": ["bogus"] }] } }),
        ] {
            assert!(
                roles_from_role_descriptors(&d).is_empty(),
                "must grant nothing: {d}"
            );
        }
    }

    #[test]
    fn all_privilege_covers_read_write_admin() {
        let roles = roles_from_role_descriptors(&serde_json::json!({
            "r": { "indices": [{ "names": ["logs-*"], "privileges": ["all"] }] }
        }));
        let r = &roles[0];
        assert!(r.allows("logs-prod", Privilege::ReadIndex));
        assert!(r.allows("logs-prod", Privilege::WriteIndex));
        assert!(r.allows("logs-prod", Privilege::AdminIndex));
        assert!(!r.allows("metrics", Privilege::ReadIndex));
    }
}
