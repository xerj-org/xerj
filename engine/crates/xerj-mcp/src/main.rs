//! `xerj-mcp` — a Model Context Protocol (MCP) server for XERJ.
//!
//! This binary speaks the MCP **stdio transport**: newline-delimited
//! JSON-RPC 2.0 messages on stdin/stdout. It exposes XERJ to any MCP-capable
//! agent host (Claude Desktop, IDE agents, custom orchestrators) as six
//! tools that map 1:1 onto XERJ's real, verified REST surface. Every tool is
//! a *thin proxy*: it constructs exactly the request body the running engine
//! already accepts and forwards it to a configurable base URL.
//!
//! ## The six canonical agent operations
//!
//! | MCP tool               | XERJ endpoint                         | Real capability |
//! |------------------------|---------------------------------------|-----------------|
//! | `xerj_search`          | `POST /{index}/_search`               | ES query-DSL search (full-text / keyword / structured) |
//! | `xerj_semantic_search` | `POST /{index}/_search` (`semantic`)  | server-side lexical embedding, no external key |
//! | `xerj_vector_search`   | `POST /{index}/_search` (`knn`)       | kNN over a `dense_vector` field (HNSW-served unfiltered, exact filtered) |
//! | `xerj_hybrid_search`   | `POST /{index}/_search` (`hybrid`)    | RRF or linear fusion of sub-queries |
//! | `xerj_memory_store`    | `POST /_memory/{ns}`                  | namespaced agent-memory write |
//! | `xerj_memory_recall`   | `POST /_memory/{ns}/_recall`          | recall by meaning (BM25 / semantic / vector) |
//!
//! ## Honesty notes (must match the engine, never oversell)
//!
//! * Unfiltered kNN is **HNSW-served with exact rescoring** (measured recall@10
//!   1.00 on the official bench query); filtered kNN and other ineligible shapes
//!   (non-cosine, SQ8, small indexes) run the exact brute-force scan. The tool
//!   description says so.
//! * `hybrid` supports `fusion: "rrf"` and `"linear"` only. `"learned"` is
//!   forwarded verbatim and the engine rejects it loudly; the schema therefore
//!   advertises only `rrf`/`linear`.
//! * This proxy adds **no** capabilities of its own — whatever the engine
//!   returns (including errors) is passed straight back to the agent.
//!
//! ## Configuration (environment)
//!
//! * `XERJ_URL`  — base URL of the XERJ ES-compatible listener. Default
//!   `http://localhost:9200`.
//! * `XERJ_AUTH` — optional; if set, sent verbatim as the `Authorization`
//!   header on every proxied request (e.g. `ApiKey <token>`).
//!
//! Diagnostics go to **stderr**; stdout is reserved exclusively for the
//! JSON-RPC stream.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// MCP protocol revision we default to when the client does not request one.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
/// Default XERJ ES-compatible endpoint.
const DEFAULT_XERJ_URL: &str = "http://localhost:9200";

/// Shared per-process state: the HTTP client + where to proxy to.
struct Ctx {
    client: reqwest::Client,
    base_url: String,
    auth: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base_url = std::env::var("XERJ_URL")
        .unwrap_or_else(|_| DEFAULT_XERJ_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let auth = std::env::var("XERJ_AUTH").ok().filter(|s| !s.is_empty());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let ctx = Ctx {
        client,
        base_url,
        auth,
    };

    eprintln!(
        "xerj-mcp v{} — MCP stdio server, proxying to {}",
        env!("CARGO_PKG_VERSION"),
        ctx.base_url
    );

    // stdout is the JSON-RPC channel; stderr is for logs only.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // Parse error: reply per JSON-RPC (id unknown → null).
                let resp = rpc_error(Value::Null, -32700, format!("parse error: {e}"));
                write_msg(&mut stdout, &resp).await?;
                continue;
            }
        };

        // JSON-RPC allows a batch (array) of messages. MCP 2024-11-05 uses it;
        // 2025-06-18 dropped it. Support both: object → single, array → batch.
        let responses: Vec<Value> = match parsed {
            Value::Array(batch) => {
                let mut out = Vec::new();
                for m in batch {
                    if let Some(r) = handle_message(&ctx, m).await {
                        out.push(r);
                    }
                }
                out
            }
            other => handle_message(&ctx, other).await.into_iter().collect(),
        };

        for resp in responses {
            write_msg(&mut stdout, &resp).await?;
        }
    }

    Ok(())
}

