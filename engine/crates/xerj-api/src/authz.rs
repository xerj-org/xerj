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
//! ## Why the boundary is not enforced in `graph_api` alone
//!
//! A brain's edges live in an ordinary index, and `IndexName::validate`
//! deliberately admits a leading `.`, so `.xerj-memory-{brain}-edges` is
//! reachable through the generic ES-compat surface. An access check bolted
//! onto the four `/_graph/*` handlers would leave `POST
//! /.xerj-memory-{brain}-edges/_search` (read), `POST
//! /.xerj-memory-{brain}-edges/_doc/{id}` (forge an edge around the derived-
//! `edge_id` invariant of SECOND_BRAIN_SPEC §2.3), `DELETE
//! /.xerj-memory-{brain}-edges` (destroy the brain) and `GET /_mapping`
//! (enumerate every brain name) wide open — a boundary that only *looks* like
//! one, which is worse than a documented absence. So enforcement lives here,
//! as a middleware over the whole ES-compat router, **plus** in-handler checks
//! in `graph_api`/`memory_api` so those handlers are safe even if mounted on a
//! router that forgot the middleware.
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
//! | any index pattern that could *match* a reserved index | middleware (patterns are rejected, not expanded) |
//! | unnamed fan-out (`POST /_search`, `/_bulk`, `/_all/*`, …) | middleware (refused for non-superusers) |
//! | enumeration (`GET /_mapping`, `/_cat/indices`, `/_resolve/index/*`, …) | middleware (reserved entries pruned from the response) |
//! | the native router's `/v1/indices/{name}/…` spelling of the same index | middleware ([`Target::Indices`] classifies both routers) |
//! | the gRPC listener (`:8081` — `Search`/`Index`/`BulkIndex`/`Get`/`Delete` take the index from the message body) | `xerj-server::grpc`, using [`Principal::allows_index`] |
//! | privilege escalation via `POST /_security/api_key` | `es_compat::security_create_api_key` (a non-superuser cannot mint grants it does not hold) |
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
//! - a wildcard that *could* match the reserved namespace → refused, never
//!   expanded-and-filtered;
//! - a request whose target this module cannot resolve → refused for a scoped
//!   principal.
//!
//! ## What this does **not** claim
//!
//! Broad RBAC over the general ES-compat surface is still deferred: an
//! `Unscoped` key keeps its historical superuser-equivalent reach over
//! *ordinary* indices. This module makes the reserved namespace a real
//! boundary; it does not turn xerj into a general multi-tenant authorization
//! system. `xerj_engine::rbac`'s named `RoleStore` remains unenforced data.

// The decision functions return `Result<(), Response>` where `Err` IS the
// ready-to-send 403. That trips `clippy::result_large_err` (an `axum::Response`
// is a fat value), but boxing it would add an allocation on the deny path to
// satisfy a lint aimed at hot `Ok` paths, and would obscure that the error
// *is* the response.
#![allow(clippy::result_large_err)]

use axum::{
    body::Body,
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
/// `logs-*` does not. Patterns are refused rather than expanded-and-filtered,
/// so a caller can never learn what a wildcard *would* have matched.
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
    /// A cluster/global endpoint that carries no index in its path and reads
    /// or writes only metadata (`/_cat/*`, `/_mapping`, `/_nodes`, …). Safe
    /// for any authenticated principal; enumerating responses are pruned.
    GlobalMetadata,
    /// A cluster/global endpoint that reads or writes **document data** across
    /// indices the path never names (`/_search`, `/_bulk`, `/_msearch`,
    /// `/_all/*`, …). There is no target to check, so it is refused for every
    /// non-superuser rather than allowed and hoped about.
    GlobalFanout,
    /// Not authorization-relevant (health probes, the version banner).
    Exempt,
}

/// Global endpoints that read or write document data across unnamed indices.
/// A bare `POST /_search` fans out over *every* index, including reserved
/// ones, and `_bulk`/`_msearch`/`_mget` take their index names from a body
/// this middleware deliberately does not parse. None of them can be
/// constrained without naming a target, so all of them are refused for
/// non-superusers. Index-scoped forms (`/{index}/_search`, `/{index}/_bulk`,
/// …) are unaffected — a caller keeps every route it can name.
const GLOBAL_FANOUT: &[&str] = &[
    "_search",
    "_count",
    "_msearch",
    "_mget",
    "_bulk",
    "_reindex",
    "_delete_by_query",
    "_update_by_query",
    "_explain",
    "_validate",
    "_knn_search",
    "_pit",
    "_async_search",
    "_sql",
    "_eql",
    "_esql",
    "_search_shards",
    "_rank_eval",
    "_termvectors",
    "_mtermvectors",
    "_all",
    "*",
];

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

