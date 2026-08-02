//! Per-resource authorization — *what* an authenticated caller may touch.
//!
//! [`crate::auth`] answers *who* the caller is ([`Principal`]); this module is
//! the decision function and the enforcement points. It exists because of
//! issue #79: a brain used to be isolated **by name only**, so any credential
//! that could reach the node could read, write and destroy every brain.
//!
//! ## The rule, in one sentence
//!
//! The `.xerj-memory-*` index namespace — every agent-memory namespace and
//! every second brain — is **reserved**: reaching it requires a principal that
//! holds the matching privilege on the specific index, and the only principals
//! that hold anything there are the superuser and a key explicitly granted the
//! index by name.
//!
//! ## The mistake this file is built to make impossible
//!
//! The first cut of #79 decided against the index named in the **URL path**.
//! That is not the index several handlers operate on. `_msearch` takes it from
//! an NDJSON header line, `_bulk` from an action line, `_mget` from
//! `docs[]._index`, `_aliases` from its action list, `_reindex` from
//! `source`/`dest`, a `terms` lookup and a `lookup` runtime field from deep
//! inside a query, an index template from a *pattern* that will match brains
//! created later, `_sql` from the table name in the statement — in every one of
//! those the path index is only a *default* that the body overrides. Four of
//! them were proven live against a running node: read another tenant's brain
//! through `_msearch`, forge and delete its edges through `_bulk`, read a
//! document through `_mget`, and launder a whole index into reach by pointing
//! an alias at it.
//!
//! A per-handler patch list would have closed those four and left the fifth.
//! So authorization is decided at two places that no handler can route around:
//!
//! 1. **Here, before the handler runs.** [`authz_middleware`] resolves the
//!    complete target set of a request — from the path *and* from the body —
//!    resolves aliases to concrete indices, and authorizes each one with the
//!    privilege the request actually needs. This produces the precise
//!    ES-shaped 403 and gets read-vs-write-vs-manage right.
//! 2. **In the engine, at the point the name becomes an index.**
//!    [`xerj_engine::index_guard`] installs the request's principal as a
//!    task-local visibility rule that `Engine::get_index`,
//!    `get_or_create_index`, `create_index`, `delete_index`,
//!    `list_indices` and `index_name_list` all consult. A handler cannot
//!    forget this check, because a handler does not call it — it is inside the
//!    only funnel to index data. Anything the first layer did not know to look
//!    for (a body shape added next year, an `_sql` table name, a scroll
//!    context) still fails closed there.
//!
//! Layer 2 answers "not found" rather than "forbidden", deliberately: it makes
//! a denied brain indistinguishable from an absent one, and it means fan-out
//! (`POST /_search`, `_cat/indices`, `logs-*`) *filters* instead of failing —
//! which is what lets the global verbs keep working for ordinary credentials
//! instead of being refused wholesale.
//!
//! ## Enforcement points (all of them)
//!
//! | Door | Enforced by |
//! |---|---|
//! | `POST /_graph/{brain}/link` | `graph_api::link` + middleware |
//! | `DELETE /_graph/{brain}/link/{edge_id}` | `graph_api::unlink` + middleware |
//! | `GET /_graph/{brain}/ego` | `graph_api::ego` + middleware |
//! | `GET /_graph/{brain}/overview` | `graph_api::overview` + middleware |
//! | `POST/GET/DELETE /_memory/{ns}[/…]` (the brain's *nodes* index) | `memory_api` + middleware |
//! | any ES-compat route naming a reserved index in the path | middleware |
//! | any route naming an index in its **body** (`_bulk`, `_msearch`, `_mget`, `_aliases`, `_reindex`, snapshot `indices`, `terms` lookups, `lookup` runtime fields, `POST /v1/indices`) | middleware ([`body_targets`]) |
//! | an index template whose `index_patterns` would own a brain's mapping | middleware ([`authorize_template_patterns`]) |
//! | an ML job/datafeed config naming the index the server will go and read | middleware ([`BodyShape::MlConfig`]) |
//! | the detached datafeed scorer `_start` spawns | `es_compat::spawn_datafeed_task`, carrying the starter's rule across the `tokio::spawn` |
//! | an **alias** pointing into the reserved namespace | middleware (resolved before the decision) + `Engine::get_index` (resolved before the guard) |
//! | index patterns (`logs-*`, `_all`, `*`) | expanded only over the principal's visible set ([`xerj_engine::index_guard`]) |
//! | unnamed fan-out (`POST /_search`, `/_bulk`, `/_all/*`) | authorized per named index; anything unnamed is filtered by the guard |
//! | enumeration (`GET /_mapping`, `/_cat/indices`, `/_resolve/index/*`) | filtered at `Engine::list_indices`, then pruned in the response |
//! | the native router's `/v1/indices/{name}/…` spelling of the same index | middleware ([`Target::Indices`] classifies both routers) |
//! | the native router's body-named create (`POST /v1/indices`) | middleware ([`BodyShape::NativeCreate`]) |
//! | the gRPC listener (`:8081`) | `xerj-server::grpc`, using [`Principal::allows_index`] |
//! | privilege escalation via `POST /_security/api_key` | `es_compat::security_create_api_key` |
//! | anything else that resolves an index name from anywhere | `xerj_engine::index_guard` |
//!
//! ## Fail-closed
//!
//! Matching the precedent set elsewhere in the tree (a corrupt sidecar, an
//! unknown marker version and an unsatisfiable doc all refuse rather than
//! guess), every unresolved case here denies:
//! - an unknown/expired/invalidated credential → [`Principal::Denied`] → deny;
//! - a key minted with no usable `role_descriptors` → [`Principal::Unscoped`]
//!   → **no** privilege on the reserved namespace;
//! - an unrecognized ES privilege name → grants nothing;
//! - a body this module must parse to find its target but cannot → deny;
//! - a snapshot or restore that names no indices (i.e. *all* of them,
//!   including brains) → superuser only;
//! - an index expression that resolves to nothing this principal can see →
//!   resolves to nothing, not to everything.
//!
//! ## What this does **not** claim
//!
//! Broad RBAC over the general ES-compat surface is still deferred: an
//! `Unscoped` key keeps its historical superuser-equivalent reach over
//! *ordinary* indices. This module makes the reserved namespace a real
//! boundary and confines a `Scoped` key to its grants; it does not turn xerj
//! into a general multi-tenant authorization system. `xerj_engine::rbac`'s
//! named `RoleStore` remains unenforced data.
//!
//! Nor does it second-guess an explicit grant. A scoped key minted with
//! `names: ["*"]` **can** read the reserved namespace, because `*` matches
//! every index and that is what the operator asked for; only a superuser can
//! mint one. See [`xerj_engine::rbac::Role::applies_to`] for the reasoning and
//! `a_star_grant_reaches_the_reserved_namespace` below for the pinned
//! behaviour. Operators who want brain isolation must grant concrete names or
//! a prefix that excludes `.xerj-memory-`.

// The decision functions return `Result<(), Response>` where `Err` IS the
// ready-to-send 403. That trips `clippy::result_large_err` (an `axum::Response`
// is a fat value), but boxing it would add an allocation on the deny path to
// satisfy a lint aimed at hot `Ok` paths, and would obscure that the error
// *is* the response.
#![allow(clippy::result_large_err)]

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::auth::{authenticate, Principal, AUTH_EXEMPT_PATHS};
use crate::error::{EsErrorBody, EsErrorResponse, EsRootCause};
use crate::state::AppState;
use xerj_engine::index_guard::IndexVisibility;
use xerj_engine::rbac::Privilege;

// ─────────────────────────────────────────────────────────────────────────────
// The reserved namespace
// ─────────────────────────────────────────────────────────────────────────────

/// Index-name prefix reserved for agent-memory namespaces (`memory_api`) and
/// second brains (`graph_api`). Kept in one place so the three writers of the
/// name — `memory_api::MEMORY_PREFIX`, `graph_api::edges_index` and this
/// module — cannot drift.
pub const RESERVED_INDEX_PREFIX: &str = ".xerj-memory-";

/// Is `index` inside the reserved namespace?
pub fn is_reserved_index(index: &str) -> bool {
    index.starts_with(RESERVED_INDEX_PREFIX)
}

/// The edges index backing brain `brain` (SECOND_BRAIN_SPEC §1). This is the
/// resource name a `role_descriptors` grant must name to unlock
/// `/_graph/{brain}/*`.
pub fn brain_edges_index(brain: &str) -> String {
    format!("{RESERVED_INDEX_PREFIX}{brain}-edges")
}

/// The index backing agent-memory namespace `ns` — which is also the default
/// *nodes* index of the brain of the same name.
pub fn memory_namespace_index(ns: &str) -> String {
    format!("{RESERVED_INDEX_PREFIX}{ns}")
}

/// Could this index *expression* (a literal name or a pattern) reach the
/// reserved namespace?
///
/// Deliberately conservative in the deny direction: a pattern is judged by the
/// literal text before its first `*`, and if that prefix is a prefix of — or is
/// prefixed by — the reserved prefix, the expression is treated as reaching it.
/// `*`, `_all`, `.*`, `.xerj-*` and `.xerj-memory-alice*` all qualify;
/// `logs-*` does not.
///
/// Read patterns are no longer *refused* on the strength of this — they are
/// expanded over the principal's visible set instead, which is both safe and
/// the only way a granted `logs-*` can work. It is still what decides whether
/// a name a caller is about to *create* (an alias, a fresh index) is squatting
/// the reserved namespace.
pub fn may_reach_reserved(expression: &str) -> bool {
    let expr = expression.trim();
    if expr.is_empty() {
        return false;
    }
    if expr == "_all" {
        return true;
    }
    let head = expr.split('*').next().unwrap_or("");
    if expr.contains('*') {
        head.starts_with(RESERVED_INDEX_PREFIX) || RESERVED_INDEX_PREFIX.starts_with(head)
    } else {
        is_reserved_index(expr)
    }
}

/// Is this expression a pattern rather than one concrete name?
fn is_pattern(expr: &str) -> bool {
    expr.contains('*') || expr == "_all"
}

// ─────────────────────────────────────────────────────────────────────────────
// Decisions
// ─────────────────────────────────────────────────────────────────────────────

/// Authorize `principal` for `privilege` on the single index `index`.
///
/// Ordered before any existence check on purpose: an unauthorized caller gets
/// the same 403 whether or not the resource exists, so 403-vs-404 cannot be
/// used to enumerate brains.
pub fn authorize_index(
    principal: &Principal,
    index: &str,
    privilege: Privilege,
) -> Result<(), Response> {
    if principal.allows_index(index, privilege) {
        Ok(())
    } else {
        Err(forbidden(principal, index, privilege))
    }
}

/// Authorize `principal` for `privilege` on brain `brain` — i.e. on its edges
/// index. Every `/_graph/{brain}/*` handler calls this before it does anything
/// else that could reveal whether the brain exists.
pub fn authorize_brain(
    principal: &Principal,
    brain: &str,
    privilege: Privilege,
) -> Result<(), Response> {
    authorize_index(principal, &brain_edges_index(brain), privilege)
}

/// Authorize `principal` for `privilege` on agent-memory namespace `ns`.
pub fn authorize_memory_namespace(
    principal: &Principal,
    ns: &str,
    privilege: Privilege,
) -> Result<(), Response> {
    authorize_index(principal, &memory_namespace_index(ns), privilege)
}

/// ES-shaped 403. Names the resource and the privilege the caller lacks — the
/// caller supplied the resource name, so echoing it back confirms nothing —
/// and points at the grant that would fix it.
pub fn forbidden(principal: &Principal, resource: &str, privilege: Privilege) -> Response {
    let action = match privilege {
        Privilege::ReadIndex => "read",
        Privilege::WriteIndex => "write",
        Privilege::AdminIndex => "manage",
        Privilege::SnapshotCreate => "create_snapshot",
        Privilege::SnapshotRestore => "restore_snapshot",
        Privilege::SecurityAdmin => "manage_security",
        Privilege::AuditRead => "read_audit",
    };
    let reason = format!(
        "action [{action}] is unauthorized for this credential on [{resource}]: mint a key whose \
         role_descriptors grant [{action}] on that index (POST /_security/api_key), or use the \
         configured admin key"
    );
    tracing::debug!(
        principal = principal.label(),
        resource,
        action,
        "authorization denied"
    );
    es_error(StatusCode::FORBIDDEN, reason)
}

