//! Request-level write auditing — issue #329.
//!
//! ## The gap this closes
//!
//! `xerj_engine::audit` has been a real hash-chained, restart-surviving log
//! since #201, but there were exactly four `audit.append` call sites in the
//! whole API surface: `_search`, and the three `_security/api_key` operations.
//! Indexing, updates, deletes, bulk, index creation and index deletion produced
//! **nothing**. An auditor asking "did anyone change this record" got an empty
//! answer, and `audit.rs`'s own module doc had to say that an absent entry was
//! not evidence a write had not happened. That is the whole enterprise ask, and
//! it was the one thing the feature could not do.
//!
//! ## Why a middleware and not `append` at each handler
//!
//! Because the same reasoning that put authorization in [`crate::authz`]'s
//! middleware and in the engine's index funnel applies here: *a handler cannot
//! forget a check it does not make*. `es_compat.rs` alone is ~37k lines with
//! dozens of mutating handlers and several return points each; a hand-placed
//! `append` per success path is a list that is wrong the day someone adds a
//! route. `POST /v1/indices/{name}/syslog` is a write nobody would have
//! remembered.
//!
//! This is also how the reference implementation does it. Elasticsearch emits
//! its audit trail from the security filter layer, not from REST handlers:
//! `AuditTrail`'s vocabulary is request-shaped and outcome-shaped
//! (`accessGranted` / `accessDenied` / `authenticationSuccess`, one per request
//! id — `x-pack/plugin/security/…/audit/AuditTrail.java:26-60`), and
//! `LoggingAuditTrail.accessGranted` records subject + action + the indices
//! pulled off the request (`…/audit/logfile/LoggingAuditTrail.java:687-723`,
//! `AuditUtil.indices`, `…/audit/AuditUtil.java:40-44`). Read for the design
//! only — Elasticsearch is Elastic-2.0/AGPL/SSPL, no code from it is in xerj —
//! and the mechanics here are our own: axum middleware over our `Principal`,
//! writing our ring.
//!
//! ## What it records, and what it deliberately does not
//!
//! * **One entry per request, never per document.** The ring holds
//!   [`xerj_engine::audit::DEFAULT_AUDIT_CAPACITY`] entries; auditing per
//!   document would let one bulk ingest evict every other event on the node,
//!   which is the retention question issue #329 raised. A bulk is one entry
//!   whose note carries the item counts ([`AuditNote`]).
//! * **Every mutation; reads only when refused.** A successful read is not
//!   worth a slot in a shared ring — `_search` keeps its own append, which can
//!   say `took`/`hits`, and auditing every `GET /_cluster/health` from a k8s
//!   probe would flush the writes out. A read that came back `403` *is*
//!   recorded, because a credential reaching for data it does not hold is the
//!   event an auditor is looking for, and because the handler that would have
//!   logged a denied search never runs.
//! * **Authenticated requests only.** This layer sits *inside*
//!   `auth_middleware`, so a 401 leaves no entry. An unauthenticated flood
//!   would otherwise be a one-request-per-line way to evict the evidence from
//!   a bounded ring; the credential-less case is already a clean 401 and is
//!   visible in the access log.
//! * **Denials are entries too.** The layer sits *outside*
//!   [`crate::authz::authz_middleware`], so a refused write is recorded with
//!   `outcome: "denied"`. "Nothing happened" and "someone tried and was
//!   stopped" are different answers to an auditor.
//!
//! Cost is one SHA-256 over a short entry plus one `write(2)` per mutating
//! request — the tax `_search` has always paid, now paid by writes too, and
//! amortised over a whole bulk rather than charged per document.

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::auth::authenticate;
use crate::state::AppState;

/// Extra context a handler wants on its audit entry, passed back through the
/// response extensions.
///
/// The middleware knows the request; only the handler knows how much work it
/// turned into. `process_bulk_body` uses this to report item counts, so a bulk
/// entry says `items=200 failed=0` instead of just `status=200`.
#[derive(Clone, Debug)]
pub struct AuditNote(pub String);

/// What a request is, in audit terms: the op tag and the resource it names.
struct Audited {
    op: String,
    resource: String,
    /// Record this one **only** if it was refused.
    ///
    /// True for reads. A successful read is not evidence anyone needs at the
    /// cost of a ring slot — `_search` keeps its own entry, and auditing every
    /// `GET /_cluster/health` would evict the writes. A read that came back
    /// `403` is a different thing: someone with a credential reached for data
    /// they do not hold, and that is exactly what an auditor is looking for.
    /// It is also the only way a *denied* search gets recorded at all, since
    /// the handler that would have appended never runs.
    only_if_denied: bool,
}