/// Endpoints whose responses enumerate index names. For a non-superuser these
/// are answered normally and then pruned of reserved entries, which is what
/// keeps brain *names* unguessable without breaking Kibana's metadata polling.
const ENUMERATING_ROOTS: &[&str] = &[
    "_mapping",
    "_mappings",
    "_settings",
    "_alias",
    "_aliases",
    "_stats",
    "_cat",
    "_resolve",
    "_cluster",
    "_field_caps",
    "_segments",
    "_recovery",
    "_shard_stores",
    "_data_stream",
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
            // `POST /v1/indices` takes the index name from the body, which this
            // middleware deliberately does not parse — so it is the native
            // router's `_bulk`: no target to authorize, therefore refused for
            // non-superusers. ES-compat `PUT /{index}` names it in the path and
            // still works.
            (Some("indices"), None) => Target::GlobalFanout,
            _ => Target::GlobalMetadata,
        },
        s if s.starts_with('_') || s == "*" => {
            if GLOBAL_FANOUT.contains(&s) {
                Target::GlobalFanout
            } else {
                Target::GlobalMetadata
            }
        }
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
// Middleware
// ─────────────────────────────────────────────────────────────────────────────

/// Largest response this middleware will buffer in order to prune reserved
/// index names out of it. Metadata listings are kilobytes; the cap only exists
/// so a pathological response cannot be turned into an OOM.
const MAX_PRUNABLE_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Authorization middleware for the ES-compat router.
///
/// Layered *inside* [`crate::auth::auth_middleware`] (added to the router
/// before it, so it runs after it): authentication has already rejected
/// anonymous callers, and this decides what the authenticated one may reach.
///
/// A superuser — open mode (`--insecure`, point-at-a-folder) or the configured
/// admin key — short-circuits on the first line, so the zero-configuration
/// local path pays nothing and behaves exactly as before.
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

    if let Err(denied) = decide(&principal, &method, &segs, &target) {
        return denied;
    }

    let response = next.run(req).await;
    if enumerates(&segs) {
        prune_response(response, &principal).await
    } else {
        response
    }
}

/// The whole decision, split out so it can be unit-tested without a router.
fn decide(
    principal: &Principal,
    method: &Method,
    segs: &[String],
    target: &Target,
) -> Result<(), Response> {
    // Restated here and not only in the middleware, so this function is a
    // complete decision on its own and cannot deny the local-dev superuser if
    // it is ever called from somewhere else.
    if principal.is_superuser() {
        return Ok(());
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
                if expr.contains('*') || expr == "_all" {
                    // A pattern is never expanded and then filtered — it is
                    // refused outright, so a caller can never learn what it
                    // *would* have matched. A pattern that could reach the
                    // reserved namespace is refused for everyone below
                    // superuser; any other pattern is refused for a scoped
                    // principal, which must name the index it was granted, and
                    // kept for a legacy unscoped one (`logs-*` still works).
                    if may_reach_reserved(expr) || !matches!(principal, Principal::Unscoped { .. })
                    {
                        return Err(forbidden(principal, expr, privilege));
                    }
                    continue;
                }
                // A literal name — including a reserved one the principal was
                // explicitly granted, which it may then use directly.
                authorize_index(principal, expr, privilege)?;
            }
            Ok(())
        }
        Target::GlobalMetadata => match principal {
            // A scoped credential names its index or gets nothing: metadata
            // endpoints answer across the cluster, and "answer then filter" is
            // one missed shape away from a leak.
            Principal::Scoped { .. } | Principal::Denied => {
                Err(forbidden(principal, "<cluster>", Privilege::ReadIndex))
            }
            _ => Ok(()),
        },
        Target::GlobalFanout => Err(forbidden(
            principal,
            "<all indices>",
            required_privilege(method, segs, 1),
        )),
    }
}

