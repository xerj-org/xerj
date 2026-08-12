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

    /// The `role_descriptors` key this role came from, with the per-entry
    /// index [`roles_from_role_descriptors`] appends stripped off.
    ///
    /// That function fans one descriptor out into one [`Role`] per `indices`
    /// entry and names them `"{descriptor}[{i}]"`, because the privileges and
    /// patterns differ per entry and the names have to stay distinct. `name`
    /// is therefore an internal identifier, not something a caller ever wrote.
    /// This is its inverse, and it lives here — beside the encoder — so the
    /// two cannot drift apart.
    ///
    /// Used by `GET /_security/_authenticate` to report the names the caller
    /// actually supplied (`reader`), not the encoding (`reader[0]`).
    ///
    /// Strips only a trailing `[<digits>]`, so a descriptor legitimately named
    /// `weird[x]` or `weird[]` comes back unchanged rather than mangled. A
    /// role built by any other means (the seeded [`RoleStore`] entries, a test)
    /// has no suffix and is returned as-is.
    pub fn descriptor_name(&self) -> &str {
        let Some(open) = self.name.rfind('[') else {
            return &self.name;
        };
        let Some(digits) = self.name[open + 1..].strip_suffix(']') else {
            return &self.name;
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return &self.name;
        }
        &self.name[..open]
    }

    /// Does this role apply to the named index?  Glob: "*" matches
    /// everything; literal names must match exactly; suffix-`*` (e.g.
    /// `logs-*`) matches by prefix.
    ///
    /// # `names: ["*"]` includes the reserved namespace, deliberately
    ///
    /// "Everything" means everything: a role granted `*` matches
    /// `.xerj-memory-alice-edges` like any other index, so a key minted with
    /// it can read and write every second brain and every agent-memory
    /// namespace on the node. `xerj_api::authz::may_reach_reserved` is *not*
    /// consulted on this path — it decides whether a name a caller is about to
    /// **create** (an alias, a fresh index, a template pattern) is squatting
    /// the reserved prefix, not whether an existing grant covers a read.
    ///
    /// This is an administrator's explicit choice rather than an escalation:
    /// the only principal that can mint a scoped key at all is the superuser
    /// (`POST /_security/api_key`), and `*` is the plainest possible way to
    /// write "this key gets the whole node". Narrowing it silently would break
    /// the operator who wrote `*` and meant it — a Kibana service account, a
    /// backup runner — with no error to explain the missing data.
    ///
    /// **Operators wanting brain isolation must not hand out `names: ["*"]`.**
    /// Grant the concrete indices, or a prefix that excludes the reserved
    /// namespace (`logs-*`, `app-*`); `.xerj-memory-*` is reachable only by a
    /// grant that reaches it. `a_star_grant_reaches_the_reserved_namespace` in
    /// `xerj_api::authz`'s tests pins this behaviour so it cannot be changed
    /// by accident.
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

/// Map one cluster-privilege name onto the cluster privileges this crate
/// enforces — currently only [`Privilege::AuditRead`] (issue #329).
///
/// Everything else in a `cluster` array still grants nothing, for the reason
/// [`roles_from_role_descriptors`] gives: honouring a privilege nothing checks
/// would be theatre. `all` is included because an operator who wrote
/// `cluster: ["all"]` asked for every cluster capability, and reading the audit
/// log is one; it still grants **no** index privilege, so it cannot be used to
/// reach data.
fn es_cluster_privilege(name: &str) -> &'static [Privilege] {
    use Privilege::*;
    match name {
        "all" | "read_audit" => &[AuditRead],
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
/// - `cluster` privileges are ignored **except** `read_audit` / `all`, which
///   grant [`Privilege::AuditRead`] and nothing else (issue #329) — every other
///   cluster name is still dropped, because honouring a privilege that nothing
///   checks would be theatre.
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
        // Cluster privileges, of which exactly one is enforced (issue #329).
        // A `cluster` array is otherwise still ignored — see the doc comment —
        // but `read_audit` has to be grantable or the gate on `/_audit/*` would
        // mean "superuser only", and the enterprise ask is precisely to hand an
        // auditor a credential that reads the audit log and nothing else.
        // Emitted as a role over `*` because [`Privilege::AuditRead`] is not
        // index-scoped: the pattern is irrelevant to the only check that reads
        // it (`xerj_api::authz::holds_audit_read`), and the privilege set
        // contains no index privilege, so this grants no access to any data.
        if let Some(cluster) = descriptor.get("cluster").and_then(|v| v.as_array()) {
            let privileges: HashSet<Privilege> = cluster
                .iter()
                .filter_map(|v| v.as_str())
                .flat_map(|p| es_cluster_privilege(p).iter().copied())
                .collect();
            if !privileges.is_empty() {
                roles.push(Role::new(
                    format!("{descriptor_name}[cluster]"),
                    privileges,
                    vec!["*".to_string()],
                ));
            }
        }
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
        assert!(!roles
            .iter()
            .any(|r| r.allows("anything", Privilege::ReadIndex)));
    }

    /// `descriptor_name` must be the exact inverse of the `"{name}[{i}]"`
    /// encoding `roles_from_role_descriptors` applies — that is what
    /// `GET /_security/_authenticate` reports, so a mismatch shows a caller a
    /// role name it never wrote.
    #[test]
    fn descriptor_name_inverts_the_per_entry_suffix() {
        let roles = roles_from_role_descriptors(&serde_json::json!({
            "alice-brain": {
                "indices": [
                    { "names": [".xerj-memory-alice-edges"], "privileges": ["read"] },
                    { "names": [".xerj-memory-alice"], "privileges": ["read"] }
                ]
            }
        }));
        assert_eq!(roles.len(), 2);
        assert_eq!(roles[0].name, "alice-brain[0]");
        assert_eq!(roles[1].name, "alice-brain[1]");
        // Both entries came from the one descriptor the caller named.
        for r in &roles {
            assert_eq!(r.descriptor_name(), "alice-brain");
        }

        // Only a trailing `[<digits>]` is a suffix we produced. Anything else
        // is part of the name the caller chose and must survive untouched —
        // stripping it would rename someone's role behind their back.
        for name in [
            "plain",
            "weird[x]",
            "weird[]",
            "trailing[",
            "]leading",
            "nested[0][a]",
        ] {
            let r = Role::new(name, HashSet::new(), vec![]);
            assert_eq!(r.descriptor_name(), name, "{name} must not be rewritten");
        }
        // …and ones that genuinely are our encoding, on awkward base names.
        let r = Role::new("has[brackets][2]", HashSet::new(), vec![]);
        assert_eq!(r.descriptor_name(), "has[brackets]");
        // JSON permits `"role_descriptors": {"": {...}}`, which encodes to
        // `"[0]"`. Inverting it to the empty name is faithful — that is what
        // the caller wrote — and is why this is an inverse rather than a
        // prettifier.
        let r = Role::new("[0]", HashSet::new(), vec![]);
        assert_eq!(r.descriptor_name(), "");
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