fn es_error(status: StatusCode, reason: String) -> Response {
    let error_type = "security_exception".to_string();
    let body = EsErrorResponse {
        error: EsErrorBody {
            root_cause: vec![EsRootCause {
                error_type: error_type.clone(),
                reason: reason.clone(),
                resource_type: None,
                resource_id: None,
                index_uuid: None,
                index: None,
            }],
            error_type,
            reason,
            resource_type: None,
            resource_id: None,
            index_uuid: None,
            index: None,
            request_id: None,
        },
        status: status.as_u16(),
    };
    (status, Json(body)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// The request's visible set (layer 2)
// ─────────────────────────────────────────────────────────────────────────────

/// Adapts a [`Principal`] to the engine's [`IndexVisibility`] hook.
///
/// "Visible" is *any* index privilege, not a specific one: this layer decides
/// whether the principal may reach the index at all, which is the property that
/// closes a body-named target the middleware did not know to parse. Which
/// privilege the request needs on it stays the middleware's decision, so a
/// read-only grant still cannot write through the front door.
struct PrincipalVisibility(Principal);

impl IndexVisibility for PrincipalVisibility {
    fn visible(&self, index: &str) -> bool {
        self.0.allows_index(index, Privilege::ReadIndex)
            || self.0.allows_index(index, Privilege::WriteIndex)
            || self.0.allows_index(index, Privilege::AdminIndex)
    }
}

/// The visibility rule for `principal`, for installing around a request.
pub fn visibility_for(principal: &Principal) -> Arc<dyn IndexVisibility> {
    Arc::new(PrincipalVisibility(principal.clone()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Request classification
// ─────────────────────────────────────────────────────────────────────────────

/// What a request path targets.
///
/// `op_start` on [`Target::Indices`] is the segment index at which the
/// operation begins, because the two routers spell resources differently:
/// ES-compat puts the index first (`/{index}/_search`, op at 1) while the
/// native router nests it (`/v1/indices/{index}/search`, op at 3). Both are
/// doors into the reserved namespace, so both are classified here.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// `/_graph/{brain}/…`
    Brain(String),
    /// `/_memory/{namespace}[/…]`
    Memory(String),
    /// One or more index expressions, plus where the op segment starts.
    Indices(Vec<String>, usize),
    /// A cluster/global endpoint whose path names no index: `/_cat/*`,
    /// `/_mapping`, `/_nodes`, but also `/_search`, `/_bulk`, `/_msearch`.
    /// Reads are answered over the principal's visible set; mutations must
    /// name their targets in the body (which [`body_targets`] extracts and
    /// authorizes) or belong to a principal that holds the general surface.
    Cluster,
    /// Not authorization-relevant (health probes, the version banner).
    Exempt,
}

/// Read-only ops that are POSTed rather than GETed. Without this list a
/// `POST /{index}/_search` would be classified as a write. Covers both
/// spellings: ES-compat (`_search`) and native (`search`).
const POST_READ_OPS: &[&str] = &[
    "_search",
    "_count",
    "_msearch",
    "_mget",
    "_explain",
    "_validate",
    "_field_caps",
    "_termvectors",
    "_mtermvectors",
    "_search_shards",
    "_rank_eval",
    "_knn_search",
    "_pit",
    "_async_search",
    "_sql",
    "_esql",
    "_eql",
    "_analyze",
    "_graph",
    "_resolve",
    "_terms_enum",
    "_disk_usage",
    // Cluster endpoints that carry a body but change nothing: privilege
    // probes, template rendering, pipeline/painless simulation. Only ever
    // consulted for POST-shaped verbs, so `PUT /_ingest/pipeline/{id}` and
    // `PUT /_scripts/{id}` remain the mutations they are.
    "_security",
    "_ingest",
    "_render",
    "_scripts",
    // native router
    "search",
    "explain-plan",
    "encodings",
];

/// Index-lifecycle ops. These reshape or destroy the index itself, so they
/// need `manage`, not `write`.
const ADMIN_OPS: &[&str] = &[
    "_settings",
    "_mapping",
    "_mappings",
    "_close",
    "_open",
    "_shrink",
    "_split",
    "_clone",
    "_rollover",
    "_alias",
    "_aliases",
    "_cache",
    "_forcemerge",
    "_freeze",
    "_unfreeze",
    "_block",
    "_upgrade",
    "_migration",
];

/// Split a URI path into percent-decoded, non-empty segments.
fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect()
}

/// Minimal percent-decoder. The middleware compares raw path text against
/// index names, so `%2Exerj-memory-alice-edges` must not slip past a check
/// that `.xerj-memory-alice-edges` would fail. Invalid escapes are left
/// verbatim (they cannot decode into the reserved prefix).
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Classify a request path.
fn classify(path: &str) -> Target {
    // Probes, the scrape endpoint, and "who am I" — none of which name or
    // reveal an index. `_security/_authenticate` in particular must stay open
    // to a scoped key, or a caller cannot verify its own credential.
    if AUTH_EXEMPT_PATHS.contains(&path)
        || path == "/v1/metrics"
        || path == "/_security/_authenticate"
    {
        return Target::Exempt;
    }
    let segs = segments(path);
    let Some(first) = segs.first() else {
        // `GET /` — the version banner. No data.
        return Target::Exempt;
    };
    match first.as_str() {
        "_graph" => match segs.get(1) {
            Some(brain) => Target::Brain(brain.clone()),
            // No brain in the path: no route matches, let it 404.
            None => Target::Exempt,
        },
        "_memory" => match segs.get(1) {
            Some(ns) => Target::Memory(ns.clone()),
            None => Target::Exempt,
        },
        // Native router (`build_native_router`, :8080). Same engine, same
        // indices, different spelling — and therefore the same boundary:
        // `GET /v1/indices/.xerj-memory-alice-edges` must be no more reachable
        // than `GET /.xerj-memory-alice-edges`.
        "v1" => match (segs.get(1).map(String::as_str), segs.get(2)) {
            (Some("indices"), Some(expr)) | (Some("schema"), Some(expr)) => {
                Target::Indices(split_expressions(expr), 3)
            }
            // `POST /v1/indices` names its index in the body — authorized as
            // `BodyShape::NativeCreate`, not refused for lack of a path target.
            _ => Target::Cluster,
        },
        // `/_all/_search` and `/*/_search` are index-scoped routes whose index
        // happens to be a pattern — not cluster endpoints. Classifying them
        // here is what lets them expand over the caller's visible set like any
        // other pattern instead of being refused for naming nothing.
        s if s == "*" || s == "_all" => Target::Indices(split_expressions(s), 1),
        s if s.starts_with('_') => Target::Cluster,
        s => Target::Indices(split_expressions(s), 1),
    }
}

/// Split a comma-separated multi-index expression (`logs-a,logs-b`).
fn split_expressions(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect()
}

/// The privilege a request needs on whatever it targets.
///
/// Conservative by construction: anything not recognized as a read or a
/// lifecycle op is treated as a write, so an unclassified mutation can never
/// be waved through as a read.
fn required_privilege(method: &Method, segs: &[String], op_start: usize) -> Privilege {
    if method == Method::GET || method == Method::HEAD {
        return Privilege::ReadIndex;
    }
    match segs.get(op_start).map(String::as_str) {
        Some(o) if POST_READ_OPS.contains(&o) => Privilege::ReadIndex,
        Some(o) if ADMIN_OPS.contains(&o) => Privilege::AdminIndex,
        // No op segment at all: the request targets the index itself, so it is
        // creating or destroying it.
        None => Privilege::AdminIndex,
        _ => Privilege::WriteIndex,
    }
}

/// Does this cluster-level request only *read*?
///
/// `GET`/`HEAD` always do. So does any verb whose op is one of the read ops:
/// `POST /_search`, `/_count`, `/_msearch`, `/_mget` are the reason the global
/// verbs are usable at all, and `DELETE /_search/scroll` / `DELETE /_pit` /
/// `DELETE /_async_search/{id}` release a session the caller already opened.
fn cluster_is_read(method: &Method, segs: &[String]) -> bool {
    if method == Method::GET || method == Method::HEAD {
        return true;
    }
    match segs.first().map(String::as_str) {
        Some(op) => POST_READ_OPS.contains(&op),
        None => true,
    }
}

/// The privilege a `/_graph/{brain}/…` or `/_memory/{ns}[/…]` request needs.
///
/// Kept separate from [`required_privilege`] because these paths spell their
/// ops without a leading underscore (`link`) and because deleting a whole
/// namespace is a lifecycle op while deleting one document is not.
fn reserved_api_privilege(method: &Method, segs: &[String]) -> Privilege {
    if method == Method::GET || method == Method::HEAD {
        return Privilege::ReadIndex;
    }
    if method == Method::POST {
        // `POST /_memory/{ns}/_recall` reads; `POST /_memory/{ns}` and
        // `POST /_graph/{b}/link` write. Creating the backing index lazily is
        // part of writing a brain, not a separate `manage` step.
        return if segs.iter().any(|s| s == "_recall") {
            Privilege::ReadIndex
        } else {
            Privilege::WriteIndex
        };
    }
    if method == Method::DELETE {
        // `DELETE /_memory/{ns}` drops the whole namespace → manage.
        // `DELETE /_memory/{ns}/{id}` and `DELETE /_graph/{b}/link/{id}` are
        // document-level → write.
        return if segs.len() <= 2 {
            Privilege::AdminIndex
        } else {
            Privilege::WriteIndex
        };
    }
    Privilege::WriteIndex
}

// ─────────────────────────────────────────────────────────────────────────────
// Body-named targets
// ─────────────────────────────────────────────────────────────────────────────

/// The shape in which a route's body can name the indices it operates on.
///
/// This table is the exhaustive answer to "which handlers take an index from
/// the request body". It is not the *only* protection — [`PrincipalVisibility`]
/// in the engine catches whatever is missing from it — but it is what produces
/// a precise 403 with the right privilege instead of a downstream "not found".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyShape {
    /// Nothing in the body names an index.
    None,
    /// `_bulk` NDJSON: `{"index"|"create"|"update"|"delete": {"_index": …}}`.
    Bulk,
    /// `_msearch` / `_msearch/template` NDJSON header lines: `{"index": …}`,
    /// where the value may be a string or an array of strings.
    MsearchHeaders,
    /// `_mget`: `{"docs": [{"_index": …}]}`.
    MgetDocs,
    /// `POST /_aliases`: `{"actions": [{"add"|"remove"|"remove_index": …}]}`.
    AliasActions,
    /// `_reindex`: `{"source": {"index": …}, "dest": {"index": …}}`.
    Reindex,
    /// Snapshot create / restore: `{"indices": "a,b" | ["a","b"]}`.
    SnapshotIndices,
    /// Native `POST /v1/indices`: `{"name": …}`.
    NativeCreate,
    /// `PUT /{index}`: the `aliases` block creates alias names.
    CreateIndex,
    /// Any search-shaped body — a `terms` lookup names another index to read
    /// its terms out of, at arbitrary depth.
    Query,
    /// `POST /_monitoring/bulk` writes one fixed internal index.
    MonitoringBulk,
    /// An index template's `index_patterns`. It names no index that exists
    /// yet, but it decides the mapping of every index created under a matching
    /// name later — including a brain — so the patterns are checked in their
    /// own right by [`authorize_template_patterns`].
    IndexTemplate,
    /// An ML job or datafeed config: `PUT /_ml/anomaly_detectors/{id}`
    /// (`source_index`, `index`, and the combined-form `datafeed_config`) and
    /// `PUT /_ml/datafeeds/{id}` (`indices` / `indexes`).
    ///
    /// These name an index the *server* will go and read, on a schedule, on
    /// the caller's behalf. Registering one against an index the caller cannot
    /// read is wrong on its own terms — it was accepted with a 200 before this
    /// arm existed — and it was also the front half of a live cross-brain
    /// read: the detached scorer `POST /_ml/datafeeds/{id}/_start` spawns had
    /// no visibility rule, so it fetched what the caller could not. The
    /// detached half is fixed at the spawn site (`es_compat`'s
    /// `spawn_datafeed_task` carries the rule across); this is the half that
    /// refuses the configuration in the first place.
    MlConfig,
}

/// Search-shaped ops whose body can carry a `terms` lookup.
const QUERY_BODY_OPS: &[&str] = &[
    "_search",
    "_count",
    "_explain",
    "_validate",
    "_delete_by_query",
    "_update_by_query",
    "_async_search",
    "_eql",
    "_rank_eval",
    "_terms_enum",
    "_field_caps",
    "_knn_search",
    "_pit",
    "_graph",
    "_sql",
    "_esql",
];

/// The index `POST /_monitoring/bulk` ingests into
/// (`es_compat::ingest_monitoring_ndjson`).
const MONITORING_INDEX: &str = "xerj-monitoring";

/// Which body shape does this route carry?
fn body_shape(method: &Method, segs: &[String]) -> BodyShape {
    let first = segs.first().map(String::as_str).unwrap_or("");
    // Native router. Its ingest bodies are plain document arrays under an
    // index named in the path; the only body-named index it has is the create.
    if first == "v1" {
        return match (segs.get(1).map(String::as_str), segs.len()) {
            (Some("indices"), 2) if method == Method::POST => BodyShape::NativeCreate,
            _ => BodyShape::None,
        };
    }
    let has = |op: &str| segs.iter().any(|s| s == op);
    if has("_bulk") {
        return BodyShape::Bulk;
    }
    if has("_msearch") {
        return BodyShape::MsearchHeaders;
    }
    if has("_mget") {
        return BodyShape::MgetDocs;
    }
    if first == "_aliases" && method != Method::GET {
        return BodyShape::AliasActions;
    }
    if first == "_reindex" {
        return BodyShape::Reindex;
    }
    if first == "_snapshot" {
        return BodyShape::SnapshotIndices;
    }
    if first == "_monitoring" {
        return BodyShape::MonitoringBulk;
    }
    if (first == "_index_template" || first == "_template") && method != Method::GET {
        return BodyShape::IndexTemplate;
    }
    // `PUT|POST /_ml/anomaly_detectors/{id}` and `PUT|POST /_ml/datafeeds/{id}`
    // — exactly three segments, so the sub-verbs (`…/{id}/_start`, `_stop`,
    // `_score`, `results/records`) are not swept in: they carry no index name
    // of their own, and their reach is decided by the config that was already
    // authorized here plus the guard the spawned scorer carries.
    if first == "_ml" && segs.len() == 3 && (method == Method::PUT || method == Method::POST) {
        return match segs[1].as_str() {
            "anomaly_detectors" | "datafeeds" => BodyShape::MlConfig,
            _ => BodyShape::None,
        };
    }
    if segs.len() == 1 && method == Method::PUT && !first.starts_with('_') {
        return BodyShape::CreateIndex;
    }
    if QUERY_BODY_OPS.iter().any(|op| has(op)) {
        return BodyShape::Query;
    }
    BodyShape::None
}

/// One index expression a request will touch, and what it needs on it.
type Demand = (String, Privilege);

/// Refuse a request whose body should have named its targets but could not be
/// parsed. Fail-closed: an unreadable target is not an absent one.
fn unresolvable(principal: &Principal, what: &str) -> Response {
    tracing::debug!(
        principal = principal.label(),
        what,
        "authorization refused: request body names indices but could not be resolved"
    );
    es_error(
        StatusCode::FORBIDDEN,
        format!(
            "this credential is authorized per index, and the {what} could not be resolved from \
             the request body; send a well-formed body that names its indices"
        ),
    )
}

/// Extract every index this request's **body** will touch, with the privilege
/// it needs on each.
///
/// `default_index` is the path index for the index-scoped spellings
/// (`/{index}/_bulk`, `/{index}/_msearch`, `/{index}/_mget`), which the body
/// may override — the override being the whole bug this exists for.
fn body_targets(
    principal: &Principal,
    shape: BodyShape,
    body: &[u8],
    default_index: Option<&str>,
) -> Result<Vec<Demand>, Response> {
    let mut out: Vec<Demand> = Vec::new();
    match shape {
        BodyShape::None => {}
        BodyShape::MonitoringBulk => {
            out.push((MONITORING_INDEX.to_string(), Privilege::WriteIndex))
        }
        BodyShape::Bulk => {
            // NDJSON, alternating action and (except for `delete`) source
            // lines. Every action line must parse — one that does not is a
            // target we cannot see, so the request is refused rather than
            // handed to a bulk pipeline that would happily route it.
            let Ok(text) = std::str::from_utf8(body) else {
                return Err(unresolvable(principal, "bulk action lines"));
            };
            // Fast path for the index-scoped spelling every client uses by
            // default: with no `_index` anywhere in the payload, every action
            // targets the path index, and there is nothing to find by parsing
            // a million action lines.
            if !text.contains("\"_index\"") {
                if let Some(d) = default_index {
                    out.push((d.to_string(), Privilege::WriteIndex));
                }
                return Ok(out);
            }
            let mut expect_action = true;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if !expect_action {
                    expect_action = true;
                    continue;
                }
                let Ok(action) = serde_json::from_str::<Value>(line) else {
                    return Err(unresolvable(principal, "bulk action lines"));
                };
                let Some(obj) = action.as_object() else {
                    return Err(unresolvable(principal, "bulk action lines"));
                };
                let Some((verb, meta)) = obj.iter().next() else {
                    return Err(unresolvable(principal, "bulk action lines"));
                };
                // `delete` carries no source line; everything else does.
                expect_action = verb == "delete";
                let named = meta.get("_index").and_then(Value::as_str);
                if let Some(index) = named.or(default_index) {
                    out.push((index.to_string(), Privilege::WriteIndex));
                }
            }
        }
        BodyShape::MsearchHeaders => {
            let Ok(text) = std::str::from_utf8(body) else {
                return Err(unresolvable(principal, "multi-search header lines"));
            };
            let mut expect_header = true;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(parsed) = serde_json::from_str::<Value>(line) else {
                    return Err(unresolvable(principal, "multi-search header lines"));
                };
                if expect_header {
                    match parsed.get("index") {
                        Some(Value::String(s)) => out.push((s.clone(), Privilege::ReadIndex)),
                        Some(Value::Array(items)) => {
                            for i in items {
                                if let Some(s) = i.as_str() {
                                    out.push((s.to_string(), Privilege::ReadIndex));
                                }
                            }
                        }
                        // A header that names no index means "the path index,
                        // else every index" — the latter is answered over the
                        // visible set, so nothing to demand here.
                        _ => {
                            if let Some(d) = default_index {
                                out.push((d.to_string(), Privilege::ReadIndex));
                            }
                        }
                    }
                } else {
                    // The search body half — it can still carry a terms lookup.
                    collect_query_body_indices(&parsed, &mut out);
                }
                expect_header = !expect_header;
            }
        }
        BodyShape::MgetDocs => {
            let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
                return Err(unresolvable(principal, "multi-get documents"));
            };
            match parsed.get("docs").and_then(Value::as_array) {
                Some(docs) => {
                    for doc in docs {
                        let named = doc.get("_index").and_then(Value::as_str);
                        match named.or(default_index) {
                            Some(index) => out.push((index.to_string(), Privilege::ReadIndex)),
                            None => return Err(unresolvable(principal, "multi-get documents")),
                        }
                    }
                }
                // `{"ids": [...]}` short form uses the path index.
                None => {
                    if let Some(d) = default_index {
                        out.push((d.to_string(), Privilege::ReadIndex));
                    }
                }
            }
        }
        BodyShape::AliasActions => {
            let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
                return Err(unresolvable(principal, "alias actions"));
            };
            let Some(actions) = parsed.get("actions").and_then(Value::as_array) else {
                return Err(unresolvable(principal, "alias actions"));
            };
            for action in actions {
                let Some(obj) = action.as_object() else {
                    return Err(unresolvable(principal, "alias actions"));
                };
                for (_verb, params) in obj {
                    // Pointing an alias at an index is a change to that index's
                    // addressing, so it needs `manage` on it — the same
                    // privilege `PUT /{index}/_alias/{alias}` already demands.
                    push_names(params.get("index"), Privilege::AdminIndex, &mut out);
                    push_names(params.get("indices"), Privilege::AdminIndex, &mut out);
                    // …and the alias NAME is authorized in its own right, so a
                    // caller cannot squat an alias inside the reserved
                    // namespace and have a brain resolve through it later.
                    push_names(params.get("alias"), Privilege::AdminIndex, &mut out);
                    push_names(params.get("aliases"), Privilege::AdminIndex, &mut out);
                }
            }
            if out.is_empty() {
                return Err(unresolvable(principal, "alias actions"));
            }
        }
        BodyShape::Reindex => {
            let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
                return Err(unresolvable(principal, "reindex source and destination"));
            };
            let before = out.len();
            if let Some(source) = parsed.get("source") {
                push_names(source.get("index"), Privilege::ReadIndex, &mut out);
                push_names(source.get("indices"), Privilege::ReadIndex, &mut out);
                if let Some(q) = source.get("query") {
                    collect_query_body_indices(q, &mut out);
                }
            }
            if let Some(dest) = parsed.get("dest") {
                push_names(dest.get("index"), Privilege::WriteIndex, &mut out);
            }
            if out.len() == before {
                return Err(unresolvable(principal, "reindex source and destination"));
            }
        }
        BodyShape::SnapshotIndices => {
            // Absent `indices` means "every index on the node", which for a
            // non-superuser would be a bulk read (or overwrite) of every
            // brain. `decide` turns an empty demand list on this route into a
            // refusal; here we only report what was named. A restore *writes*
            // its targets — `decide` upgrades the privilege, since only the
            // path says which of the two this is.
            if let Ok(parsed) = serde_json::from_slice::<Value>(body) {
                push_names(parsed.get("indices"), Privilege::ReadIndex, &mut out);
            }
        }
        BodyShape::NativeCreate => {
            let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
                return Err(unresolvable(principal, "index name"));
            };
            match parsed.get("name").and_then(Value::as_str) {
                Some(name) => out.push((name.to_string(), Privilege::AdminIndex)),
                None => return Err(unresolvable(principal, "index name")),
            }
        }
        BodyShape::CreateIndex => {
            // `PUT /{index}` names its index in the path (already demanded);
            // its body can additionally mint alias names.
            if let Ok(parsed) = serde_json::from_slice::<Value>(body) {
                if let Some(aliases) = parsed.get("aliases").and_then(Value::as_object) {
                    for alias in aliases.keys() {
                        out.push((alias.clone(), Privilege::AdminIndex));
                    }
                }
            }
        }
        BodyShape::Query => {
            if let Ok(parsed) = serde_json::from_slice::<Value>(body) {
                collect_query_body_indices(&parsed, &mut out);
            }
        }
        BodyShape::MlConfig => {
            let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
                // Not JSON: it names no index, and the handler will reject it
                // for the missing `source_index` / `job_id` anyway.
                return Ok(out);
            };
            // Every spelling the two ML handlers actually read. `put_ml_datafeed`
            // takes `indices` OR `indexes`; `put_ml_anomaly_detector` takes
            // `source_index` (string or array) OR `index`; the ES combined form
            // nests a whole datafeed under `datafeed_config`.
            let push_ml = |v: Option<&Value>, out: &mut Vec<Demand>| {
                push_names(v, Privilege::ReadIndex, out);
            };
            push_ml(parsed.get("source_index"), &mut out);
            push_ml(parsed.get("index"), &mut out);
            push_ml(parsed.get("indices"), &mut out);
            push_ml(parsed.get("indexes"), &mut out);
            if let Some(dfc) = parsed.get("datafeed_config") {
                push_ml(dfc.get("indices"), &mut out);
                push_ml(dfc.get("indexes"), &mut out);
                push_ml(dfc.get("index"), &mut out);
            }
            // A body that names nothing is NOT refused: `PUT /_ml/datafeeds/{id}`
            // may carry only a `job_id`, in which case the handler inherits the
            // detector's `source_index` — a name this middleware cannot see, but
            // one that was authorized when the detector itself was created, and
            // that the scorer re-checks under the starter's own visibility rule.
            // `decide` reads the empty list on this shape as "nothing named",
            // not as "unrestricted".
        }
        // Patterns, not names — `authorize_template_patterns` handles them.
        BodyShape::IndexTemplate => {}
    }
    // A bulk usually names two or three indices across a million action lines.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(rank(a.1).cmp(&rank(b.1))));
    out.dedup();
    Ok(out)
}