/// Path segments, empty ones dropped.
fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Endpoints that read despite being reached with a mutating method.
///
/// ES uses POST for most reads, so this is the list that keeps a search out of
/// the write log. A name missing from it is a *noisy* entry (an op tagged after
/// the endpoint), never a missing one — the failure direction we want, since
/// the bug being fixed is silence.
fn is_read_shaped(seg: &str) -> bool {
    matches!(
        seg,
        "_search"
            | "_msearch"
            | "_search_scroll"
            | "scroll"
            | "_count"
            | "_mget"
            | "_explain"
            | "explain-plan"
            | "_analyze"
            | "_validate"
            | "_field_caps"
            | "_knn_search"
            | "_terms_enum"
            | "_rank_eval"
            | "_resolve"
            | "_sql"
            | "_render"
            | "_simulate"
            | "_disk_usage"
            | "_recovery"
            | "_stats"
            | "search"
            | "encodings"
    )
}

/// The verb suffix for an endpoint with no better name.
fn verb(method: &Method) -> &'static str {
    match *method {
        Method::PUT => "put",
        Method::POST => "post",
        Method::DELETE => "delete",
        Method::PATCH => "patch",
        _ => "write",
    }
}

/// Classify a request into an audit op + resource, or `None` to skip it.
///
/// Handles both routers, because both reach the same data: the ES-compat
/// spelling puts the index first (`/{index}/_doc/{id}`) and the native one
/// nests it (`/v1/indices/{index}/docs/{id}`).
fn classify(method: &Method, path: &str) -> Option<Audited> {
    let segs = segments(path);
    if segs.is_empty() {
        return None;
    }
    // The three `_security/api_key` operations append their own entries (with
    // a `sync_to_disk` barrier a generic layer should not impose on every
    // write); auditing them here as well would double every one.
    if segs.first() == Some(&"_security") && segs.get(1) == Some(&"api_key") {
        return None;
    }
    // `/_xerj-console/*` is a separate application mounted at the server level
    // with its own `.xerj_audit` trail; the SPA's own traffic is not node data.
    if segs.first() == Some(&"_xerj-console") {
        return None;
    }
    let mutating = matches!(
        *method,
        Method::PUT | Method::POST | Method::DELETE | Method::PATCH
    );
    // A read — by method, or by one of the endpoints ES spells with POST. It
    // earns an entry only when it was refused; see [`Audited::only_if_denied`].
    if !mutating || segs.iter().any(|s| is_read_shaped(s)) {
        return classify_read(&segs);
    }

    // Native router: /v1/indices/{name}/…
    if segs.first() == Some(&"v1") {
        return classify_native(method, &segs);
    }

    let index = segs[0];
    // `PUT /{index}` / `DELETE /{index}` — the whole index.
    if segs.len() == 1 && !index.starts_with('_') {
        let op = match *method {
            Method::DELETE => "index.delete",
            _ => "index.create",
        };
        return Some(Audited {
            op: op.to_string(),
            resource: index.to_string(),
            only_if_denied: false,
        });
    }
    // The endpoint keyword is the first `_`-prefixed segment.
    let keyword = segs.iter().find(|s| s.starts_with('_')).copied()?;
    let op = match (keyword, method) {
        ("_doc", &Method::DELETE) => "delete".to_string(),
        ("_doc", _) => "index".to_string(),
        ("_create", _) => "create".to_string(),
        ("_update", _) => "update".to_string(),
        ("_bulk", _) => "bulk".to_string(),
        ("_delete_by_query", _) => "delete_by_query".to_string(),
        ("_update_by_query", _) => "update_by_query".to_string(),
        ("_reindex", _) => "reindex".to_string(),
        (k, m) => format!("{}.{}", k.trim_start_matches('_'), verb(m)),
    };
    // `segs[0]` is the index when the path is index-scoped (`/payroll/_doc/1`)
    // and the endpoint itself when it is not (`/_bulk`, `/_aliases`,
    // `/_reindex`). Both are recorded as-is: an entry that reported a *guessed*
    // index — parsed out of a bulk body that may name a dozen — would be worse
    // evidence than one that says plainly which endpoint was called. The
    // per-index truth for those requests is in the response the caller got, and
    // authorization over body-named indices is `authz`'s job, not this layer's.
    Some(Audited {
        op,
        resource: index.to_string(),
        only_if_denied: false,
    })
}