/// Does this path's response enumerate index names?
///
/// Kept narrow on purpose: pruning buffers and re-serializes the body, so it
/// runs only where a listing can actually appear — never on a search response.
fn enumerates(segs: &[String]) -> bool {
    match segs.first().map(String::as_str) {
        Some("v1") => matches!(
            segs.get(1).map(String::as_str),
            Some("dashboard") | Some("cluster")
        ),
        Some(s) => ENUMERATING_ROOTS.contains(&s),
        None => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Enumeration pruning
// ─────────────────────────────────────────────────────────────────────────────

/// Remove reserved index names the principal cannot read from a metadata
/// response, so `GET /_mapping` and friends do not hand over the list of
/// brains on the node.
async fn prune_response(response: Response, principal: &Principal) -> Response {
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

    let pruned: Vec<u8> = if is_json {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(mut v) => {
                prune_json(&mut v, principal);
                serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec())
            }
            // Not JSON after all (some `_cat` handlers answer text under a
            // JSON content type); fall through to the line filter.
            Err(_) => prune_text(&bytes, principal),
        }
    } else {
        prune_text(&bytes, principal)
    };

    let mut parts = parts;
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(pruned))
}

/// Is this index name one the principal must not even see?
fn hidden(name: &str, principal: &Principal) -> bool {
    is_reserved_index(name) && !principal.allows_index(name, Privilege::ReadIndex)
}

/// Recursively drop reserved-index keys, string entries and `{index|name: …}`
/// records. One walk covers every metadata shape: `_mapping`/`_settings`/
/// `_alias` (object keyed by index), `_stats`/`_cluster/state` (the same under
/// `"indices"`), `_cat/indices?format=json` (array of `{index: …}`),
/// `_resolve/index` (array of `{name: …}`) and `_field_caps` (array of names).
fn prune_json(v: &mut Value, principal: &Principal) {
    match v {
        Value::Object(map) => {
            map.retain(|k, _| !hidden(k, principal));
            for (_, child) in map.iter_mut() {
                prune_json(child, principal);
            }
        }
        Value::Array(items) => {
            items.retain(|item| match item {
                Value::String(s) => !hidden(s, principal),
                Value::Object(o) => !o
                    .get("index")
                    .or_else(|| o.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|n| hidden(n, principal))
                    .unwrap_or(false),
                _ => true,
            });
            for item in items.iter_mut() {
                prune_json(item, principal);
            }
        }
        _ => {}
    }
}