/// Serialize one JSON-RPC message and write it as a single newline-terminated
/// line (the MCP stdio framing), then flush.
async fn write_msg(stdout: &mut tokio::io::Stdout, msg: &Value) -> anyhow::Result<()> {
    let mut buf = serde_json::to_vec(msg)?;
    buf.push(b'\n');
    stdout.write_all(&buf).await?;
    stdout.flush().await?;
    Ok(())
}

/// Route one JSON-RPC message. Returns `Some(response)` for requests and
/// `None` for notifications (no `id`) and messages that need no reply.
async fn handle_message(ctx: &Ctx, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let is_notification = id.is_none();
    let method = msg.get("method").and_then(Value::as_str)?.to_string();

    match method.as_str() {
        "initialize" => Some(rpc_result(id, initialize_result(&msg))),

        // Lifecycle / keepalive notifications — no response.
        "notifications/initialized" | "initialized" | "notifications/cancelled" => None,

        "ping" => Some(rpc_result(id, json!({}))),

        "tools/list" => Some(rpc_result(id, json!({ "tools": tool_specs() }))),

        "tools/call" => Some(rpc_result(id, call_tool(ctx, &msg).await)),

        // Unknown method: error for requests, silence for notifications.
        _ => {
            if is_notification {
                None
            } else {
                Some(rpc_error(
                    id.unwrap_or(Value::Null),
                    -32601,
                    format!("method not found: {method}"),
                ))
            }
        }
    }
}

/// Build the `initialize` result, echoing the client's requested protocol
/// version when present (our JSON-RPC handling is version-agnostic).
fn initialize_result(msg: &Value) -> Value {
    let pv = msg
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);

    json!({
        "protocolVersion": pv,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "xerj-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions":
            "XERJ tools proxy to a running XERJ engine (ES-compatible). Use \
             xerj_search for full-text/keyword/structured queries (ES query DSL), \
             xerj_semantic_search for meaning-based recall over a semantic_text \
             field (embedding is server-side, no key), xerj_vector_search for \
             kNN over a dense_vector field, xerj_hybrid_search to fuse \
             lexical + vector results (rrf|linear), and xerj_memory_store / \
             xerj_memory_recall for durable agent memory recalled by meaning.",
    })
}

// ─────────────────────────── JSON-RPC helpers ──────────────────────────────

fn rpc_result(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() },
    })
}

/// A `tools/call` result carrying a single text block.
fn tool_text(text: impl Into<String>, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text.into() } ],
        "isError": is_error,
    })
}

// ─────────────────────────────── Tools ─────────────────────────────────────