/// A read, described well enough to be worth an entry **if it was refused**.
///
/// The op tag matches what the successful path would have written — a denied
/// search is `op: "search", outcome: "denied"`, next to the searches that were
/// allowed — so an auditor filtering by op sees both halves of the story.
fn classify_read(segs: &[&str]) -> Option<Audited> {
    let (op, resource) = match segs {
        // Native router: /v1/indices/{name}/search and friends.
        ["v1", "indices", name, tail @ ..] => (
            tail.last().copied().unwrap_or("read").trim_start_matches('_'),
            (*name).to_string(),
        ),
        [first, rest @ ..] => {
            let keyword = rest
                .iter()
                .chain(std::iter::once(first))
                .find(|s| s.starts_with('_'))
                .copied()
                .unwrap_or("_read");
            (keyword.trim_start_matches('_'), (*first).to_string())
        }
        [] => return None,
    };
    Some(Audited {
        op: op.to_string(),
        resource,
        only_if_denied: true,
    })
}

/// `/v1/…` — the native router's spelling of the same writes.
fn classify_native(method: &Method, segs: &[&str]) -> Option<Audited> {
    match segs {
        // POST /v1/indices — create, named in the body.
        ["v1", "indices"] => Some(Audited {
            op: "index.create".to_string(),
            resource: "_indices".to_string(),
            only_if_denied: false,
        }),
        ["v1", "indices", name] => Some(Audited {
            op: if *method == Method::DELETE {
                "index.delete".to_string()
            } else {
                format!("index.{}", verb(method))
            },
            resource: (*name).to_string(),
            only_if_denied: false,
        }),
        ["v1", "indices", name, tail @ ..] => {
            let op = match tail {
                ["docs"] | ["logs"] | ["otlp"] | ["syslog"] | ["turbo-ingest"] | ["ingest"] => {
                    "index".to_string()
                }
                ["docs", "_bulk"] => "bulk".to_string(),
                ["docs", _id] if *method == Method::DELETE => "delete".to_string(),
                ["docs", _id] => "index".to_string(),
                other => format!(
                    "{}.{}",
                    other.last().copied().unwrap_or("index").trim_start_matches('_'),
                    verb(method)
                ),
            };
            Some(Audited {
                op,
                resource: (*name).to_string(),
                only_if_denied: false,
            })
        }
        ["v1", rest @ ..] => Some(Audited {
            op: format!(
                "{}.{}",
                rest.join(".").trim_start_matches('_'),
                verb(method)
            ),
            resource: format!("/v1/{}", rest.join("/")),
            only_if_denied: false,
        }),
        _ => None,
    }
}

/// `ok` / `denied` / `error`, from the status the caller actually got.
///
/// A bulk that returns 200 with per-item failures is `ok` at this layer and
/// says so in its note — the request was accepted and did write; the items
/// that did not are in the response the caller already has.
fn outcome_of(status: StatusCode) -> &'static str {
    if status.is_success() || status.is_redirection() {
        "ok"
    } else if status == StatusCode::FORBIDDEN {
        "denied"
    } else {
        "error"
    }
}