/// Line filter for `_cat` in its default text form: a row naming a hidden
/// index is dropped whole.
fn prune_text(bytes: &[u8], principal: &Principal) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes.to_vec();
    };
    if !text.contains(RESERVED_INDEX_PREFIX) {
        return bytes.to_vec();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let hides = line.split_whitespace().any(|token| {
            hidden(
                token.trim_matches(|c: char| c == '"' || c == ','),
                principal,
            )
        });
        if !hides {
            out.push_str(line);
        }
    }
    out.into_bytes()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
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
        assert_eq!(classify("/_search"), Target::GlobalFanout);
        assert_eq!(classify("/_bulk"), Target::GlobalFanout);
        assert_eq!(classify("/_all/_search"), Target::GlobalFanout);
        assert_eq!(classify("/_mapping"), Target::GlobalMetadata);
        assert_eq!(classify("/_cat/indices"), Target::GlobalMetadata);
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
        assert_eq!(classify("/v1/dashboard/summary"), Target::GlobalMetadata);
        // Body-named index: no target, so it is fan-out, not metadata.
        assert_eq!(classify("/v1/indices"), Target::GlobalFanout);
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
        let check = |p: &Principal, method: Method, path: &str| {
            let segs = segments(path);
            decide(p, &method, &segs, &classify(path)).is_ok()
        };

        // Alice reaches her own brain …
        assert!(check(&alice, Method::GET, "/_graph/alice/ego"));
        assert!(check(&alice, Method::GET, "/_graph/alice/overview"));
        assert!(check(&alice, Method::POST, "/_graph/alice/link"));
        assert!(check(&alice, Method::POST, "/_memory/alice/_recall"));
        // … and nothing of bob's, by any door.
        assert!(!check(&alice, Method::GET, "/_graph/bob/ego"));
        assert!(!check(&alice, Method::GET, "/_graph/bob/overview"));
        assert!(!check(&alice, Method::POST, "/_graph/bob/link"));
        assert!(!check(&alice, Method::DELETE, "/_graph/bob/link/e1"));
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-bob-edges/_search"
        ));
        assert!(!check(
            &alice,
            Method::POST,
            "/.xerj-memory-bob-edges/_doc/x"
        ));
        assert!(!check(&alice, Method::DELETE, "/.xerj-memory-bob-edges"));
        assert!(!check(&alice, Method::POST, "/_memory/bob/_recall"));
        assert!(!check(&alice, Method::GET, "/_mapping"));
        assert!(!check(&alice, Method::GET, "/_cat/indices"));
        assert!(!check(&alice, Method::POST, "/_search"));
        assert!(!check(&alice, Method::POST, "/_bulk"));
        assert!(!check(&alice, Method::GET, "/.xerj-memory-*/_search"));
        // Including the native router's spelling of the same index.
        assert!(!check(
            &alice,
            Method::POST,
            "/v1/indices/.xerj-memory-bob-edges/search"
        ));
        assert!(!check(
            &alice,
            Method::DELETE,
            "/v1/indices/.xerj-memory-bob-edges"
        ));
        // A grant is a grant: alice may use her own backing index directly.
        assert!(check(
            &alice,
            Method::POST,
            "/.xerj-memory-alice-edges/_search"
        ));

        // A read-only grant does not write.
        let ro = scoped(&[".xerj-memory-alice-edges"], &[Privilege::ReadIndex]);
        assert!(check(&ro, Method::GET, "/_graph/alice/ego"));
        assert!(!check(&ro, Method::POST, "/_graph/alice/link"));
        // … and does not destroy the backing index.
        assert!(!check(&ro, Method::DELETE, "/.xerj-memory-alice-edges"));

        // A legacy key with no grants: keeps ordinary indices, loses every
        // brain door. This is the fail-closed half.
        let legacy = unscoped();
        assert!(check(&legacy, Method::POST, "/logs-2026/_search"));
        assert!(check(&legacy, Method::GET, "/logs-*/_search"));
        assert!(check(&legacy, Method::GET, "/_mapping"));
        assert!(!check(&legacy, Method::GET, "/_graph/alice/ego"));
        assert!(!check(&legacy, Method::POST, "/_memory/alice/_recall"));
        assert!(!check(
            &legacy,
            Method::POST,
            "/.xerj-memory-alice-edges/_search"
        ));
        assert!(!check(&legacy, Method::GET, "/*/_search"));
        assert!(!check(&legacy, Method::POST, "/_search"));
        // A body-named create could squat `.xerj-memory-victim-edges`, so it is
        // refused too; `PUT /logs-2026` names its target and still works.
        assert!(!check(&legacy, Method::POST, "/v1/indices"));
        assert!(check(&legacy, Method::PUT, "/logs-2026"));

        // No credential resolves to nothing at all.
        let denied = Principal::Denied;
        assert!(!check(&denied, Method::GET, "/_graph/alice/ego"));
        assert!(!check(&denied, Method::GET, "/logs/_search"));
        assert!(!check(&denied, Method::GET, "/_mapping"));

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
                check(&root, Method::GET, path),
                "superuser blocked on {path}"
            );
        }
    }

    #[test]
    fn json_pruning_hides_reserved_indices() {
        let p = scoped(&[".xerj-memory-alice-edges"], &[Privilege::ReadIndex]);
        let mut mapping = serde_json::json!({
            ".xerj-memory-alice-edges": {"mappings": {}},
            ".xerj-memory-bob-edges": {"mappings": {}},
            "logs-2026": {"mappings": {}}
        });
        prune_json(&mut mapping, &p);
        assert!(mapping.get(".xerj-memory-alice-edges").is_some());
        assert!(mapping.get(".xerj-memory-bob-edges").is_none());
        assert!(mapping.get("logs-2026").is_some());

        // Array shapes: `_cat/indices?format=json` and `_resolve/index`.
        let mut cat = serde_json::json!([
            {"index": ".xerj-memory-bob-edges", "docs.count": "1"},
            {"index": "logs-2026", "docs.count": "9"}
        ]);
        prune_json(&mut cat, &p);
        assert_eq!(cat.as_array().unwrap().len(), 1);

        let mut resolved = serde_json::json!({
            "indices": [{"name": ".xerj-memory-bob-edges"}, {"name": "logs-2026"}]
        });
        prune_json(&mut resolved, &p);
        assert_eq!(resolved["indices"].as_array().unwrap().len(), 1);

        // Bare-string lists (`_field_caps.indices`).
        let mut caps = serde_json::json!({"indices": [".xerj-memory-bob-edges", "logs-2026"]});
        prune_json(&mut caps, &p);
        assert_eq!(caps["indices"], serde_json::json!(["logs-2026"]));
    }

    #[test]
    fn text_pruning_drops_cat_rows() {
        let p = unscoped();
        let table = "green open logs-2026 uuid 1 0 9 0 1kb 1kb\n\
                     green open .xerj-memory-bob-edges uuid 1 0 1 0 1kb 1kb\n";
        let out = String::from_utf8(prune_text(table.as_bytes(), &p)).unwrap();
        assert!(out.contains("logs-2026"));
        assert!(!out.contains(".xerj-memory-bob-edges"));
    }
}
