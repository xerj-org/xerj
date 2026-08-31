//! Zero-overhead binary protocol for xerj.
//!
//! # Wire format
//!
//! Every message (request and response) uses a fixed 5-byte header followed by
//! a variable-length JSON payload:
//!
//! ```text
//! ┌──────────────────────┬───────────┬──────────────────────────────────┐
//! │  length  (4 bytes LE)│ op (1 byte│ payload  (length bytes, UTF-8 JSON)│
//! └──────────────────────┴───────────┴──────────────────────────────────┘
//! ```
//!
//! * **length** — `u32` little-endian: byte length of the payload that follows.
//! * **op** — one of the [`Op`] constants; determines how the server interprets
//!   the payload.
//! * **payload** — JSON-encoded request or response body (no trailing newline).
//!
//! # Operation codes
//!
//! | Op | Value | Direction | Description |
//! |----|-------|-----------|-------------|
//! | SEARCH | 1 | client→server | Full-text / structured search |
//! | INDEX  | 2 | client→server | Index a single document |
//! | GET    | 3 | client→server | Retrieve a document by ID |
//! | DELETE | 4 | client→server | Delete a document by ID |
//! | BULK   | 5 | client→server | Batch-index multiple documents |
//! | HEALTH | 6 | client→server | Cluster health probe |
//! | RESP_OK  | 64 | server→client | Success response |
//! | RESP_ERR | 65 | server→client | Error response |
//!
//! # Example session
//!
//! ```text
//! client  → [0x0E 0x00 0x00 0x00] [0x06] {}                 (HEALTH, 14-byte payload)
//! server  → [0x31 0x00 0x00 0x00] [0x40] {"status":"green"} (RESP_OK)
//! ```

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use xerj_query::parse_request;

use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Op codes
// ─────────────────────────────────────────────────────────────────────────────

/// Op-code constants for the xerj binary protocol.
pub mod op {
    pub const SEARCH: u8 = 1;
    pub const INDEX: u8 = 2;
    pub const GET: u8 = 3;
    pub const DELETE: u8 = 4;
    pub const BULK: u8 = 5;
    pub const HEALTH: u8 = 6;
    pub const RESP_OK: u8 = 64;
    pub const RESP_ERR: u8 = 65;
}

// ─────────────────────────────────────────────────────────────────────────────
// Request / response payload types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BinSearchRequest {
    index: String,
    #[serde(default)]
    query: Option<Value>,
    #[serde(default = "default_size")]
    size: usize,
    #[serde(default)]
    from: usize,
}

fn default_size() -> usize {
    10
}

#[derive(Debug, Deserialize)]
struct BinIndexRequest {
    index: String,
    #[serde(default)]
    id: Option<String>,
    source: Value,
}

#[derive(Debug, Deserialize)]
struct BinGetRequest {
    index: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct BinDeleteRequest {
    index: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct BinBulkRequest {
    index: String,
    docs: Vec<Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Frame I/O helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Read a single framed message: `[u32-LE length][u8 op][payload]`.
///
/// Returns `(op, payload_bytes)` or an I/O error.
async fn read_frame(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header).await?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let op = header[4];

    let mut payload = vec![0u8; length];
    if length > 0 {
        stream.read_exact(&mut payload).await?;
    }
    Ok((op, payload))
}

/// Write a framed response: `[u32-LE length][u8 op][payload]`.
async fn write_frame(stream: &mut TcpStream, op: u8, payload: &[u8]) -> std::io::Result<()> {
    let length = payload.len() as u32;
    let mut header = [0u8; 5];
    header[..4].copy_from_slice(&length.to_le_bytes());
    header[4] = op;
    stream.write_all(&header).await?;
    if !payload.is_empty() {
        stream.write_all(payload).await?;
    }
    stream.flush().await?;
    Ok(())
}

/// Serialize `value` and write it as an `RESP_OK` frame.
async fn respond_ok(stream: &mut TcpStream, value: &impl Serialize) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    write_frame(stream, op::RESP_OK, &payload).await
}

/// Write a JSON error string as a `RESP_ERR` frame.
async fn respond_err(stream: &mut TcpStream, msg: &str) -> std::io::Result<()> {
    let payload = serde_json::to_vec(&serde_json::json!({ "error": msg }))
        .unwrap_or_else(|_| b"{\"error\":\"unknown\"}".to_vec());
    write_frame(stream, op::RESP_ERR, &payload).await
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-connection handler
// ─────────────────────────────────────────────────────────────────────────────

async fn handle_connection(mut stream: TcpStream, state: Arc<AppState>) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    debug!(peer = peer.as_str(), "binary protocol: new connection");

    loop {
        let (op_code, payload) = match read_frame(&mut stream).await {
            Ok(frame) => frame,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                debug!(peer = peer.as_str(), "binary protocol: client disconnected");
                break;
            }
            Err(e) => {
                warn!(peer = peer.as_str(), error = %e, "binary protocol: read error");
                break;
            }
        };

        let result = dispatch(&mut stream, op_code, &payload, &state).await;
        if let Err(e) = result {
            error!(peer = peer.as_str(), error = %e, "binary protocol: write error");
            break;
        }
    }
}

