//! Streamable HTTP transport for `xerj mcp --http` (#3066).
//!
//! MCP spec revision 2025-06-18, "Transports → Streamable HTTP": one endpoint
//! (`/mcp`) takes JSON-RPC over `POST`. A request is answered with one
//! `application/json` body; a POST that carries only notifications or
//! responses is answered `202 Accepted` with no body. The server opens no SSE
//! stream, so `GET /mcp` is `405`, and it issues no `Mcp-Session-Id`, so
//! `DELETE /mcp` is `405` as well. Every message goes through the same
//! `handle_message` the stdio loop uses: this module only adds the transport
//! and the policy in front of it.
//!
//! Policy, in the order a request meets it:
//!
//! 1. **Origin.** A browser always sends `Origin` on a POST. The spec asks
//!    servers to validate it, because a page on `evil.example` can re-point
//!    its own DNS name at 127.0.0.1 (DNS rebinding) and then talk to a
//!    loopback listener. An absent `Origin` (curl, an agent runtime, a tunnel
//!    client) passes; a loopback origin passes; anything else must be named
//!    with `--allow-origin`. `Origin: null` never passes.
//! 2. **Bearer token.** Required whenever one is configured, and a
//!    non-loopback bind refuses to start without one ([`check_bind`]). The
//!    token is its own secret (`XERJ_MCP_TOKEN` or `--token-file`), never the
//!    node's `admin_api_key`, compared in constant time and never printed.
//!    With no token, a request that arrived through a reverse proxy or tunnel
//!    (it carries `X-Forwarded-For`, `Forwarded`, `X-Real-IP`,
//!    `CF-Connecting-IP` or `Tailscale-User-Login`) is refused: a tunnel in
//!    front of a loopback bind would otherwise publish an unauthenticated
//!    endpoint. That check is best effort, which is why the docs say to set a
//!    token before starting any tunnel.
//! 3. **Tool scope.** The proxy forwards with the node credential, so a remote
//!    caller would otherwise hold everything that credential can do. Over
//!    HTTP `tools/list` and `tools/call` cover only [`READ_ONLY_TOOLS`] unless
//!    the server was started with `--allow-writes`. The published schema
//!    (`landing/docs/agents/schemas/mcp-tools.json`) stays the full stdio
//!    list: the filter is per transport, not a second tool list.
//!
//! Reference: qdrant `src/common/auth/mod.rs:141-170` (Apache-2.0) reads the
//! key from `Authorization: Bearer …` and compares with `ct_eq`, and keeps a
//! separate read-only key; the read-only default here follows the same idea.
//! Written for this crate, not copied.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::{json, Value};

use crate::{handle_message, rpc_error, tool_specs, tool_text, Ctx};

/// Default listen address: loopback, so a plain `xerj mcp --http` is reachable
/// from this machine only.
pub(crate) const DEFAULT_BIND: &str = "127.0.0.1:9330";

/// Environment variable holding the bearer token.
pub(crate) const TOKEN_ENV: &str = "XERJ_MCP_TOKEN";

/// Shortest token accepted. A short secret on a network listener is a guess
/// away; 16 characters is the floor, `openssl rand -hex 32` the suggestion.
pub(crate) const MIN_TOKEN_LEN: usize = 16;

/// Largest request body read. Tool arguments are small; a batch search with
/// many queries is the largest legitimate body and stays far below this.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Tools served over HTTP by default: they read the node, a local pack or a
/// catalogue already cached, and change nothing.
///
/// Left out unless `--allow-writes`: `xerj_memory_store`, `xerj_brain_link`,
/// `xerj_brain_unlink`, `xerj_sources` (connect/delete), `xerj_autoindex_remove`
/// (they change the node); `xerj_index_budget` (walks any folder path on this
/// machine the caller names); `xerj_field_report`, `xerj_corpus_feedback`,
/// `xerj_hub_ask`, `xerj_hub_def` (they send to a third party, the hub pair
/// through the automatic corpus review).
pub const READ_ONLY_TOOLS: &[&str] = &[
    "xerj_def",
    "xerj_references",
    "xerj_batch_search",
    "xerj_search",
    "xerj_semantic_search",
    "xerj_vector_search",
    "xerj_hybrid_search",
    "xerj_memory_recall",
    "xerj_brain_ego",
    "xerj_brain_overview",
    "xerj_map",
    "xerj_status",
    "xerj_code_search",
    "xerj_plan",
    "xerj_hub_find",
];

/// Request headers a reverse proxy or tunnel adds. Their presence on a
/// token-less server means the loopback bind is being published.
const FORWARDING_HEADERS: &[&str] = &[
    "x-forwarded-for",
    "forwarded",
    "x-real-ip",
    "cf-connecting-ip",
    "tailscale-user-login",
];

/// What `--http` was asked for.
#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    /// Bearer token; `None` serves without auth (loopback bind only).
    pub token: Option<String>,
    /// Expose the write tools too.
    pub allow_writes: bool,
    /// Exact origins (`scheme://host[:port]`) allowed besides loopback ones.
    pub allowed_origins: Vec<String>,
}