/// Stable ordering key for a privilege, so the demand list can be deduped
/// without asking `xerj_engine::rbac` for an `Ord` it has no other use for.
fn rank(p: Privilege) -> u8 {
    match p {
        Privilege::ReadIndex => 0,
        Privilege::WriteIndex => 1,
        Privilege::AdminIndex => 2,
        Privilege::SnapshotCreate => 3,
        Privilege::SnapshotRestore => 4,
        Privilege::SecurityAdmin => 5,
        Privilege::AuditRead => 6,
    }
}

/// An index template applies to indices that do not exist yet, so there is no
/// name to authorize — only a pattern. A template whose `index_patterns` can
/// match the reserved namespace would pick the mapping of a brain created
/// later, which is a write to that brain by another route, so a non-superuser
/// may not register one.
fn authorize_template_patterns(principal: &Principal, body: &[u8]) -> Result<(), Response> {
    let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
        // Not JSON: it names no pattern, and the handler will reject it.
        return Ok(());
    };
    let mut patterns: Vec<Demand> = Vec::new();
    push_names(
        parsed.get("index_patterns"),
        Privilege::AdminIndex,
        &mut patterns,
    );
    // ES 6-era `_template` spelled it `template`.
    push_names(parsed.get("template"), Privilege::AdminIndex, &mut patterns);
    for (pattern, _) in patterns {
        if !may_reach_reserved(&pattern) {
            continue;
        }
        // Only a principal that was *granted* the namespace may template over
        // it. `Unscoped::allows_index` answers "not reserved" for a pattern
        // like `*` — true of the literal text, useless as an answer here — so
        // it is excluded explicitly rather than consulted.
        let held = match principal {
            Principal::Superuser => true,
            Principal::Scoped { .. } => principal.allows_index(&pattern, Privilege::AdminIndex),
            _ => false,
        };
        if !held {
            return Err(forbidden(principal, &pattern, Privilege::AdminIndex));
        }
    }
    Ok(())
}