/// Dispatch a single request frame to the appropriate handler and write the
/// response frame back to the stream.
async fn dispatch(
    stream: &mut TcpStream,
    op_code: u8,
    payload: &[u8],
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    match op_code {
        op::SEARCH => handle_search(stream, payload, state).await,
        op::INDEX => handle_index(stream, payload, state).await,
        op::GET => handle_get(stream, payload, state).await,
        op::DELETE => handle_delete(stream, payload, state).await,
        op::BULK => handle_bulk(stream, payload, state).await,
        op::HEALTH => handle_health(stream, state).await,
        unknown => {
            warn!(op = unknown, "binary protocol: unknown op code");
            respond_err(stream, &format!("unknown op code: {unknown}")).await
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Individual op handlers
// ─────────────────────────────────────────────────────────────────────────────

async fn handle_search(
    stream: &mut TcpStream,
    payload: &[u8],
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    let req: BinSearchRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return respond_err(stream, &format!("bad search request: {e}")).await,
    };

    let idx = match state.engine.get_index(&req.index) {
        Ok(i) => i,
        Err(e) => return respond_err(stream, &e.to_string()).await,
    };

    let query_body = if let Some(q) = req.query {
        serde_json::json!({ "query": q, "from": req.from, "size": req.size })
    } else {
        serde_json::json!({ "query": { "match_all": {} }, "from": req.from, "size": req.size })
    };

    let search_req = match parse_request(&query_body) {
        Ok(r) => r,
        Err(e) => return respond_err(stream, &format!("invalid query: {e}")).await,
    };

    let started = std::time::Instant::now();
    match idx.search(&search_req).await {
        Ok(result) => {
            let took_ms = started.elapsed().as_millis() as i64;
            let hits: Vec<Value> = result
                .hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "id": h.id,
                        "score": h.score,
                        "source": h.source,
                        "index": req.index,
                    })
                })
                .collect();
            let resp = serde_json::json!({
                "total_hits": result.total.value,
                "hits": hits,
                "took_ms": took_ms,
                "timed_out": false,
            });
            respond_ok(stream, &resp).await
        }
        Err(e) => respond_err(stream, &e.to_string()).await,
    }
}

async fn handle_index(
    stream: &mut TcpStream,
    payload: &[u8],
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    let req: BinIndexRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return respond_err(stream, &format!("bad index request: {e}")).await,
    };

    let idx = match state.engine.get_index(&req.index) {
        Ok(i) => i,
        Err(e) => return respond_err(stream, &e.to_string()).await,
    };

    let id = req.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    match idx.index_document(Some(id.clone()), req.source).await {
        Ok(r) => {
            state.metrics.record_doc_indexed(&req.index);
            let resp = serde_json::json!({
                "id": r.id,
                "version": r.seq_no as i64,
                "result": r.result,
            });
            respond_ok(stream, &resp).await
        }
        Err(e) => respond_err(stream, &e.to_string()).await,
    }
}