/// The six tool specifications advertised via `tools/list`. Input schemas are
/// plain JSON Schema; every property maps onto a field the engine accepts.
fn tool_specs() -> Value {
    json!([
        {
            "name": "xerj_search",
            "description":
                "Full-text / keyword / structured search over a XERJ index using \
                 the Elasticsearch query DSL. Proxies POST /{index}/_search. \
                 Provide `query` as an ES query object (e.g. {\"match\":{\"body\":\"rust\"}}, \
                 {\"term\":{\"status\":\"open\"}}, or a bool clause). Omit `query` for match_all.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string", "description": "Index name to search." },
                    "query": {
                        "type": "object",
                        "description": "ES query-DSL clause. Omit for match_all.",
                    },
                    "size": { "type": "integer", "description": "Max hits to return (default engine value)." },
                    "from": { "type": "integer", "description": "Offset for pagination." },
                    "sort": { "description": "ES sort clause (array or object)." },
                    "_source": { "description": "Source filtering (bool, field, or {includes,excludes})." }
                },
                "required": ["index"]
            }
        },
        {
            "name": "xerj_semantic_search",
            "description":
                "Meaning-based search over a `semantic_text` field. The query text is \
                 embedded SERVER-SIDE by XERJ's built-in lexical embedder (no external \
                 API key), then matched by vector similarity. Proxies POST /{index}/_search \
                 with {\"query\":{\"semantic\":{...}}}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "field": { "type": "string", "description": "Name of the semantic_text field." },
                    "query": { "type": "string", "description": "Natural-language query text to embed and match." },
                    "k": { "type": "integer", "description": "Number of nearest results (default 10)." },
                    "filter": { "type": "object", "description": "Optional ES query clause applied as a pre-filter." }
                },
                "required": ["index", "field", "query"]
            }
        },
        {
            "name": "xerj_vector_search",
            "description":
                "K-nearest-neighbour search over a `dense_vector` field, given a \
                 caller-supplied query vector. NOTE: unfiltered kNN is HNSW-served \
                 (approximate) with exact rescoring — measured recall@10 1.00 on the \
                 official bench query; num_candidates sets the beam width (floored at \
                 800). Filtered kNN, non-cosine metrics, SQ8 fields, and small indexes \
                 run an exact brute-force scan. \
                 Proxies POST /{index}/_search with a top-level {\"knn\":{...}}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "field": { "type": "string", "description": "Name of the dense_vector field." },
                    "query_vector": {
                        "type": "array",
                        "items": { "type": "number" },
                        "description": "Query embedding; length must match the field's dims."
                    },
                    "k": { "type": "integer", "description": "Number of nearest neighbours (default 10)." },
                    "num_candidates": { "type": "integer", "description": "Optional candidate pool size." },
                    "filter": { "type": "object", "description": "Optional ES query clause applied as a pre-filter." }
                },
                "required": ["index", "field", "query_vector"]
            }
        },
        {
            "name": "xerj_hybrid_search",
            "description":
                "Hybrid search: fuse several sub-queries (e.g. a lexical `match` plus a \
                 vector `knn`) into one ranked list. Fusion is `rrf` (reciprocal-rank) or \
                 `linear` (weighted). Proxies POST /{index}/_search with \
                 {\"query\":{\"hybrid\":{\"queries\":[...],\"fusion\":...}}}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": { "type": "string" },
                    "queries": {
                        "type": "array",
                        "description":
                            "Sub-queries to fuse. Each item is {\"query\": <ES query clause>, \
                             \"weight\": <number, optional>}.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "object" },
                                "weight": { "type": "number" }
                            },
                            "required": ["query"]
                        }
                    },
                    "fusion": {
                        "type": "string",
                        "enum": ["rrf", "linear"],
                        "description": "Fusion strategy (default rrf)."
                    },
                    "size": { "type": "integer", "description": "Max fused hits to return." }
                },
                "required": ["index", "queries"]
            }
        },
        {
            "name": "xerj_memory_store",
            "description":
                "Store a durable agent memory in a namespace. The text is BM25-indexed \
                 and (via a semantic_text field) auto-embedded so it can later be recalled \
                 by meaning. Proxies POST /_memory/{namespace}. Set `dedup:true` to skip \
                 writing a near-identical existing memory.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": { "type": "string", "description": "Memory namespace (isolates recall)." },
                    "text": { "type": "string", "description": "Free text of the memory." },
                    "metadata": { "type": "object", "description": "Optional structured metadata." },
                    "id": { "type": "string", "description": "Optional explicit id (upsert)." },
                    "vector": {
                        "type": "array",
                        "items": { "type": "number" },
                        "description": "Optional precomputed embedding to enable vector recall."
                    },
                    "dedup": { "type": "boolean", "description": "Skip write if a near-duplicate exists." },
                    "dedup_threshold": { "type": "number", "description": "Similarity threshold for dedup." }
                },
                "required": ["namespace", "text"]
            }
        },
        {
            "name": "xerj_memory_recall",
            "description":
                "Recall the most relevant memories from a namespace. Default is BM25 text \
                 recall; set `semantic:true` to embed the query server-side and recall by \
                 meaning; supply `vector` for pure vector recall. Proxies \
                 POST /_memory/{namespace}/_recall.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": { "type": "string" },
                    "query": { "type": "string", "description": "Query text (BM25, or embedded when semantic:true)." },
                    "semantic": { "type": "boolean", "description": "Embed `query` server-side and recall by meaning." },
                    "vector": {
                        "type": "array",
                        "items": { "type": "number" },
                        "description": "Query embedding for pure vector recall (takes precedence)."
                    },
                    "k": { "type": "integer", "description": "Number of memories to return (default 10)." },
                    "filter": { "type": "object", "description": "Optional metadata pre-filter (ES query clause)." },
                    "recency_weight": {
                        "type": "number",
                        "description": "Blend relevance with recency in [0,1]; 0=pure relevance, 1=pure recency."
                    }
                },
                "required": ["namespace"]
            }
        }
    ])
}