/// Push a `"a"` / `"a,b"` / `["a","b"]` index field onto the demand list.
fn push_names(v: Option<&Value>, privilege: Privilege, out: &mut Vec<Demand>) {
    match v {
        Some(Value::String(s)) => {
            for part in split_expressions(s) {
                out.push((part, privilege));
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    for part in split_expressions(s) {
                        out.push((part, privilege));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Collect every index a **search body** reads that the URL never names.
///
/// Two shapes, both found by auditing the tree rather than by being attacked —
/// which is the point of auditing:
///
/// - a `terms` lookup, `{"terms": {"field": {"index": "other", "id": "1",
///   "path": "vals"}}}`, fetches a document out of `other` and substitutes its
///   values into the query;
/// - a `lookup` runtime field, `{"runtime_mappings": {"f": {"type": "lookup",
///   "target_index": "other", …}}}`, joins rows out of `other` into the hits.
///
/// Both are matched structurally (the full lookup triple, the `type: lookup`
/// marker) rather than on the key name alone, so a document field that happens
/// to be called `index` is never mistaken for one.
fn collect_query_body_indices(q: &Value, out: &mut Vec<Demand>) {
    match q {
        Value::Object(obj) => {
            if let Some(Value::Object(terms)) = obj.get("terms") {
                for (field, spec) in terms {
                    if field == "boost" {
                        continue;
                    }
                    if let Some(s) = spec.as_object() {
                        // Only a full lookup triple actually fetches.
                        if s.contains_key("id") && s.contains_key("path") {
                            if let Some(ix) = s.get("index").and_then(Value::as_str) {
                                out.push((ix.to_string(), Privilege::ReadIndex));
                            }
                        }
                    }
                }
            }
            if obj.get("type").and_then(Value::as_str) == Some("lookup") {
                if let Some(ix) = obj.get("target_index").and_then(Value::as_str) {
                    out.push((ix.to_string(), Privilege::ReadIndex));
                }
            }
            for child in obj.values() {
                collect_query_body_indices(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_query_body_indices(item, out);
            }
        }
        _ => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Alias resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Resolves an alias to the indices behind it.
///
/// Authorization consults this **before** deciding, so `POST /pwned/_search`
/// on an alias pointed at someone else's brain is authorized against that
/// brain, not against the alias name. `Engine::get_index` resolves aliases too
/// and re-checks after doing so, which is what covers an alias created between
/// the decision and the read.
pub trait AliasResolver {
    fn targets(&self, name: &str) -> Option<Vec<String>>;
}

impl AliasResolver for AppState {
    fn targets(&self, name: &str) -> Option<Vec<String>> {
        self.engine.aliases.get(name).map(|e| e.value().clone())
    }
}

/// No aliases at all — used by the unit tests and as the fallback when a
/// decision is made without engine state.
pub struct NoAliases;

impl AliasResolver for NoAliases {
    fn targets(&self, _name: &str) -> Option<Vec<String>> {
        None
    }
}

/// Authorize one index expression, resolving aliases first.
///
/// A pattern is *not* refused: it is expanded over the principal's visible set
/// by the engine (see [`xerj_engine::index_guard`]), so a granted `logs-*`
/// works and an ungranted `*` silently resolves to nothing instead of leaking
/// what it would have matched.
fn authorize_expression(
    principal: &Principal,
    aliases: &dyn AliasResolver,
    expr: &str,
    privilege: Privilege,
) -> Result<(), Response> {
    if is_pattern(expr) {
        return Ok(());
    }
    match aliases.targets(expr) {
        Some(backing) if !backing.is_empty() => {
            for index in backing {
                authorize_index(principal, &index, privilege)?;
            }
            Ok(())
        }
        _ => authorize_index(principal, expr, privilege),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Middleware
// ─────────────────────────────────────────────────────────────────────────────

/// Largest response this middleware will buffer in order to prune reserved
/// index names out of it. Metadata listings are kilobytes; the cap only exists
/// so a pathological response cannot be turned into an OOM.
const MAX_PRUNABLE_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Authorization middleware for both routers.
///
/// Layered *inside* [`crate::auth::auth_middleware`] (added to the router
/// before it, so it runs after it): authentication has already rejected
/// anonymous callers, and this decides what the authenticated one may reach.
///
/// A superuser — open mode (`--insecure`, point-at-a-folder) or the configured
/// admin key — short-circuits on the first line, so the zero-configuration
/// local path pays nothing: no body buffering, no visibility guard, no
/// response pruning.
pub async fn authz_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let target = classify(&path);
    if matches!(target, Target::Exempt) {
        return next.run(req).await;
    }

    let principal = authenticate(
        &state,
        req.headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
    );
    if principal.is_superuser() {
        return next.run(req).await;
    }

    let segs = segments(&path);
    let method = req.method().clone();
    let shape = body_shape(&method, &segs);

    // Buffer the body only for the routes that can name an index inside it.
    // `Bytes` is refcounted, so handing the same buffer to the handler costs a
    // clone of a pointer, not of the payload.
    let (req, body) = if shape == BodyShape::None {
        (req, Bytes::new())
    } else {
        let limit = state.config.limits.max_body_bytes;
        let (parts, raw) = req.into_parts();
        match axum::body::to_bytes(raw, limit).await {
            Ok(bytes) => (Request::from_parts(parts, Body::from(bytes.clone())), bytes),
            Err(_) => {
                return es_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body exceeds limits.max_body_bytes and cannot be authorized"
                        .to_string(),
                )
            }
        }
    };

    if let Err(denied) = decide(&principal, &method, &segs, &target, shape, &body, &state) {
        return denied;
    }

    // Layer 2: whatever the handler resolves from here on — including names
    // this module never parsed — is checked against the same principal at the
    // engine's index funnel.
    let response =
        xerj_engine::index_guard::scoped(visibility_for(&principal), next.run(req)).await;
    prune_response(response, &segs, &principal, &state).await
}

/// The whole decision, split out so it can be unit-tested without a router.
#[allow(clippy::too_many_arguments)]
fn decide(
    principal: &Principal,
    method: &Method,
    segs: &[String],
    target: &Target,
    shape: BodyShape,
    body: &[u8],
    aliases: &dyn AliasResolver,
) -> Result<(), Response> {
    // Restated here and not only in the middleware, so this function is a
    // complete decision on its own and cannot deny the local-dev superuser if
    // it is ever called from somewhere else.
    if principal.is_superuser() {
        return Ok(());
    }
    if matches!(principal, Principal::Denied) {
        return Err(forbidden(principal, "<cluster>", Privilege::ReadIndex));
    }
    match target {
        Target::Exempt => Ok(()),
        Target::Brain(brain) => {
            authorize_brain(principal, brain, reserved_api_privilege(method, segs))
        }
        Target::Memory(ns) => {
            authorize_memory_namespace(principal, ns, reserved_api_privilege(method, segs))
        }
        Target::Indices(expressions, op_start) => {
            let privilege = required_privilege(method, segs, *op_start);
            for expr in expressions {
                authorize_expression(principal, aliases, expr, privilege)?;
            }
            // `PUT|DELETE /{index}/_alias/{alias}` mints or drops an alias
            // name; the name itself is a resource, or the reserved namespace
            // could be squatted by an alias nobody authorized.
            if segs.get(*op_start).map(String::as_str) == Some("_alias") {
                if let Some(alias) = segs.get(op_start + 1) {
                    authorize_index(principal, alias, Privilege::AdminIndex)?;
                }
            }
            // The path index is the default for a body that may override it —
            // `/{index}/_bulk`, `/{index}/_msearch`, `/{index}/_mget`.
            let default = expressions.first().map(String::as_str);
            authorize_body(principal, aliases, shape, body, default)
        }
        Target::Cluster => {
            // Snapshot and restore that name no indices cover *every* index on
            // the node, brains included. There is no per-index decision to
            // make, so they are superuser-only.
            let snapshotting = segs.first().map(String::as_str) == Some("_snapshot")
                && segs.len() >= 3
                && method != Method::GET
                && method != Method::HEAD;
            let restoring = segs.last().map(String::as_str) == Some("_restore");
            if shape == BodyShape::IndexTemplate {
                authorize_template_patterns(principal, body)?;
            }
            let mut demands = body_targets(principal, shape, body, None)?;
            if restoring {
                // A restore overwrites the indices it names.
                for demand in demands.iter_mut() {
                    demand.1 = Privilege::WriteIndex;
                }
            }
            if snapshotting && demands.is_empty() {
                return Err(forbidden(
                    principal,
                    "<all indices>",
                    if restoring {
                        Privilege::SnapshotRestore
                    } else {
                        Privilege::SnapshotCreate
                    },
                ));
            }
            for (expr, privilege) in &demands {
                authorize_expression(principal, aliases, expr, *privilege)?;
            }
            if cluster_is_read(method, segs) {
                // Cluster reads (`_cat`, `_mapping`, `_nodes`, `_cluster/*`,
                // and the global `_search`/`_count`/`_msearch`/`_mget`) are
                // answered over the principal's visible set, so a scoped key
                // gets a filtered cluster rather than a blanket 403 — which is
                // what Kibana needs to function at all.
                return Ok(());
            }
            if !demands.is_empty() {
                // A mutating cluster verb that named its targets: they are all
                // authorized above. `POST /_bulk`, `/_reindex` and `/_aliases`
                // live here — the global bulk is the default write path for
                // essentially every ES client.
                return Ok(());
            }
            if shape == BodyShape::MlConfig {
                // An ML config that named no index inherits one that was
                // authorized when the detector was created (see the `MlConfig`
                // arm of `body_targets`). Falling through to the
                // names-nothing branch below would 403 a scoped key for the
                // ordinary `PUT /_ml/datafeeds/{id} {"job_id": …}`, which
                // reaches nothing this principal was not already granted.
                return Ok(());
            }
            match principal {
                // A cluster-level mutation that names nothing: an unscoped key
                // keeps its historical reach over the general surface, a scoped
                // one has to name what it is changing.
                Principal::Unscoped { .. } => Ok(()),
                _ => Err(forbidden(
                    principal,
                    "<cluster>",
                    required_privilege(method, segs, 1),
                )),
            }
        }
    }
}

/// Authorize the indices a request's body names, if any.
fn authorize_body(
    principal: &Principal,
    aliases: &dyn AliasResolver,
    shape: BodyShape,
    body: &[u8],
    default_index: Option<&str>,
) -> Result<(), Response> {
    for (expr, privilege) in body_targets(principal, shape, body, default_index)? {
        authorize_expression(principal, aliases, &expr, privilege)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Enumeration pruning
//
// Which endpoints get pruned is decided by `prune_sites` / `cat_index_columns`
// below rather than by a list of path roots: an endpoint is prunable exactly
// when a position is known for it, so "we prune here" and "here is where the
// names are" cannot drift apart. Everything else — every search response
// included — is passed through without being buffered.
// ─────────────────────────────────────────────────────────────────────────────

/// Remove index names the principal cannot read from a metadata response.
///
/// `Engine::list_indices` already filters, so most listings arrive clean; this
/// is the second pass for the handful of handlers that read the
/// `index_settings` / `index_mappings` side maps directly, and it is what keeps
/// `GET /_mapping` from handing over the list of brains on the node.
async fn prune_response(
    response: Response,
    segs: &[String],
    principal: &Principal,
    state: &AppState,
) -> Response {
    let sites = prune_sites(segs);
    let columns = cat_index_columns(segs).unwrap_or(&[]);
    if sites.is_empty() && columns.is_empty() {
        // Nothing in this response names an index. Skip the buffer entirely —
        // it is also the only correct thing to do: pruning a body with no
        // index-name position in it can subtract, never protect.
        return response;
    }
    let (parts, body) = response.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let is_json = content_type.contains("json");
    let is_text = content_type.starts_with("text/");
    if !is_json && !is_text {
        return Response::from_parts(parts, body);
    }
    let bytes = match axum::body::to_bytes(body, MAX_PRUNABLE_RESPONSE_BYTES).await {
        Ok(b) => b,
        // Could not buffer it, so could not prune it. Refuse rather than ship
        // an unfiltered listing.
        Err(_) => {
            return es_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "response too large to authorize; name an explicit index".to_string(),
            )
        }
    };

    // The real index names on the node. A response key is only a candidate for
    // pruning when it IS one — otherwise `"mappings"`, `"settings"` and every
    // other structural key would be dropped for a scoped principal, which
    // would corrupt the very responses this is protecting. Read outside the
    // request's visibility scope on purpose: this needs the unfiltered truth.
    let known: HashSet<String> = state.engine.index_name_list().into_iter().collect();

    let pruned: Vec<u8> = if is_json {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(mut v) => {
                prune_json(&mut v, sites, principal, &known);
                serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec())
            }
            // Not JSON after all (some `_cat` handlers answer text under a
            // JSON content type); fall through to the line filter.
            Err(_) => prune_text(&bytes, columns, principal, &known),
        }
    } else {
        prune_text(&bytes, columns, principal, &known)
    };

    let mut parts = parts;
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(pruned))
}

/// Is this index name one the principal must not even see?
fn hidden(name: &str, principal: &Principal, known: &HashSet<String>) -> bool {
    (is_reserved_index(name) || known.contains(name))
        && !principal.allows_index(name, Privilege::ReadIndex)
}

/// One place in an endpoint's response where index **names** appear.
///
/// This exists because the first cut pruned by *key name at every depth*: any
/// key equal to a real index name was deleted, wherever it appeared. An index
/// name is an arbitrary string, so ordinary names collide with the structural
/// keys of the very responses being pruned, and the collision only bites a
/// scoped key — i.e. exactly the multi-tenant and Kibana case this whole
/// branch exists to enable. Measured, on a node with one ordinary index of
/// each name: `status` cost `GET /_cluster/health` its `status` field, the
/// single most-polled field in the API; `indices` cost `GET /_cluster/stats`
/// its entire `indices` section; `type` cost global `GET /_mapping` the `type`
/// of every field definition, which is the endpoint Kibana reads to build
/// index patterns.
///
/// So the positions are enumerated per endpoint instead, from the handlers'
/// actual response shapes. `path` walks object keys from the document root,
/// `/`-separated, where `*` matches any one key or array element and `""` is
/// the root itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    /// The object at `path` is **keyed** by index name.
    KeyedByIndex(&'static str),
    /// The array at `path` **holds** index names: bare strings, or objects
    /// whose `field` names one (`""` = bare strings only).
    NamedIn(&'static str, &'static str),
}

/// Where index names appear in this endpoint's JSON response.
///
/// Anything not listed prunes nothing, which is the right default: a response
/// that never names an index cannot leak one, and pruning it can only damage
/// it. `_cluster/stats` is the worked example — its `indices` object holds
/// aggregate counts, not a map keyed by index — and so are `_cluster/settings`,
/// `_cluster/pending_tasks`, `_nodes` and `/v1/cluster/health`.
fn prune_sites(segs: &[String]) -> &'static [Site] {
    // `_mapping`, `_settings`, `_alias`, `_aliases`, `_recovery` and
    // `_shard_stores` all answer one object keyed by index name at the root.
    const ROOT_KEYED: &[Site] = &[Site::KeyedByIndex("")];
    // `_stats` and `_segments` put the same map one level down.
    const INDICES_KEYED: &[Site] = &[Site::KeyedByIndex("indices")];
    match segs.first().map(String::as_str) {
        Some("_mapping")
        | Some("_mappings")
        | Some("_settings")
        | Some("_alias")
        | Some("_aliases")
        | Some("_recovery")
        | Some("_shard_stores") => ROOT_KEYED,
        Some("_stats") | Some("_segments") => INDICES_KEYED,
        Some("_cluster") => match segs.get(1).map(String::as_str) {
            // `level=indices` / `level=shards` add the per-index breakdown;
            // the root is cluster-wide scalars (`status`, `timed_out`, …).
            Some("health") => INDICES_KEYED,
            Some("state") => &[
                Site::KeyedByIndex("metadata/indices"),
                Site::KeyedByIndex("routing_table/indices"),
            ],
            _ => &[],
        },
        // `{"indices": [name], "fields": {f: {type: {…, "indices": [name]}}}}`
        Some("_field_caps") => &[
            Site::NamedIn("indices", ""),
            Site::NamedIn("fields/*/*/indices", ""),
        ],
        // `{"indices": [{name}], "aliases": [{name, indices}], "data_streams": []}`
        Some("_resolve") => &[
            Site::NamedIn("indices", "name"),
            Site::NamedIn("aliases", "name"),
            Site::NamedIn("aliases/*/indices", ""),
            Site::NamedIn("data_streams", "name"),
            Site::NamedIn("data_streams/*/backing_indices", ""),
        ],
        // `{"data_streams": [{name, indices: [{index_name}]}]}`
        Some("_data_stream") => &[
            Site::NamedIn("data_streams", "name"),
            Site::NamedIn("data_streams/*/indices", "index_name"),
        ],
        // A `_cat` table in its `format=json` form is an array of row objects.
        // Only the per-index tables have an `index` column — `_cat/templates`
        // keys its rows by TEMPLATE name, which is not an index name and must
        // not be matched against one.
        Some("_cat") if cat_index_columns(segs).is_some() => &[Site::NamedIn("", "index")],
        Some("v1") => match segs.get(1).map(String::as_str) {
            // `{"data": {"indices": [{name, …}], …}}`
            Some("dashboard") => &[Site::NamedIn("data/indices", "name")],
            _ => &[],
        },
        _ => &[],
    }
}

/// The whitespace-separated column(s) holding an index name in a `_cat`
/// table's plain-text form, by sub-verb; `None` means this table has no index
/// column, so no row of it may be dropped for containing an index name.
///
/// Column numbers are read off the handlers in `es_compat`, not guessed. The
/// alternative — "drop a row if ANY token is a hidden index name" — has the
/// same collision as key-matching did: an ordinary index named `open`, `green`
/// or `p` that a scoped key cannot read would empty `_cat/indices` and
/// `_cat/shards` completely.
fn cat_index_columns(segs: &[String]) -> Option<&'static [usize]> {
    match segs.get(1).map(String::as_str)? {
        // health status INDEX uuid pri rep docs.count docs.deleted store …
        "indices" => Some(&[2]),
        // ALIAS INDEX filter routing.index routing.search is_write_index.
        // Column 0 is an alias, not an index — but an alias name IS a name in
        // this namespace elsewhere in this module (it is authorized as a
        // resource, and it resolves to indices), so a reserved-namespace alias
        // is hidden here too.
        "aliases" => Some(&[0, 1]),
        // INDEX shard prirep state docs store ip node
        "shards" => Some(&[0]),
        // INDEX shard time type stage …
        "recovery" => Some(&[0]),
        // INDEX shard prirep ip segment …
        "segments" => Some(&[0]),
        // INDEX present field nodes tombstones …
        "ann" => Some(&[0]),
        // id host ip node INDEX size
        "fielddata" => Some(&[4]),
        // Everything else under `_cat` — nodes, health, master, plugins,
        // templates, thread_pool, nodeattrs, pending_tasks, allocation, count,
        // ml/* — reports no index name at all.
        _ => None,
    }
}

/// Apply `f` to every node the site `path` selects. `*` matches any one object
/// key or array element; an empty path selects the node itself.
fn for_each_at(v: &mut Value, path: &str, f: &mut dyn FnMut(&mut Value)) {
    if path.is_empty() {
        f(v);
        return;
    }
    let (head, rest) = path.split_once('/').unwrap_or((path, ""));
    match v {
        Value::Object(map) => {
            if head == "*" {
                for (_, child) in map.iter_mut() {
                    for_each_at(child, rest, f);
                }
            } else if let Some(child) = map.get_mut(head) {
                for_each_at(child, rest, f);
            }
        }
        Value::Array(items) if head == "*" => {
            for item in items.iter_mut() {
                for_each_at(item, rest, f);
            }
        }
        _ => {}
    }
}

/// Drop the index names this principal may not read, at the positions this
/// endpoint actually carries them — and nowhere else.
fn prune_json(v: &mut Value, sites: &[Site], principal: &Principal, known: &HashSet<String>) {
    for site in sites {
        match *site {
            Site::KeyedByIndex(path) => for_each_at(v, path, &mut |node| {
                if let Value::Object(map) = node {
                    map.retain(|k, _| !hidden(k, principal, known));
                }
            }),
            Site::NamedIn(path, field) => for_each_at(v, path, &mut |node| {
                if let Value::Array(items) = node {
                    items.retain(|item| match item {
                        Value::String(s) => !hidden(s, principal, known),
                        Value::Object(o) if !field.is_empty() => !o
                            .get(field)
                            .and_then(Value::as_str)
                            .map(|n| hidden(n, principal, known))
                            .unwrap_or(false),
                        _ => true,
                    });
                }
            }),
        }
    }
}

/// Line filter for `_cat` in its default text form: a row whose index column
/// names a hidden index is dropped whole. A table with no index column
/// (`columns` is empty) is returned untouched.
fn prune_text(
    bytes: &[u8],
    columns: &[usize],
    principal: &Principal,
    known: &HashSet<String>,
) -> Vec<u8> {
    if columns.is_empty() {
        return bytes.to_vec();
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    let mut dropped_any = false;
    for line in text.split_inclusive('\n') {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let hides = columns.iter().any(|&c| {
            fields
                .get(c)
                .map(|token| {
                    hidden(
                        token.trim_matches(|ch: char| ch == '"' || ch == ','),
                        principal,
                        known,
                    )
                })
                .unwrap_or(false)
        });
        if hides {
            dropped_any = true;
        } else {
            out.push_str(line);
        }
    }
    if dropped_any {
        out.into_bytes()
    } else {
        bytes.to_vec()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use xerj_engine::rbac::Role;

    fn scoped(indices: &[&str], privileges: &[Privilege]) -> Principal {
        Principal::Scoped {
            key_id: "k1".into(),
            roles: vec![Role::new(
                "r",
                privileges.iter().copied().collect::<HashSet<_>>(),
                indices.iter().map(|s| s.to_string()).collect(),
            )],
        }
    }

    fn unscoped() -> Principal {
        Principal::Unscoped {
            key_id: "k2".into(),
        }
    }

    struct Map(HashMap<String, Vec<String>>);
    impl AliasResolver for Map {
        fn targets(&self, name: &str) -> Option<Vec<String>> {
            self.0.get(name).cloned()
        }
    }

    /// Run the whole decision the way the middleware does.
    fn check(p: &Principal, method: Method, path: &str, body: &str) -> bool {
        check_with(p, method, path, body, &NoAliases)
    }

    fn check_with(
        p: &Principal,
        method: Method,
        path: &str,
        body: &str,
        aliases: &dyn AliasResolver,
    ) -> bool {
        let segs = segments(path);
        let target = classify(path);
        let shape = body_shape(&method, &segs);
        decide(p, &method, &segs, &target, shape, body.as_bytes(), aliases).is_ok()
    }

    #[test]
    fn reserved_prefix_recognition() {
        assert!(is_reserved_index(".xerj-memory-alice-edges"));
        assert!(is_reserved_index(".xerj-memory-alice"));
        assert!(!is_reserved_index("logs-2026"));
        assert!(!is_reserved_index(".kibana"));
    }

    #[test]
    fn wildcards_that_could_reach_the_namespace_are_recognized() {
        for pattern in ["*", "_all", ".*", ".xerj-*", ".xerj-memory-alice*", ".x*"] {
            assert!(may_reach_reserved(pattern), "{pattern} must be caught");
        }
        for pattern in ["logs-*", "notes", ".kibana*", ".xerj-other-*"] {
            assert!(!may_reach_reserved(pattern), "{pattern} must be allowed");
        }
    }

    #[test]
    fn percent_encoded_reserved_names_are_decoded() {
        // `%2E` is `.` — without decoding, the leading-dot check would miss it.
        assert_eq!(
            percent_decode("%2Exerj-memory-alice-edges"),
            ".xerj-memory-alice-edges"
        );
        assert_eq!(
            classify("/%2Exerj-memory-alice-edges/_search"),
            Target::Indices(vec![".xerj-memory-alice-edges".into()], 1)
        );
    }

    #[test]
    fn classification() {
        assert_eq!(classify("/_graph/alice/ego"), Target::Brain("alice".into()));
        assert_eq!(
            classify("/_memory/alice/_recall"),
            Target::Memory("alice".into())
        );
        assert_eq!(classify("/_search"), Target::Cluster);
        assert_eq!(classify("/_bulk"), Target::Cluster);
        assert_eq!(classify("/_mapping"), Target::Cluster);
        assert_eq!(classify("/_cat/indices"), Target::Cluster);
        // A pattern is an index expression, not a cluster endpoint.
        assert_eq!(
            classify("/_all/_search"),
            Target::Indices(vec!["_all".into()], 1)
        );
        assert_eq!(classify("/*/_search"), Target::Indices(vec!["*".into()], 1));
        assert_eq!(classify("/health/live"), Target::Exempt);
        assert_eq!(classify("/"), Target::Exempt);
        assert_eq!(
            classify("/logs-a,logs-b/_search"),
            Target::Indices(vec!["logs-a".into(), "logs-b".into()], 1)
        );
        // Native router: the index is the third segment, not the first.
        assert_eq!(
            classify("/v1/indices/.xerj-memory-bob-edges/search"),
            Target::Indices(vec![".xerj-memory-bob-edges".into()], 3)
        );
        assert_eq!(classify("/v1/dashboard/summary"), Target::Cluster);
        assert_eq!(classify("/v1/indices"), Target::Cluster);
    }

    /// Every route that can take an index name from its body must be mapped to
    /// the shape that finds it. This is the audit, executable.
    #[test]
    fn body_shape_table() {
        let cases: &[(&str, &str, BodyShape)] = &[
            ("POST", "/_bulk", BodyShape::Bulk),
            ("POST", "/logs/_bulk", BodyShape::Bulk),
            ("POST", "/_msearch", BodyShape::MsearchHeaders),
            ("POST", "/logs/_msearch", BodyShape::MsearchHeaders),
            ("POST", "/_msearch/template", BodyShape::MsearchHeaders),
            ("POST", "/logs/_msearch/template", BodyShape::MsearchHeaders),
            ("POST", "/_mget", BodyShape::MgetDocs),
            ("POST", "/logs/_mget", BodyShape::MgetDocs),
            ("POST", "/_aliases", BodyShape::AliasActions),
            ("POST", "/_reindex", BodyShape::Reindex),
            ("PUT", "/_snapshot/repo/snap", BodyShape::SnapshotIndices),
            (
                "POST",
                "/_snapshot/repo/snap/_restore",
                BodyShape::SnapshotIndices,
            ),
            ("POST", "/v1/indices", BodyShape::NativeCreate),
            ("PUT", "/logs-2026", BodyShape::CreateIndex),
            ("POST", "/_monitoring/bulk", BodyShape::MonitoringBulk),
            ("PUT", "/_index_template/t", BodyShape::IndexTemplate),
            ("PUT", "/_template/t", BodyShape::IndexTemplate),
            (
                "POST",
                "/_index_template/_simulate",
                BodyShape::IndexTemplate,
            ),
            ("PUT", "/_ml/anomaly_detectors/j", BodyShape::MlConfig),
            ("PUT", "/_ml/datafeeds/f", BodyShape::MlConfig),
            ("POST", "/_ml/datafeeds/f", BodyShape::MlConfig),
            // Sub-verbs and reads carry no index name of their own.
            ("POST", "/_ml/datafeeds/f/_start", BodyShape::None),
            ("POST", "/_ml/anomaly_detectors/j/_score", BodyShape::None),
            ("GET", "/_ml/anomaly_detectors/j", BodyShape::None),
            ("POST", "/_search", BodyShape::Query),
            ("POST", "/logs/_search", BodyShape::Query),
            ("POST", "/_count", BodyShape::Query),
            ("POST", "/logs/_delete_by_query", BodyShape::Query),
            ("POST", "/logs/_update_by_query", BodyShape::Query),
            ("POST", "/_sql", BodyShape::Query),
            ("POST", "/logs/_eql/search", BodyShape::Query),
            // Opaque document bodies: nothing in them is an index name.
            ("PUT", "/logs/_doc/1", BodyShape::None),
            ("POST", "/logs/_update/1", BodyShape::None),
            ("POST", "/v1/indices/logs/docs", BodyShape::None),
            ("POST", "/v1/indices/logs/docs/_bulk", BodyShape::None),
        ];
        for (method, path, want) in cases {
            let m: Method = method.parse().unwrap();
            let got = body_shape(&m, &segments(path));
            assert_eq!(got, *want, "{method} {path}");
        }
    }

    #[test]
    fn privilege_mapping() {
        let s = |p: &str| segments(p);
        assert_eq!(
            required_privilege(&Method::POST, &s("/idx/_search"), 1),
            Privilege::ReadIndex
        );
        assert_eq!(
            required_privilege(&Method::POST, &s("/idx/_doc/1"), 1),
            Privilege::WriteIndex
        );
        assert_eq!(
            required_privilege(&Method::DELETE, &s("/idx"), 1),
            Privilege::AdminIndex
        );
        assert_eq!(
            required_privilege(&Method::PUT, &s("/idx/_settings"), 1),
            Privilege::AdminIndex
        );
        // Native spellings resolve to the same privileges.
        assert_eq!(
            required_privilege(&Method::POST, &s("/v1/indices/idx/search"), 3),
            Privilege::ReadIndex
        );
        assert_eq!(
            required_privilege(&Method::POST, &s("/v1/indices/idx/docs"), 3),
            Privilege::WriteIndex
        );
        assert_eq!(
            required_privilege(&Method::DELETE, &s("/v1/indices/idx"), 3),
            Privilege::AdminIndex
        );
        assert_eq!(
            reserved_api_privilege(&Method::POST, &s("/_graph/a/link")),
            Privilege::WriteIndex
        );
        assert_eq!(
            reserved_api_privilege(&Method::GET, &s("/_graph/a/ego")),
            Privilege::ReadIndex
        );
        assert_eq!(
            reserved_api_privilege(&Method::DELETE, &s("/_memory/a")),
            Privilege::AdminIndex
        );
        assert_eq!(
            reserved_api_privilege(&Method::DELETE, &s("/_memory/a/doc1")),
            Privilege::WriteIndex
        );
    }

    /// The decision table, stated once. `decide` is the only thing between an
    /// authenticated caller and someone else's brain.
    #[test]
    fn decision_table() {
        let alice = scoped(
            &[".xerj-memory-alice-edges", ".xerj-memory-alice"],
            &[Privilege::ReadIndex, Privilege::WriteIndex],
        );

        // Alice reaches her own brain …
        assert!(check(&alice, Method::GET, "/_graph/alice/ego", ""));
        assert!(check(&alice, Method::GET, "/_graph/alice/overview", ""));
        assert!(check(&alice, Method::POST, "/_graph/alice/link", ""));
        assert!(check(&alice, Method::POST, "/_memory/alice/_recall", ""));
        // … and nothing of bob's, by any door.
        assert!(!check(&alice, Method::GET, "/_graph/bob/ego", ""));
        assert!(!check(&alice, Method::POST, "/_graph/bob/link", ""));
        assert!(!check(&alice, Method::DELETE, "/_graph/bob/link/e1", ""));
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-bob-edges/_search",
            ""
        ));
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-bob-edges/_doc/x",
            ""
        ));
        assert!(!check(
            &alice,
            Method::DELETE,
            "/.xerj-memory-bob-edges",
            ""
        ));
        assert!(!check(&alice, Method::POST, "/_memory/bob/_recall", ""));
        // Including the native router's spelling of the same index.
        assert!(!check(
            &alice,
            Method::POST,
            "/v1/indices/.xerj-memory-bob-edges/search",
            ""
        ));
        // A grant is a grant: alice may use her own backing index directly.
        assert!(check(
            &alice,
            Method::POST,
            "/.xerj-memory-alice-edges/_search",
            ""
        ));

        // A read-only grant does not write.
        let ro = scoped(&[".xerj-memory-alice-edges"], &[Privilege::ReadIndex]);
        assert!(check(&ro, Method::GET, "/_graph/alice/ego", ""));
        assert!(!check(&ro, Method::POST, "/_graph/alice/link", ""));
        assert!(!check(&ro, Method::DELETE, "/.xerj-memory-alice-edges", ""));

        // A legacy key with no grants: keeps ordinary indices, loses every
        // brain door. This is the fail-closed half.
        let legacy = unscoped();
        assert!(check(&legacy, Method::POST, "/logs-2026/_search", ""));
        assert!(check(&legacy, Method::GET, "/logs-*/_search", ""));
        assert!(check(&legacy, Method::GET, "/_mapping", ""));
        assert!(!check(&legacy, Method::GET, "/_graph/alice/ego", ""));
        assert!(!check(&legacy, Method::POST, "/_memory/alice/_recall", ""));
        assert!(!check(
            &legacy,
            Method::POST,
            "/.xerj-memory-alice-edges/_search",
            ""
        ));
        assert!(check(&legacy, Method::PUT, "/logs-2026", ""));

        // No credential resolves to nothing at all.
        let denied = Principal::Denied;
        assert!(!check(&denied, Method::GET, "/_graph/alice/ego", ""));
        assert!(!check(&denied, Method::GET, "/logs/_search", ""));
        assert!(!check(&denied, Method::GET, "/_mapping", ""));

        // The superuser is untouched — this is the `--insecure` path.
        let root = Principal::Superuser;
        for path in [
            "/_graph/bob/ego",
            "/_mapping",
            "/_search",
            "/.xerj-memory-bob-edges/_search",
            "/*/_search",
        ] {
            assert!(
                check(&root, Method::GET, path, ""),
                "superuser blocked on {path}"
            );
        }
    }

    /// BYPASS 1-3: the index in the body is the one that gets authorized.
    #[test]
    fn body_named_indices_are_authorized_not_the_path_one() {
        let alice = scoped(
            &[".xerj-memory-alice-edges", ".xerj-memory-alice"],
            &[Privilege::ReadIndex, Privilege::WriteIndex],
        );

        // _msearch: the header line overrides the path index.
        let msearch = "{\"index\":\".xerj-memory-bob-edges\"}\n{\"query\":{\"match_all\":{}}}\n";
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-alice-edges/_msearch",
            msearch
        ));
        assert!(!check(&alice, Method::POST, "/_msearch", msearch));
        // Her own brain through the same door still works.
        let mine = "{\"index\":\".xerj-memory-alice-edges\"}\n{\"query\":{\"match_all\":{}}}\n";
        assert!(check(&alice, Method::POST, "/_msearch", mine));

        // _bulk: the action line's `_index` overrides the path index.
        let forge =
            "{\"index\":{\"_index\":\".xerj-memory-bob-edges\",\"_id\":\"f\"}}\n{\"src\":\"x\"}\n";
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-alice-edges/_bulk",
            forge
        ));
        assert!(!check(&alice, Method::POST, "/_bulk", forge));
        let destroy = "{\"delete\":{\"_index\":\".xerj-memory-bob-edges\",\"_id\":\"e1\"}}\n";
        assert!(!check(&alice, Method::POST, "/_bulk", destroy));
        // A bulk that stays inside her grant is allowed — the global `_bulk`
        // is the default write path for every ES client and must work.
        let ok = "{\"index\":{\"_index\":\".xerj-memory-alice-edges\",\"_id\":\"a\"}}\n{\"src\":\"x\"}\n";
        assert!(check(&alice, Method::POST, "/_bulk", ok));

        // _mget: docs[]._index overrides the path index.
        let mget = "{\"docs\":[{\"_index\":\".xerj-memory-bob-edges\",\"_id\":\"1\"}]}";
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-alice-edges/_mget",
            mget
        ));
        assert!(!check(&alice, Method::POST, "/_mget", mget));
        assert!(check(
            &alice,
            Method::POST,
            "/.xerj-memory-alice-edges/_mget",
            "{\"ids\":[\"1\"]}"
        ));

        // An unscoped legacy key holds nothing in the reserved namespace by
        // any of the three doors either.
        let legacy = unscoped();
        assert!(!check(&legacy, Method::POST, "/_msearch", msearch));
        assert!(!check(&legacy, Method::POST, "/_bulk", forge));
        assert!(!check(&legacy, Method::POST, "/_mget", mget));
        // …but its ordinary-index traffic is untouched.
        let plain = "{\"index\":{\"_index\":\"logs-2026\"}}\n{\"m\":1}\n";
        assert!(check(&legacy, Method::POST, "/_bulk", plain));
    }

    /// BYPASS 4: `POST /_aliases` authorizes the indices it names, and an
    /// alias already pointing into the namespace does not launder reads.
    #[test]
    fn aliases_cannot_launder_access() {
        let legacy = unscoped();
        let add =
            "{\"actions\":[{\"add\":{\"index\":\".xerj-memory-bob-edges\",\"alias\":\"pwned\"}}]}";
        assert!(!check(&legacy, Method::POST, "/_aliases", add));
        // Also refused for a scoped key that holds a different brain.
        let alice = scoped(
            &[".xerj-memory-alice-edges"],
            &[
                Privilege::ReadIndex,
                Privilege::WriteIndex,
                Privilege::AdminIndex,
            ],
        );
        assert!(!check(&alice, Method::POST, "/_aliases", add));
        // An ordinary alias over an ordinary index still works for the legacy
        // key that has the general surface.
        let ordinary = "{\"actions\":[{\"add\":{\"index\":\"logs-2026\",\"alias\":\"logs\"}}]}";
        assert!(check(&legacy, Method::POST, "/_aliases", ordinary));
        // Squatting an alias NAME inside the reserved namespace is refused.
        let squat =
            "{\"actions\":[{\"add\":{\"index\":\"logs-2026\",\"alias\":\".xerj-memory-victim\"}}]}";
        assert!(!check(&legacy, Method::POST, "/_aliases", squat));
        // …by the path spelling too.
        assert!(!check(
            &legacy,
            Method::PUT,
            "/logs-2026/_alias/.xerj-memory-victim",
            ""
        ));

        // Reading THROUGH an alias that already points at a brain resolves to
        // the brain before the decision is made.
        let mut map = HashMap::new();
        map.insert(
            "pwned".to_string(),
            vec![".xerj-memory-bob-edges".to_string()],
        );
        let aliases = Map(map);
        assert!(!check_with(
            &legacy,
            Method::POST,
            "/pwned/_search",
            "",
            &aliases
        ));
        assert!(!check_with(
            &legacy,
            Method::POST,
            "/_mget",
            "{\"docs\":[{\"_index\":\"pwned\",\"_id\":\"1\"}]}",
            &aliases
        ));
    }

    /// The fifth shape, found by audit rather than by attack: a `terms` lookup
    /// reads a document out of an index the URL never names.
    #[test]
    fn terms_lookup_index_is_authorized() {
        let alice = scoped(&["logs-2026"], &[Privilege::ReadIndex]);
        let lookup = r#"{"query":{"bool":{"filter":[{"terms":{"user":{"index":".xerj-memory-bob-edges","id":"1","path":"dst"}}}]}}}"#;
        assert!(!check(&alice, Method::POST, "/logs-2026/_search", lookup));
        // A lookup into an index she holds is fine.
        let own = r#"{"query":{"terms":{"user":{"index":"logs-2026","id":"1","path":"u"}}}}"#;
        assert!(check(&alice, Method::POST, "/logs-2026/_search", own));
        // A plain `terms` filter (no lookup) names no index and is untouched —
        // a document field called `index` must not be read as one.
        let plain = r#"{"query":{"terms":{"index":["a","b"]}}}"#;
        assert!(check(&alice, Method::POST, "/logs-2026/_search", plain));

        // The same shape one layer over: a `lookup` runtime field joins rows
        // out of another index into the hits.
        let runtime = r#"{"runtime_mappings":{"u":{"type":"lookup","target_index":".xerj-memory-bob-edges","input_field":"src","target_field":"_id","fetch_fields":["dst"]}}}"#;
        assert!(!check(&alice, Method::POST, "/logs-2026/_search", runtime));
        let own = r#"{"runtime_mappings":{"u":{"type":"lookup","target_index":"logs-2026","input_field":"src","target_field":"_id"}}}"#;
        assert!(check(&alice, Method::POST, "/logs-2026/_search", own));
    }

    /// `_reindex` reads its source and writes its destination.
    #[test]
    fn reindex_authorizes_source_and_destination() {
        let p = scoped(
            &["logs-2026", "logs-copy"],
            &[Privilege::ReadIndex, Privilege::WriteIndex],
        );
        let ok = r#"{"source":{"index":"logs-2026"},"dest":{"index":"logs-copy"}}"#;
        assert!(check(&p, Method::POST, "/_reindex", ok));
        let steal = r#"{"source":{"index":".xerj-memory-bob-edges"},"dest":{"index":"logs-copy"}}"#;
        assert!(!check(&p, Method::POST, "/_reindex", steal));
        let overwrite =
            r#"{"source":{"index":"logs-2026"},"dest":{"index":".xerj-memory-bob-edges"}}"#;
        assert!(!check(&p, Method::POST, "/_reindex", overwrite));
        // A body that names neither is refused rather than guessed at.
        assert!(!check(&p, Method::POST, "/_reindex", "{}"));
        assert!(!check(&p, Method::POST, "/_reindex", "not json"));
    }

    /// The native router's body-named create is authorized, not blanket-refused.
    #[test]
    fn native_body_named_create_is_authorized() {
        let legacy = unscoped();
        assert!(check(
            &legacy,
            Method::POST,
            "/v1/indices",
            r#"{"name":"logs-2026","fields":[]}"#
        ));
        assert!(!check(
            &legacy,
            Method::POST,
            "/v1/indices",
            r#"{"name":".xerj-memory-victim-edges","fields":[]}"#
        ));
        assert!(!check(&legacy, Method::POST, "/v1/indices", "{}"));
    }

    /// A snapshot or restore that names no indices covers every brain on the
    /// node, so it is superuser-only.
    #[test]
    fn unbounded_snapshot_is_superuser_only() {
        let legacy = unscoped();
        assert!(!check(&legacy, Method::PUT, "/_snapshot/repo/s1", "{}"));
        assert!(!check(
            &legacy,
            Method::POST,
            "/_snapshot/repo/s1/_restore",
            "{}"
        ));
        assert!(check(
            &legacy,
            Method::PUT,
            "/_snapshot/repo/s1",
            r#"{"indices":["logs-2026"]}"#
        ));
        assert!(!check(
            &legacy,
            Method::PUT,
            "/_snapshot/repo/s1",
            r#"{"indices":[".xerj-memory-bob-edges"]}"#
        ));
        assert!(check(
            &Principal::Superuser,
            Method::PUT,
            "/_snapshot/repo/s1",
            "{}"
        ));
    }

    /// COMPATIBILITY: the regression that made the branch unusable. Every
    /// global verb works for an ordinary credential, and a scoped key can read
    /// cluster metadata and use a wildcard it was granted.
    #[test]
    fn ordinary_credentials_keep_the_global_surface() {
        let legacy = unscoped();
        let scoped_all = scoped(
            &["*"],
            &[
                Privilege::ReadIndex,
                Privilege::WriteIndex,
                Privilege::AdminIndex,
            ],
        );
        let logs = scoped(&["logs-*"], &[Privilege::ReadIndex, Privilege::WriteIndex]);

        for p in [&legacy, &scoped_all, &logs] {
            assert!(check(p, Method::POST, "/_search", "{}"), "global _search");
            assert!(check(p, Method::POST, "/_count", "{}"), "global _count");
            assert!(
                check(p, Method::POST, "/_msearch", "{}\n{\"query\":{}}\n"),
                "global _msearch"
            );
            assert!(
                check(
                    p,
                    Method::POST,
                    "/_bulk",
                    "{\"index\":{\"_index\":\"logs-2026\"}}\n{\"m\":1}\n"
                ),
                "global _bulk"
            );
            assert!(
                check(
                    p,
                    Method::POST,
                    "/_mget",
                    "{\"docs\":[{\"_index\":\"logs-2026\",\"_id\":\"1\"}]}"
                ),
                "global _mget"
            );
            assert!(check(p, Method::GET, "/_cluster/health", ""), "health");
            assert!(check(p, Method::GET, "/_nodes", ""), "nodes");
            assert!(check(p, Method::GET, "/_mapping", ""), "mapping");
            assert!(check(p, Method::GET, "/_cat/indices", ""), "cat");
            assert!(check(p, Method::GET, "/v1/health", ""), "native health");
            assert!(
                check(p, Method::GET, "/_resolve/index/logs-*", ""),
                "resolve"
            );
        }
        // A granted wildcard is usable, and an ungranted one resolves to
        // nothing rather than 403 (the engine expands it over the visible set).
        assert!(check(&logs, Method::GET, "/logs-*/_search", ""));
        assert!(check(&scoped_all, Method::GET, "/logs-*/_search", ""));
        assert!(check(&logs, Method::GET, "/.xerj-memory-*/_search", ""));
        // A scoped key still cannot NAME another tenant's index.
        assert!(!check(
            &logs,
            Method::GET,
            "/.xerj-memory-bob-edges/_search",
            ""
        ));
        // …nor mutate the cluster itself.
        assert!(!check(&logs, Method::PUT, "/_cluster/settings", "{}"));
        assert!(check(&legacy, Method::PUT, "/_cluster/settings", "{}"));
    }

    /// Prune the way the middleware does: pick the endpoint's sites from its
    /// path, then apply them.
    fn prune_at(path: &str, v: &mut Value, p: &Principal, known: &HashSet<String>) {
        prune_json(v, prune_sites(&segments(path)), p, known);
    }

    fn prune_rows(path: &str, table: &str, p: &Principal, known: &HashSet<String>) -> String {
        let cols = cat_index_columns(&segments(path)).unwrap_or(&[]);
        String::from_utf8(prune_text(table.as_bytes(), cols, p, known)).expect("utf8")
    }

    #[test]
    fn json_pruning_hides_unreadable_indices() {
        let p = scoped(&[".xerj-memory-alice-edges"], &[Privilege::ReadIndex]);
        let known: HashSet<String> = [
            ".xerj-memory-alice-edges",
            ".xerj-memory-bob-edges",
            "logs-2026",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut mapping = serde_json::json!({
            ".xerj-memory-alice-edges": {"mappings": {}},
            ".xerj-memory-bob-edges": {"mappings": {}},
            "logs-2026": {"mappings": {}}
        });
        prune_at("/_mapping", &mut mapping, &p, &known);
        assert!(mapping.get(".xerj-memory-alice-edges").is_some());
        assert!(mapping.get(".xerj-memory-bob-edges").is_none());
        // A scoped key does not see another tenant's ordinary index either …
        assert!(mapping.get("logs-2026").is_none());
        // … but structural keys are never mistaken for index names.
        assert!(mapping[".xerj-memory-alice-edges"]
            .get("mappings")
            .is_some());

        // Array shapes: `_cat/indices?format=json` and `_resolve/index`.
        let mut cat = serde_json::json!([
            {"index": ".xerj-memory-bob-edges", "docs.count": "1"},
            {"index": ".xerj-memory-alice-edges", "docs.count": "9"}
        ]);
        prune_at("/_cat/indices", &mut cat, &p, &known);
        assert_eq!(cat.as_array().unwrap().len(), 1);

        let mut caps =
            serde_json::json!({"indices": [".xerj-memory-bob-edges", ".xerj-memory-alice-edges"]});
        prune_at("/_field_caps", &mut caps, &p, &known);
        assert_eq!(
            caps["indices"],
            serde_json::json!([".xerj-memory-alice-edges"])
        );
        // …including the per-field `indices` lists buried under `fields`.
        let mut caps = serde_json::json!({
            "indices": [".xerj-memory-alice-edges", ".xerj-memory-bob-edges"],
            "fields": {"host": {"keyword": {
                "type": "keyword",
                "indices": [".xerj-memory-alice-edges", ".xerj-memory-bob-edges"]
            }}}
        });
        prune_at("/_field_caps", &mut caps, &p, &known);
        assert_eq!(
            caps["fields"]["host"]["keyword"]["indices"],
            serde_json::json!([".xerj-memory-alice-edges"])
        );
        assert_eq!(caps["fields"]["host"]["keyword"]["type"], "keyword");
    }

    /// FINDING B. An index name is an arbitrary string, so the previous
    /// "delete any key equal to a known index name, at every depth" collided
    /// with the structural keys of the responses it was protecting — and only
    /// ever for a scoped key, i.e. exactly the multi-tenant case this branch
    /// exists to enable. Each case below is one measured breakage.
    #[test]
    fn structural_keys_survive_ordinary_indices_that_share_their_name() {
        // Ordinary indices with unfortunate names, none of them readable by
        // this principal.
        let p = scoped(&["logs-2026"], &[Privilege::ReadIndex]);
        let known: HashSet<String> = ["status", "indices", "type", "nodes", "count", "logs-2026"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // `GET /_cluster/health` kept its `status` — the most-polled field in
        // the API — while still pruning the per-index breakdown.
        let mut health = serde_json::json!({
            "cluster_name": "xerj", "status": "green", "timed_out": false,
            "indices": {"logs-2026": {"status": "green"}, "status": {"status": "green"}}
        });
        prune_at("/_cluster/health", &mut health, &p, &known);
        assert_eq!(health["status"], "green");
        assert!(health["indices"].get("logs-2026").is_some());
        assert!(health["indices"].get("status").is_none());

        // `GET /_cluster/stats` kept its whole `indices` section: those are
        // aggregate counters, not a map keyed by index name.
        let mut stats = serde_json::json!({
            "status": "green",
            "indices": {"count": 5, "docs": {"count": 42}},
            "nodes": {"count": {"total": 1}}
        });
        let before = stats.clone();
        prune_at("/_cluster/stats", &mut stats, &p, &known);
        assert_eq!(stats, before, "_cluster/stats names no index anywhere");

        // Global `GET /_mapping` kept every field's `type`, which is what
        // Kibana reads to build an index pattern.
        let mut mapping = serde_json::json!({
            "logs-2026": {"mappings": {"properties": {
                "host": {"type": "keyword"},
                "count": {"type": "long"}
            }}},
            "type": {"mappings": {"properties": {}}}
        });
        prune_at("/_mapping", &mut mapping, &p, &known);
        assert_eq!(
            mapping["logs-2026"]["mappings"]["properties"]["host"]["type"],
            "keyword"
        );
        assert_eq!(
            mapping["logs-2026"]["mappings"]["properties"]["count"]["type"],
            "long"
        );
        assert!(
            mapping.get("type").is_none(),
            "the index actually named `type` is still hidden"
        );

        // `GET /_cluster/state` prunes its two index maps and leaves the node
        // map alone, even when an index shares a node's name.
        let mut state = serde_json::json!({
            "nodes": {"nodes": {"name": "nodes"}},
            "metadata": {"templates": {}, "indices": {"logs-2026": {}, "count": {}}},
            "routing_table": {"indices": {"logs-2026": {}, "count": {}}}
        });
        prune_at("/_cluster/state", &mut state, &p, &known);
        assert!(state["nodes"].get("nodes").is_some(), "node map untouched");
        assert!(state["metadata"].get("templates").is_some());
        assert!(state["metadata"]["indices"].get("logs-2026").is_some());
        assert!(state["metadata"]["indices"].get("count").is_none());
        assert!(state["routing_table"]["indices"].get("count").is_none());
    }

    /// The same collision in the `_cat` text tables: matching *any* token
    /// meant one unreadable index called `open` or `green` emptied the table.
    #[test]
    fn cat_rows_are_matched_on_the_index_column_only() {
        let p = scoped(&["logs-2026"], &[Privilege::ReadIndex]);
        let known: HashSet<String> = ["logs-2026", "open", "green", ".xerj-memory-bob-edges"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let table = "green open logs-2026 uuid 1 0 9 0 1kb 1kb\n\
                     green open .xerj-memory-bob-edges uuid 1 0 1 0 1kb 1kb\n\
                     green open open uuid 1 0 3 0 1kb 1kb\n";
        let out = prune_rows("/_cat/indices", table, &p, &known);
        assert!(out.contains("logs-2026"), "own index survives: {out}");
        assert!(
            !out.contains(".xerj-memory-bob-edges"),
            "brain hidden: {out}"
        );
        assert_eq!(
            out.lines().count(),
            1,
            "only the row whose INDEX column is hidden goes: {out}"
        );

        // `_cat/health` has no index column at all, so nothing may be dropped
        // from it however its values happen to read.
        let health = "1785000000 12:00:00 xerj green 1 1 4 4 0 0 0 0 - 100.0%\n";
        assert_eq!(prune_rows("/_cat/health", health, &p, &known), health);

        // `_cat/fielddata` carries the index in column 4.
        let fd = "n1 127.0.0.1 127.0.0.1 n1 logs-2026 0b\n\
                  n1 127.0.0.1 127.0.0.1 n1 .xerj-memory-bob-edges 0b\n";
        let out = prune_rows("/_cat/fielddata", fd, &p, &known);
        assert!(out.contains("logs-2026"));
        assert!(!out.contains(".xerj-memory-bob-edges"));
    }

    /// A `_cat/templates` row is keyed by TEMPLATE name. It must not be
    /// matched against index names — a template and an index may share one.
    #[test]
    fn cat_templates_rows_are_never_index_matched() {
        let p = scoped(&["logs-2026"], &[Privilege::ReadIndex]);
        let known: HashSet<String> = ["count"].iter().map(|s| s.to_string()).collect();
        assert!(cat_index_columns(&segments("/_cat/templates")).is_none());
        assert!(prune_sites(&segments("/_cat/templates")).is_empty());
        let rows = "count logs-* 100 -\n";
        assert_eq!(prune_rows("/_cat/templates", rows, &p, &known), rows);
    }

    /// FINDING C. Pinned, not fixed: `names: ["*"]` matches everything,
    /// including `.xerj-memory-*`. Only a superuser can mint a scoped key, so
    /// this is an operator's explicit "the whole node" rather than an
    /// escalation — but it must not change without someone meaning it.
    /// See `xerj_engine::rbac::Role::applies_to`.
    #[test]
    fn a_star_grant_reaches_the_reserved_namespace() {
        let star = scoped(&["*"], &[Privilege::ReadIndex, Privilege::WriteIndex]);
        assert!(star.allows_index(".xerj-memory-bob-edges", Privilege::ReadIndex));
        assert!(check(
            &star,
            Method::GET,
            "/.xerj-memory-bob-edges/_search",
            ""
        ));
        assert!(check(&star, Method::GET, "/_memory/bob", ""));
        // A prefix grant that stops short of the reserved namespace does not.
        let logs = scoped(&["logs-*"], &[Privilege::ReadIndex]);
        assert!(!logs.allows_index(".xerj-memory-bob-edges", Privilege::ReadIndex));
        assert!(!check(
            &logs,
            Method::GET,
            "/.xerj-memory-bob-edges/_search",
            ""
        ));
        // …and `*` is still not a licence to squat the namespace with a
        // template, which decides the mapping of brains created later.
        assert!(!check(
            &star,
            Method::PUT,
            "/_index_template/t",
            r#"{"index_patterns":[".xerj-memory-*"]}"#
        ));
    }

    /// FINDING A, config half. The ML config endpoints name an index the
    /// server will then go and read on a schedule; before this they were
    /// accepted with a 200 for a principal holding nothing on it.
    #[test]
    fn ml_config_authorizes_the_index_it_will_read() {
        let legacy = unscoped();
        let alice = scoped(&[".xerj-memory-alice-edges"], &[Privilege::ReadIndex]);

        for (path, body) in [
            (
                "/_ml/anomaly_detectors/leak2",
                r#"{"source_index":".xerj-memory-bob-edges","time_field":"created_at"}"#,
            ),
            (
                "/_ml/anomaly_detectors/leak2",
                r#"{"index":".xerj-memory-bob-edges"}"#,
            ),
            (
                "/_ml/anomaly_detectors/leak2",
                r#"{"source_index":"logs-2026","datafeed_config":{"indices":[".xerj-memory-bob-edges"]}}"#,
            ),
            (
                "/_ml/datafeeds/leak2-feed",
                r#"{"job_id":"leak2","indices":[".xerj-memory-bob-edges"]}"#,
            ),
            (
                "/_ml/datafeeds/leak2-feed",
                r#"{"job_id":"leak2","indexes":".xerj-memory-bob-edges"}"#,
            ),
        ] {
            assert!(
                !check(&legacy, Method::PUT, path, body),
                "unscoped key configured ML over a brain: PUT {path} {body}"
            );
            assert!(
                !check(&alice, Method::PUT, path, body),
                "alice configured ML over bob's brain: PUT {path} {body}"
            );
        }

        // Its own brain is fine, and so is an ordinary index for a key that
        // holds the general surface.
        assert!(check(
            &alice,
            Method::PUT,
            "/_ml/anomaly_detectors/mine",
            r#"{"source_index":".xerj-memory-alice-edges","time_field":"t"}"#
        ));
        assert!(check(
            &legacy,
            Method::PUT,
            "/_ml/anomaly_detectors/ok",
            r#"{"source_index":"logs-2026","time_field":"t"}"#
        ));
        // A datafeed that names no index inherits the (already authorized)
        // detector's source, so a scoped key is not refused for it.
        assert!(check(
            &alice,
            Method::PUT,
            "/_ml/datafeeds/mine-feed",
            r#"{"job_id":"mine"}"#
        ));
        // The sub-verbs carry no index of their own, so they are decided by
        // the pre-existing cluster rule and this change does not move them: a
        // key that holds the general surface may start a datafeed, a scoped
        // one may not (a cluster-level mutation naming nothing). Pinned so the
        // MlConfig arm cannot be read as having widened either one.
        assert_eq!(
            body_shape(&Method::POST, &segments("/_ml/datafeeds/f/_start")),
            BodyShape::None
        );
        assert!(check(
            &legacy,
            Method::POST,
            "/_ml/datafeeds/f/_start",
            "{}"
        ));
        assert!(!check(
            &alice,
            Method::POST,
            "/_ml/datafeeds/f/_start",
            "{}"
        ));
    }

    /// A template's patterns decide the mapping of indices that do not exist
    /// yet, so a pattern reaching the reserved namespace is a write to a brain
    /// by another route.
    #[test]
    fn index_template_patterns_cannot_reach_the_namespace() {
        let legacy = unscoped();
        assert!(check(
            &legacy,
            Method::PUT,
            "/_index_template/logs",
            r#"{"index_patterns":["logs-*"]}"#
        ));
        for patterns in [
            r#"{"index_patterns":[".xerj-memory-*"]}"#,
            r#"{"index_patterns":["*"]}"#,
            r#"{"index_patterns":[".xerj-memory-bob-edges"]}"#,
            r#"{"index_patterns":"logs-*,.xerj-*"}"#,
        ] {
            assert!(
                !check(&legacy, Method::PUT, "/_index_template/grab", patterns),
                "{patterns} must be refused"
            );
            assert!(
                !check(&legacy, Method::PUT, "/_template/grab", patterns),
                "legacy template {patterns} must be refused"
            );
        }
        // The superuser still registers whatever it likes.
        assert!(check(
            &Principal::Superuser,
            Method::PUT,
            "/_index_template/grab",
            r#"{"index_patterns":["*"]}"#
        ));
    }

    /// The engine-side backstop is derived from the same principal, so a name
    /// the middleware never parsed is still refused where it is resolved.
    #[test]
    fn visibility_matches_the_principal() {
        let alice = scoped(&[".xerj-memory-alice-edges"], &[Privilege::ReadIndex]);
        let v = visibility_for(&alice);
        assert!(v.visible(".xerj-memory-alice-edges"));
        assert!(!v.visible(".xerj-memory-bob-edges"));
        assert!(!v.visible("logs-2026"));

        let legacy = visibility_for(&unscoped());
        assert!(legacy.visible("logs-2026"));
        assert!(!legacy.visible(".xerj-memory-bob-edges"));

        let root = visibility_for(&Principal::Superuser);
        assert!(root.visible(".xerj-memory-bob-edges"));

        let denied = visibility_for(&Principal::Denied);
        assert!(!denied.visible("logs-2026"));
    }
}