async fn handle_get(
    stream: &mut TcpStream,
    payload: &[u8],
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    let req: BinGetRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return respond_err(stream, &format!("bad get request: {e}")).await,
    };

    let idx = match state.engine.get_index(&req.index) {
        Ok(i) => i,
        Err(e) => return respond_err(stream, &e.to_string()).await,
    };

    match idx.get_document(&req.id).await {
        Ok(Some(source)) => {
            let resp = serde_json::json!({
                "found": true,
                "id": req.id,
                "source_json": source.to_string(),
                "version": 1i64,
            });
            respond_ok(stream, &resp).await
        }
        Ok(None) => {
            let resp = serde_json::json!({
                "found": false,
                "id": req.id,
                "source_json": "",
                "version": 0i64,
            });
            respond_ok(stream, &resp).await
        }
        Err(e) => respond_err(stream, &e.to_string()).await,
    }
}

async fn handle_delete(
    stream: &mut TcpStream,
    payload: &[u8],
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    let req: BinDeleteRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return respond_err(stream, &format!("bad delete request: {e}")).await,
    };

    let idx = match state.engine.get_index(&req.index) {
        Ok(i) => i,
        Err(e) => return respond_err(stream, &e.to_string()).await,
    };

    match idx.delete_document(&req.id).await {
        Ok(_) => {
            let resp = serde_json::json!({ "result": "deleted" });
            respond_ok(stream, &resp).await
        }
        Err(e) => respond_err(stream, &e.to_string()).await,
    }
}

async fn handle_bulk(
    stream: &mut TcpStream,
    payload: &[u8],
    state: &Arc<AppState>,
) -> std::io::Result<()> {
    let req: BinBulkRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return respond_err(stream, &format!("bad bulk request: {e}")).await,
    };

    let idx = match state.engine.get_index(&req.index) {
        Ok(i) => i,
        Err(e) => return respond_err(stream, &e.to_string()).await,
    };

    let started = std::time::Instant::now();
    let total = req.docs.len() as i32;
    let mut indexed = 0i32;
    let mut errors = false;

    // One embedding pass for the whole batch, not one per document. The
    // engine's `index_documents_batched` routes the batch through the same
    // `apply_semantic_embeddings_batch` entry the HTTP `_bulk` path uses, so
    // both bulk entry points share one batching rule; publication is still
    // per document and per-document failures stay isolated (#903).
    let docs: Vec<(Option<String>, Value)> = req
        .docs
        .into_iter()
        .map(|doc| {
            let id = doc
                .get("_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            (Some(id), doc)
        })
        .collect();

    for result in idx.index_documents_batched(docs).await {
        if result.is_ok() {
            indexed += 1;
            state.metrics.record_doc_indexed(&req.index);
        } else {
            errors = true;
        }
    }

    let took_ms = started.elapsed().as_millis() as i64;
    let resp = serde_json::json!({
        "took_ms": took_ms,
        "errors": errors,
        "indexed": indexed,
        "total": total,
    });
    respond_ok(stream, &resp).await
}

async fn handle_health(stream: &mut TcpStream, state: &Arc<AppState>) -> std::io::Result<()> {
    let health = state.engine.health().await;
    let resp = serde_json::json!({
        "status": health.status,
        "num_indices": health.index_count as i32,
        "total_docs": health.total_docs as i64,
    });
    respond_ok(stream, &resp).await
}

// ─────────────────────────────────────────────────────────────────────────────
// Server entry-point
// ─────────────────────────────────────────────────────────────────────────────

/// Start the binary protocol TCP server on `addr`.
///
/// Each connection is handled in its own Tokio task; the server runs until
/// the returned future is dropped or a fatal error occurs.
///
/// # Example
///
/// ```no_run
/// # use std::sync::Arc;
/// # use xerj_api::binary_protocol::serve_binary_protocol;
/// # use xerj_api::state::AppState;
/// # async fn run(state: Arc<AppState>) {
/// serve_binary_protocol("0.0.0.0:8081".parse().unwrap(), state).await.unwrap();
/// # }
/// ```
pub async fn serve_binary_protocol(
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!(addr = %addr, "binary protocol: listening");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                error!(error = %e, "binary protocol: accept error");
                continue;
            }
        };
        debug!(peer = %peer, "binary protocol: accepted connection");
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            handle_connection(stream, state).await;
        });
    }
}