/// Dispatch a `tools/call` to the matching proxy. Any bad-argument or transport
/// problem is returned as an `isError:true` tool result (not a protocol error),
/// which is the MCP convention for tool-execution failures.
async fn call_tool(ctx: &Ctx, msg: &Value) -> Value {
    let params = match msg.get("params") {
        Some(p) => p,
        None => return tool_text("missing `params`", true),
    };
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return tool_text("missing tool `name`", true),
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let result = match name {
        "xerj_search" => build_search(&args),
        "xerj_semantic_search" => build_semantic(&args),
        "xerj_vector_search" => build_vector(&args),
        "xerj_hybrid_search" => build_hybrid(&args),
        "xerj_memory_store" => build_memory_store(&args),
        "xerj_memory_recall" => build_memory_recall(&args),
        other => return tool_text(format!("unknown tool: {other}"), true),
    };

    match result {
        Ok((path, body)) => engine_post(ctx, &path, body).await,
        Err(msg) => tool_text(msg, true),
    }
}

// ── Argument helpers ────────────────────────────────────────────────────────

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing or empty required string field `{key}`"))
}

/// Copy an optional field from `args` into `body` under the same key, if present.
fn copy_opt(args: &Value, body: &mut serde_json::Map<String, Value>, key: &str) {
    if let Some(v) = args.get(key) {
        if !v.is_null() {
            body.insert(key.to_string(), v.clone());
        }
    }
}

// ── Per-tool request builders → (path, body) ────────────────────────────────

fn build_search(args: &Value) -> Result<(String, Value), String> {
    let index = req_str(args, "index")?;
    let mut body = serde_json::Map::new();
    match args.get("query") {
        Some(q) if !q.is_null() => {
            body.insert("query".into(), q.clone());
        }
        _ => {
            body.insert("query".into(), json!({ "match_all": {} }));
        }
    }
    for k in ["size", "from", "sort", "_source"] {
        copy_opt(args, &mut body, k);
    }
    Ok((format!("/{index}/_search"), Value::Object(body)))
}

fn build_semantic(args: &Value) -> Result<(String, Value), String> {
    let index = req_str(args, "index")?;
    let field = req_str(args, "field")?;
    let query = req_str(args, "query")?;
    let k = args.get("k").and_then(Value::as_u64).unwrap_or(10);

    let mut semantic = json!({ "field": field, "query": query, "k": k });
    if let Some(f) = args.get("filter") {
        if !f.is_null() {
            semantic["filter"] = f.clone();
        }
    }
    // Also cap `size` at k so the response isn't padded past the requested set.
    let body = json!({ "query": { "semantic": semantic }, "size": k });
    Ok((format!("/{index}/_search"), body))
}