impl HttpOptions {
    fn exposes(&self, tool: &str) -> bool {
        self.allow_writes || READ_ONLY_TOOLS.contains(&tool)
    }
}

/// Refuse a bind that would publish the endpoint without a token. Checked
/// before the socket is opened.
pub(crate) fn check_bind(addr: &SocketAddr, token: Option<&str>) -> anyhow::Result<()> {
    if let Some(t) = token {
        if t.chars().count() < MIN_TOKEN_LEN {
            anyhow::bail!(
                "the MCP bearer token is {} characters; use at least {MIN_TOKEN_LEN} \
                 (for example `openssl rand -hex 32`)",
                t.chars().count()
            );
        }
        return Ok(());
    }
    if !addr.ip().is_loopback() {
        anyhow::bail!(
            "refusing to serve MCP over HTTP on {addr} without a bearer token: a \
             non-loopback bind is reachable from other machines. Set {TOKEN_ENV} or \
             pass --token-file <path> (at least {MIN_TOKEN_LEN} characters), or bind \
             127.0.0.1"
        );
    }
    Ok(())
}

/// Compare two secrets without an early exit on the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

/// Host part of an `Origin` value (`scheme://host[:port]`), brackets removed.
fn origin_host(origin: &str) -> Option<&str> {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    if rest.is_empty() || rest.contains(['/', '@', '?', '#']) {
        return None;
    }
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split_once(']').map(|(h, _)| h);
    }
    Some(rest.rsplit_once(':').map_or(rest, |(h, _)| h))
}

fn origin_is_loopback(origin: &str) -> bool {
    match origin_host(origin) {
        Some(h) if h.eq_ignore_ascii_case("localhost") => true,
        Some(h) => h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// `true` when the request may proceed past the Origin check.
fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(raw) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = raw.to_str() else {
        return false;
    };
    let origin = origin.trim().trim_end_matches('/');
    origin_is_loopback(origin)
        || allowed
            .iter()
            .any(|a| a.trim_end_matches('/').eq_ignore_ascii_case(origin))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn forwarded(headers: &HeaderMap) -> Option<&'static str> {
    FORWARDING_HEADERS
        .iter()
        .copied()
        .find(|h| headers.contains_key(*h))
}

/// An HTTP-level refusal: a JSON-RPC error with a null id, as the spec allows
/// for errors that belong to no request.
fn refuse(status: StatusCode, message: impl Into<String>) -> Response {
    let body = rpc_error(Value::Null, -32000, message);
    let mut resp = (status, axum::Json(body)).into_response();
    if status == StatusCode::UNAUTHORIZED {
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"xerj-mcp\""),
        );
    }
    resp
}

struct Shared {
    ctx: Ctx,
    opts: HttpOptions,
}

/// One message under the HTTP tool scope; `None` when it needs no reply.
async fn handle_scoped(shared: &Shared, msg: Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "tools/list" if !shared.opts.allow_writes => {
            let id = msg.get("id").cloned()?;
            let tools: Vec<Value> = tool_specs()
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter(|t| {
                            t.get("name")
                                .and_then(Value::as_str)
                                .is_some_and(|n| shared.opts.exposes(n))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            Some(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } }))
        }
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !shared.opts.exposes(name) && !name.is_empty() {
                let id = msg.get("id").cloned()?;
                let text = format!(
                    "`{name}` is not served over HTTP: this endpoint exposes read-only \
                     tools ({}). The operator can restart it with `xerj mcp --http \
                     --allow-writes` to serve every tool.",
                    READ_ONLY_TOOLS.join(", ")
                );
                return Some(json!({
                    "jsonrpc": "2.0", "id": id, "result": tool_text(text, true),
                }));
            }
            handle_message(&shared.ctx, msg).await
        }
        "initialize" if !shared.opts.allow_writes => {
            let mut reply = handle_message(&shared.ctx, msg).await?;
            if let Some(Value::String(s)) = reply.pointer_mut("/result/instructions") {
                s.push_str(
                    " Over this HTTP endpoint only the read-only tools are served; \
                     tools that store, link, delete or send are not listed.",
                );
            }
            Some(reply)
        }
        _ => handle_message(&shared.ctx, msg).await,
    }
}