#[cfg(test)]
mod tests {
    //! #903: the binary `_bulk` handler must not embed one document per
    //! inference call.
    //!
    //! The backend here is an OpenAI-compatible proxy served on an ephemeral
    //! local port, because `EmbeddingProxy::embed_batch` issues exactly one
    //! HTTP POST per call — so the request count *is* the inference-call
    //! count, with no model download and no timing noise. The default lexical
    //! embedder is deliberately not used: it is not an active backend, it runs
    //! no batched inference, and a test against it would prove nothing here.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use xerj_common::types::{FieldConfig, FieldType, Schema};

    use super::*;

    const DIMS: usize = 8;

    /// What the embedding backend was actually asked to do.
    struct BackendCalls {
        calls: Arc<AtomicUsize>,
        widest_call: Arc<AtomicUsize>,
        texts: Arc<AtomicUsize>,
    }

    /// A mock OpenAI-compatible `/v1/embeddings` endpoint on an ephemeral port.
    async fn mock_embedding_backend() -> (String, BackendCalls) {
        let calls = Arc::new(AtomicUsize::new(0));
        let widest_call = Arc::new(AtomicUsize::new(0));
        let texts = Arc::new(AtomicUsize::new(0));
        let (c, w, t) = (
            Arc::clone(&calls),
            Arc::clone(&widest_call),
            Arc::clone(&texts),
        );
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let (c, w, t) = (Arc::clone(&c), Arc::clone(&w), Arc::clone(&t));
                async move {
                    let n = body
                        .get("input")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    c.fetch_add(1, Ordering::SeqCst);
                    t.fetch_add(n, Ordering::SeqCst);
                    w.fetch_max(n, Ordering::SeqCst);
                    let data: Vec<Value> = (0..n)
                        .map(|i| {
                            let embedding: Vec<f64> = (0..DIMS)
                                .map(|d| ((i + d) % 7) as f64 / 7.0 + 0.1)
                                .collect();
                            json!({ "index": i, "embedding": embedding })
                        })
                        .collect();
                    axum::Json(json!({ "data": data }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock backend");
        let addr = listener.local_addr().expect("mock backend addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (
            format!("http://{addr}/v1/embeddings"),
            BackendCalls {
                calls,
                widest_call,
                texts,
            },
        )
    }

    /// An index with one `semantic_text`-shaped field, served by the binary
    /// protocol on an ephemeral port. The temp dir must outlive the engine.
    async fn node(endpoint: String) -> (std::net::SocketAddr, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = xerj_common::config::Config::default();
        config.server.data_dir = dir.path().to_string_lossy().into_owned();
        config.storage.wal_sync = xerj_common::config::WalSync::Async;
        config.embedding.mode = "proxy".into();
        config.embedding.default_endpoint = endpoint;
        config.embedding.default_model = "mock-embed".into();

        let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
        let engine = xerj_engine::Engine::new(config.clone()).expect("engine");

        let mut schema = Schema::empty();
        let mut body = FieldConfig::new("body", FieldType::Text);
        body.options.dimensions = Some(DIMS);
        body.options.similarity = Some("cosine".into());
        body.embedding = Some(xerj_common::types::EmbeddingConfig {
            endpoint: None,
            model: None,
            target_field: Some("body_vector".into()),
        });
        schema.add_field(body).expect("semantic field");
        let mut companion = FieldConfig::new("body_vector", FieldType::Vector);
        companion.options.dimensions = Some(DIMS);
        companion.options.similarity = Some("cosine".into());
        schema.add_field(companion).expect("companion field");
        engine.create_index("notes", schema).expect("create index");

        let state = Arc::new(crate::state::AppState::new(config, engine, metrics));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind binary protocol");
        let addr = listener.local_addr().expect("binary protocol addr");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(handle_connection(stream, Arc::clone(&state)));
            }
        });
        (addr, dir)
    }

