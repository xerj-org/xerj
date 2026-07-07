//! Role-based access control — v0.9 9-P2 (skeleton).
//!
//! Minimal RBAC primitives that downstream auth middleware can grow
//! into.  v0.9.0-alpha.1 ships:
//!
//! - `Privilege` enum covering the seven core ops (read / write / admin
//!   index, snapshot create / restore, security admin, audit read).
//! - `Role` — name + privileges + index-pattern allow list.
//! - `RoleStore` — in-memory map of roles, default seeded with
//!   `admin`, `write`, `read`, `read_only_index`, `snapshot_admin`.
//!
//! v0.9.0-beta.1 will wire these into the auth middleware on every
//! handler and add the `PUT /_security/role/{name}` /
//! `PUT /_security/user/{name}` endpoints.  Field-level / document-level
//! security (FLS / DLS) lands in v0.9.0-rc.1.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}