fn build_vector(args: &Value) -> Result<(String, Value), String> {
    let index = req_str(args, "index")?;
    let field = req_str(args, "field")?;
    let vector = args
        .get("query_vector")
        .filter(|v| v.is_array())
        .ok_or_else(|| "missing required array field `query_vector`".to_string())?;
    let k = args.get("k").and_then(Value::as_u64).unwrap_or(10);

    let mut knn = serde_json::Map::new();
    knn.insert("field".into(), json!(field));
    knn.insert("query_vector".into(), vector.clone());
    knn.insert("k".into(), json!(k));
    if let Some(nc) = args.get("num_candidates").and_then(Value::as_u64) {
        knn.insert("num_candidates".into(), json!(nc));
    }
    if let Some(f) = args.get("filter") {
        if !f.is_null() {
            knn.insert("filter".into(), f.clone());
        }
    }
    let body = json!({ "knn": Value::Object(knn), "size": k });
    Ok((format!("/{index}/_search"), body))
}

fn build_hybrid(args: &Value) -> Result<(String, Value), String> {
    let index = req_str(args, "index")?;
    let queries = args
        .get("queries")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| "missing or empty required array field `queries`".to_string())?;

    let mut hybrid = serde_json::Map::new();
    hybrid.insert("queries".into(), Value::Array(queries.clone()));
    if let Some(f) = args.get("fusion") {
        if !f.is_null() {
            hybrid.insert("fusion".into(), f.clone());
        }
    }
    let mut body = serde_json::Map::new();
    body.insert("query".into(), json!({ "hybrid": Value::Object(hybrid) }));
    copy_opt(args, &mut body, "size");
    Ok((format!("/{index}/_search"), Value::Object(body)))
}

fn build_memory_store(args: &Value) -> Result<(String, Value), String> {
    let namespace = req_str(args, "namespace")?;
    // `text` is required unless a raw `vector` is supplied; the engine enforces
    // "non-empty text or a vector", so mirror that leniently here.
    let has_text = args
        .get("text")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let has_vector = args.get("vector").map(Value::is_array).unwrap_or(false);
    if !has_text && !has_vector {
        return Err("a memory must have non-empty `text` or a `vector`".to_string());
    }

    let mut body = serde_json::Map::new();
    for k in [
        "text",
        "metadata",
        "id",
        "vector",
        "dedup",
        "dedup_threshold",
    ] {
        copy_opt(args, &mut body, k);
    }
    Ok((format!("/_memory/{namespace}"), Value::Object(body)))
}

fn build_memory_recall(args: &Value) -> Result<(String, Value), String> {
    let namespace = req_str(args, "namespace")?;
    let mut body = serde_json::Map::new();
    for k in [
        "query",
        "vector",
        "semantic",
        "k",
        "filter",
        "recency_weight",
    ] {
        copy_opt(args, &mut body, k);
    }
    Ok((format!("/_memory/{namespace}/_recall"), Value::Object(body)))
}

// ── Engine transport ─────────────────────────────────────────────────────────

/// POST `body` to `path` on the configured XERJ base URL and wrap the response
/// as an MCP tool result. Non-2xx responses (and transport errors) come back as
/// `isError:true` so the agent sees exactly what the engine said.
async fn engine_post(ctx: &Ctx, path: &str, body: Value) -> Value {
    let url = format!("{}{}", ctx.base_url, path);
    let mut req = ctx.client.post(&url).json(&body);
    if let Some(auth) = &ctx.auth {
        req = req.header("Authorization", auth);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                tool_text(text, false)
            } else {
                tool_text(format!("XERJ returned HTTP {status}: {text}"), true)
            }
        }
        Err(e) => tool_text(format!("request to {url} failed: {e}"), true),
    }
}