    /// One framed request/response round trip on a fresh connection.
    async fn round_trip(addr: std::net::SocketAddr, op_code: u8, payload: Value) -> Value {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let body = serde_json::to_vec(&payload).expect("serialize");
        let mut header = [0u8; 5];
        header[..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
        header[4] = op_code;
        stream.write_all(&header).await.expect("write header");
        stream.write_all(&body).await.expect("write payload");
        stream.flush().await.expect("flush");

        let mut resp_header = [0u8; 5];
        stream
            .read_exact(&mut resp_header)
            .await
            .expect("read header");
        let length = u32::from_le_bytes([
            resp_header[0],
            resp_header[1],
            resp_header[2],
            resp_header[3],
        ]) as usize;
        let mut resp = vec![0u8; length];
        stream.read_exact(&mut resp).await.expect("read payload");
        assert_eq!(
            resp_header[4],
            op::RESP_OK,
            "response: {}",
            String::from_utf8_lossy(&resp)
        );
        serde_json::from_slice(&resp).expect("response json")
    }

    /// #903: a bulk of N single-passage documents must reach the embedding
    /// backend as ONE inference call carrying all N passages, not N calls of
    /// one passage each. Before the fix `handle_bulk` looped `index_document`
    /// per document and this assertion saw 12 calls of width 1.
    #[tokio::test]
    async fn binary_bulk_embeds_the_whole_batch_in_one_inference_call() {
        let (endpoint, backend) = mock_embedding_backend().await;
        let (addr, _dir) = node(endpoint).await;

        const DOCS: usize = 12;
        let docs: Vec<Value> = (0..DOCS)
            .map(|i| json!({ "_id": format!("d{i}"), "body": format!("passage number {i}") }))
            .collect();
        let response = round_trip(addr, op::BULK, json!({ "index": "notes", "docs": docs })).await;

        assert_eq!(
            response["errors"],
            json!(false),
            "bulk response: {response}"
        );
        assert_eq!(
            response["indexed"],
            json!(DOCS as i64),
            "bulk response: {response}"
        );
        assert_eq!(
            backend.texts.load(Ordering::SeqCst),
            DOCS,
            "every document's passage must still be embedded exactly once"
        );
        assert_eq!(
            backend.widest_call.load(Ordering::SeqCst),
            DOCS,
            "the whole batch must share one inference window"
        );
        assert_eq!(
            backend.calls.load(Ordering::SeqCst),
            1,
            "{DOCS} single-passage documents must cost 1 inference call, not one per document"
        );
    }

    /// Batching must not widen a single document's failure into the batch's:
    /// publication stays per document, and the response still counts only what
    /// was actually indexed.
    #[tokio::test]
    async fn a_rejected_document_does_not_fail_its_neighbours() {
        let (endpoint, backend) = mock_embedding_backend().await;
        let (addr, _dir) = node(endpoint).await;

        // `body_vector` is the engine-owned target of the semantic field, so
        // hand-supplied passage metadata for it is rejected per document.
        let metadata_field = format!(
            "{}body_vector",
            xerj_query::executor::PASSAGE_METADATA_PREFIX
        );
        let docs = vec![
            json!({ "_id": "ok-1", "body": "first passage" }),
            json!({
                "_id": "bad",
                "body": "second passage",
                metadata_field.clone(): { "field": "body", "chunks": [[0, 3]] }
            }),
            json!({ "_id": "ok-2", "body": "third passage" }),
        ];
        let response = round_trip(addr, op::BULK, json!({ "index": "notes", "docs": docs })).await;

        assert_eq!(response["total"], json!(3), "bulk response: {response}");
        assert_eq!(response["indexed"], json!(2), "bulk response: {response}");
        assert_eq!(response["errors"], json!(true), "bulk response: {response}");
        assert_eq!(
            backend.calls.load(Ordering::SeqCst),
            1,
            "the surviving documents still share one inference call"
        );

        let found = round_trip(addr, op::GET, json!({ "index": "notes", "id": "ok-2" })).await;
        assert_eq!(found["found"], json!(true), "get response: {found}");
        let missing = round_trip(addr, op::GET, json!({ "index": "notes", "id": "bad" })).await;
        assert_eq!(missing["found"], json!(false), "get response: {missing}");
    }
}