async fn mcp_endpoint(
    State(shared): State<Arc<Shared>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !origin_allowed(&headers, &shared.opts.allowed_origins) {
        return refuse(
            StatusCode::FORBIDDEN,
            "Origin not allowed: start the server with --allow-origin <origin> to admit it",
        );
    }
    match shared.opts.token.as_deref() {
        Some(token) => {
            let ok = bearer(&headers)
                .is_some_and(|given| constant_time_eq(given.as_bytes(), token.as_bytes()));
            if !ok {
                return refuse(
                    StatusCode::UNAUTHORIZED,
                    "missing or wrong bearer token: send `Authorization: Bearer <token>`",
                );
            }
        }
        None => {
            if let Some(h) = forwarded(&headers) {
                return refuse(
                    StatusCode::FORBIDDEN,
                    format!(
                        "this request came through a proxy or tunnel (it carries `{h}`), and \
                         this server has no bearer token. Restart it with {TOKEN_ENV} or \
                         --token-file before publishing it"
                    ),
                );
            }
        }
    }
    if method != Method::POST {
        let mut resp = refuse(
            StatusCode::METHOD_NOT_ALLOWED,
            "this MCP endpoint takes JSON-RPC over POST only; it opens no SSE stream and \
             keeps no session",
        );
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return resp;
    }
    if let Some(ct) = headers.get(header::CONTENT_TYPE) {
        let json = ct
            .to_str()
            .is_ok_and(|v| v.to_ascii_lowercase().contains("json"));
        if !json {
            return refuse(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "send the JSON-RPC message with Content-Type: application/json",
            );
        }
    }
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(rpc_error(Value::Null, -32700, format!("parse error: {e}"))),
            )
                .into_response()
        }
    };
    let reply = match parsed {
        // 2025-06-18 dropped batches; 2024-11-05 clients still send them, and
        // the stdio loop answers both, so this does too.
        Value::Array(batch) => {
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = handle_scoped(&shared, m).await {
                    out.push(r);
                }
            }
            (!out.is_empty()).then_some(Value::Array(out))
        }
        msg @ Value::Object(_) => handle_scoped(&shared, msg).await,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(rpc_error(
                    Value::Null,
                    -32600,
                    "invalid request: send a JSON-RPC object (or an array of them)",
                )),
            )
                .into_response()
        }
    };
    match reply {
        Some(body) => (StatusCode::OK, axum::Json(body)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn router(ctx: Ctx, opts: HttpOptions) -> Router {
    let shared = Arc::new(Shared { ctx, opts });
    Router::new()
        .route("/mcp", any(mcp_endpoint))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(shared)
}

/// Serve `/mcp` on `listener` until the process is interrupted.
pub(crate) async fn serve(
    ctx: Ctx,
    listener: tokio::net::TcpListener,
    opts: HttpOptions,
) -> anyhow::Result<()> {
    axum::serve(listener, router(ctx, opts))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_bind_without_token_is_refused() {
        let any: SocketAddr = "0.0.0.0:9330".parse().unwrap();
        let err = check_bind(&any, None).unwrap_err().to_string();
        assert!(err.contains("without a bearer token"), "{err}");
        assert!(err.contains(TOKEN_ENV), "{err}");
        let lan: SocketAddr = "[2001:db8::1]:9330".parse().unwrap();
        assert!(check_bind(&lan, None).is_err());
        assert!(check_bind(&any, Some("0123456789abcdef")).is_ok());
        for lo in ["127.0.0.1:9330", "[::1]:9330", "127.0.0.2:1"] {
            assert!(check_bind(&lo.parse().unwrap(), None).is_ok(), "{lo}");
        }
    }

    #[test]
    fn short_token_is_refused_even_on_loopback() {
        let lo: SocketAddr = "127.0.0.1:9330".parse().unwrap();
        let err = check_bind(&lo, Some("short")).unwrap_err().to_string();
        assert!(err.contains("at least 16"), "{err}");
        assert!(
            !err.contains("short"),
            "the token must not be echoed: {err}"
        );
    }

    #[test]
    fn constant_time_eq_matches_only_equal_secrets() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn origin_check_admits_loopback_and_listed_origins_only() {
        let mut h = HeaderMap::new();
        assert!(origin_allowed(&h, &[]), "no Origin: a non-browser client");
        let allowed = vec!["https://chat.example.com".to_string()];
        for (origin, ok) in [
            ("http://localhost:3000", true),
            ("http://127.0.0.1", true),
            ("http://[::1]:8080", true),
            ("https://chat.example.com", true),
            ("https://CHAT.example.com/", true),
            ("https://evil.example", false),
            ("http://localhost.evil.example", false),
            ("http://127.0.0.1@evil.example", false),
            ("http://0.0.0.0:9330", false),
            ("null", false),
            ("file://", false),
        ] {
            h.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
            assert_eq!(origin_allowed(&h, &allowed), ok, "{origin}");
        }
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_other_schemes_are_not_bearer() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer tok"),
        );
        assert_eq!(bearer(&h), Some("tok"));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("ApiKey tok"),
        );
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer"));
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn read_only_scope_names_only_served_tools_and_no_writer() {
        let all = crate::tool_specs_with(true);
        let names: Vec<&str> = all
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for t in READ_ONLY_TOOLS {
            assert!(names.contains(t), "{t} is not a served tool: {names:?}");
        }
        for writer in [
            "xerj_memory_store",
            "xerj_brain_link",
            "xerj_brain_unlink",
            "xerj_sources",
            "xerj_autoindex_remove",
            "xerj_index_budget",
            "xerj_field_report",
            "xerj_corpus_feedback",
            "xerj_hub_ask",
            "xerj_hub_def",
        ] {
            assert!(names.contains(&writer), "{writer} vanished from tools/list");
            assert!(!READ_ONLY_TOOLS.contains(&writer), "{writer}");
        }
        // Every served tool is classified: a new tool must be put on one side.
        let classified = READ_ONLY_TOOLS.len() + 10;
        assert_eq!(classified, names.len(), "{names:?}");
    }
}