/// Record one audit entry per authenticated, mutating request.
///
/// Mounted **outside** [`crate::authz::authz_middleware`] (so refusals are
/// recorded) and **inside** `auth_middleware` (so an unauthenticated flood
/// cannot evict the log). See the module docs for the full reasoning.
pub async fn audit_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(audited) = classify(req.method(), req.uri().path()) else {
        return next.run(req).await;
    };
    // Re-derived from the header rather than read from an extension, for the
    // same reason `Principal`'s extractor does: this layer must name the right
    // subject even if it is ever mounted without the authn layer above it.
    let subject = authenticate(
        &state,
        req.headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
    )
    .label()
    .to_string();

    let response = next.run(req).await;

    // A read that succeeded is not recorded: the ring is small, shared, and
    // better spent on changes and refusals.
    if audited.only_if_denied && response.status() != StatusCode::FORBIDDEN {
        return response;
    }

    let note = response
        .extensions()
        .get::<AuditNote>()
        .map(|n| n.0.clone())
        .unwrap_or_else(|| format!("status={}", response.status().as_u16()));
    state.engine.audit.append(
        &audited.op,
        &subject,
        &audited.resource,
        outcome_of(response.status()),
        &note,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request that earns an entry whatever the outcome (i.e. a write).
    fn c(method: Method, path: &str) -> Option<(String, String)> {
        classify(&method, path)
            .filter(|a| !a.only_if_denied)
            .map(|a| (a.op, a.resource))
    }

    /// A request recorded only when it is refused (i.e. a read).
    fn denied_only(method: Method, path: &str) -> Option<(String, String)> {
        classify(&method, path)
            .filter(|a| a.only_if_denied)
            .map(|a| (a.op, a.resource))
    }

    #[test]
    fn document_writes_are_classified() {
        assert_eq!(
            c(Method::PUT, "/payroll/_doc/1"),
            Some(("index".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::POST, "/payroll/_doc"),
            Some(("index".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::DELETE, "/payroll/_doc/1"),
            Some(("delete".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::POST, "/payroll/_update/1"),
            Some(("update".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::PUT, "/payroll/_create/1"),
            Some(("create".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::POST, "/payroll/_bulk"),
            Some(("bulk".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::POST, "/_bulk"),
            Some(("bulk".into(), "_bulk".into()))
        );
        assert_eq!(
            c(Method::PUT, "/payroll"),
            Some(("index.create".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::DELETE, "/payroll"),
            Some(("index.delete".into(), "payroll".into()))
        );
        assert_eq!(
            c(Method::POST, "/payroll/_delete_by_query"),
            Some(("delete_by_query".into(), "payroll".into()))
        );
    }

    /// The `_search` entry is appended by the handler, which can report
    /// `took`/`hits`; a second one from here would double every search.
    #[test]
    fn reads_are_not_write_audited() {
        assert_eq!(c(Method::POST, "/payroll/_search"), None);
        assert_eq!(c(Method::POST, "/_msearch"), None);
        assert_eq!(c(Method::POST, "/payroll/_count"), None);
        assert_eq!(c(Method::GET, "/payroll/_doc/1"), None);
        assert_eq!(c(Method::HEAD, "/payroll"), None);
        assert_eq!(c(Method::POST, "/v1/indices/payroll/search"), None);
    }

    /// …but a refused one is recorded, under the op the allowed path uses, so
    /// an auditor filtering `op: search` sees the attempts as well as the hits.
    #[test]
    fn refused_reads_are_recorded() {
        assert_eq!(
            denied_only(Method::POST, "/payroll/_search"),
            Some(("search".into(), "payroll".into()))
        );
        assert_eq!(
            denied_only(Method::GET, "/payroll/_doc/1"),
            Some(("doc".into(), "payroll".into()))
        );
        assert_eq!(
            denied_only(Method::POST, "/v1/indices/payroll/search"),
            Some(("search".into(), "payroll".into()))
        );
        // A write is never in this bucket — it is recorded either way.
        assert_eq!(denied_only(Method::PUT, "/payroll/_doc/1"), None);
    }

    /// The api-key operations audit themselves, with a durability barrier this
    /// layer must not impose on every write.
    #[test]
    fn the_api_key_ops_are_not_double_audited() {
        assert_eq!(c(Method::POST, "/_security/api_key"), None);
        assert_eq!(c(Method::DELETE, "/_security/api_key"), None);
        // …but the rest of the security surface is not exempt.
        assert_eq!(
            c(Method::PUT, "/_security/role/auditor"),
            Some(("security.put".into(), "_security".into()))
        );
    }

    /// Every native write path is covered without being enumerated by hand —
    /// including the ingest shapes (`logs`, `otlp`, `syslog`) that no
    /// per-handler `append` list would have remembered.
    #[test]
    fn the_native_router_is_covered_too() {
        assert_eq!(
            c(Method::POST, "/v1/indices/logs-app/docs"),
            Some(("index".into(), "logs-app".into()))
        );
        assert_eq!(
            c(Method::POST, "/v1/indices/logs-app/docs/_bulk"),
            Some(("bulk".into(), "logs-app".into()))
        );
        assert_eq!(
            c(Method::DELETE, "/v1/indices/logs-app/docs/7"),
            Some(("delete".into(), "logs-app".into()))
        );
        assert_eq!(
            c(Method::POST, "/v1/indices/logs-app/syslog"),
            Some(("index".into(), "logs-app".into()))
        );
        assert_eq!(
            c(Method::DELETE, "/v1/indices/logs-app"),
            Some(("index.delete".into(), "logs-app".into()))
        );
        assert_eq!(
            c(Method::POST, "/v1/indices"),
            Some(("index.create".into(), "_indices".into()))
        );
    }

    #[test]
    fn outcomes_map_to_the_three_words_the_entry_allows() {
        assert_eq!(outcome_of(StatusCode::OK), "ok");
        assert_eq!(outcome_of(StatusCode::CREATED), "ok");
        assert_eq!(outcome_of(StatusCode::FORBIDDEN), "denied");
        assert_eq!(outcome_of(StatusCode::NOT_FOUND), "error");
        assert_eq!(outcome_of(StatusCode::INTERNAL_SERVER_ERROR), "error");
    }
}