// ─────────────────────────────── Tests ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_defaults_to_match_all() {
        let (path, body) = build_search(&json!({ "index": "docs" })).unwrap();
        assert_eq!(path, "/docs/_search");
        assert_eq!(body["query"], json!({ "match_all": {} }));
    }

    #[test]
    fn search_passes_query_and_size() {
        let (_, body) = build_search(&json!({
            "index": "docs",
            "query": { "match": { "body": "rust" } },
            "size": 5
        }))
        .unwrap();
        assert_eq!(body["query"]["match"]["body"], "rust");
        assert_eq!(body["size"], 5);
    }

    #[test]
    fn search_requires_index() {
        assert!(build_search(&json!({})).is_err());
    }

    #[test]
    fn semantic_builds_semantic_query_and_caps_size() {
        let (path, body) = build_semantic(&json!({
            "index": "kb", "field": "content", "query": "how to reset", "k": 3
        }))
        .unwrap();
        assert_eq!(path, "/kb/_search");
        assert_eq!(body["query"]["semantic"]["field"], "content");
        assert_eq!(body["query"]["semantic"]["query"], "how to reset");
        assert_eq!(body["query"]["semantic"]["k"], 3);
        assert_eq!(body["size"], 3);
    }

    #[test]
    fn vector_builds_top_level_knn() {
        let (path, body) = build_vector(&json!({
            "index": "emb", "field": "vec", "query_vector": [0.1, 0.2, 0.3], "k": 4
        }))
        .unwrap();
        assert_eq!(path, "/emb/_search");
        assert_eq!(body["knn"]["field"], "vec");
        assert_eq!(body["knn"]["query_vector"], json!([0.1, 0.2, 0.3]));
        assert_eq!(body["knn"]["k"], 4);
        assert_eq!(body["size"], 4);
    }

    #[test]
    fn vector_requires_query_vector() {
        assert!(build_vector(&json!({ "index": "e", "field": "v" })).is_err());
    }

    #[test]
    fn hybrid_wraps_queries_and_fusion() {
        let (path, body) = build_hybrid(&json!({
            "index": "h",
            "queries": [
                { "query": { "match": { "body": "cats" } }, "weight": 1.0 },
                { "query": { "knn": { "field": "v", "query_vector": [0.1], "k": 5 } }, "weight": 0.3 }
            ],
            "fusion": "rrf",
            "size": 10
        }))
        .unwrap();
        assert_eq!(path, "/h/_search");
        assert_eq!(body["query"]["hybrid"]["fusion"], "rrf");
        assert_eq!(
            body["query"]["hybrid"]["queries"].as_array().unwrap().len(),
            2
        );
        assert_eq!(body["size"], 10);
    }

    #[test]
    fn memory_store_requires_text_or_vector() {
        assert!(build_memory_store(&json!({ "namespace": "n" })).is_err());
        let (path, body) =
            build_memory_store(&json!({ "namespace": "n", "text": "remember this" })).unwrap();
        assert_eq!(path, "/_memory/n");
        assert_eq!(body["text"], "remember this");
    }

    #[test]
    fn memory_recall_path_and_passthrough() {
        let (path, body) = build_memory_recall(&json!({
            "namespace": "n", "query": "what did I say", "semantic": true, "k": 5
        }))
        .unwrap();
        assert_eq!(path, "/_memory/n/_recall");
        assert_eq!(body["semantic"], true);
        assert_eq!(body["k"], 5);
    }

    #[test]
    fn initialize_echoes_client_protocol_version() {
        let msg = json!({ "params": { "protocolVersion": "2024-11-05" } });
        let res = initialize_result(&msg);
        assert_eq!(res["protocolVersion"], "2024-11-05");
        assert_eq!(res["serverInfo"]["name"], "xerj-mcp");
    }

    #[test]
    fn tools_list_has_all_six() {
        let specs = tool_specs();
        let names: Vec<&str> = specs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 6);
        for n in [
            "xerj_search",
            "xerj_semantic_search",
            "xerj_vector_search",
            "xerj_hybrid_search",
            "xerj_memory_store",
            "xerj_memory_recall",
        ] {
            assert!(names.contains(&n), "missing tool {n}");
        }
    }
}
