//! Real tonic gRPC server for the `XerjSearch` service (default `:8081`).
//!
//! Replaces the v0.1 placeholder (which only bound the port and dropped
//! connections). The service is a thin adapter over the same `Engine`/`Index`
//! methods the REST and ES-compat listeners use, so gRPC clients get identical
//! semantics. Wire schema: `proto/xerj.proto` (package `xerj.v1`), compiled to
//! Rust by `build.rs` (pure-Rust `protox`, no host `protoc`).
//!
//! The listener speaks plaintext HTTP/2 (h2c). TLS termination stays with the
//! REST/ES listeners (axum-server + ring); tonic is built without its `tls`
//! feature so no second crypto backend is dragged in.

use std::net::SocketAddr;

use anyhow::Context;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

use xerj_api::AppState;
use xerj_query::parse_request;

/// Generated prost messages + tonic client/server stubs for `xerj.v1`.
///
/// The generated code is exempted from lints — it is machine-emitted and not
/// meant to satisfy this crate's `-D warnings` clippy gate.
#[allow(clippy::all, clippy::pedantic, clippy::nursery, missing_docs)]
pub mod pb {
    tonic::include_proto!("xerj.v1");
}

use pb::xerj_search_server::{XerjSearch, XerjSearchServer};

/// gRPC adapter over the engine. Clone is cheap — `AppState` is all `Arc`s.
#[derive(Clone)]
pub struct GrpcService {
    state: AppState,
}

impl GrpcService {
    /// Build the service from shared server state.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl XerjSearch for GrpcService {
    async fn search(
        &self,
        request: Request<pb::SearchRequest>,
    ) -> Result<Response<pb::SearchResponse>, Status> {
        let req = request.into_inner();

        let idx = self
            .state
            .engine
            .get_index(&req.index)
            .map_err(|e| Status::not_found(e.to_string()))?;

        // `query_json` is the ES query DSL. We accept both a bare query clause
        // (e.g. `{"match":{...}}`) and a full request body (`{"query":{...},
        // "size":...}`); an empty string means match_all. The dedicated
        // `size`/`from` fields override the body when set (non-zero).
        let mut body: serde_json::Value = if req.query_json.trim().is_empty() {
            serde_json::json!({ "query": { "match_all": {} } })
        } else {
            serde_json::from_str(&req.query_json).map_err(|e| {
                Status::invalid_argument(format!("query_json is not valid JSON: {e}"))
            })?
        };
        if body.get("query").is_none() {
            body = serde_json::json!({ "query": body });
        }
        if req.size != 0 {
            body["size"] = serde_json::json!(req.size);
        }
        if req.from != 0 {
            body["from"] = serde_json::json!(req.from);
        }

        let search_req = parse_request(&body)
            .map_err(|e| Status::invalid_argument(format!("invalid query: {e}")))?;

        let started = std::time::Instant::now();
        let result = idx
            .search(&search_req)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let took_ms = started.elapsed().as_millis() as i64;

        let hits = result
            .hits
            .iter()
            .map(|h| pb::Hit {
                id: h.id.clone(),
                score: h.score,
                source_json: h.source.to_string(),
                index: req.index.clone(),
            })
            .collect();

        Ok(Response::new(pb::SearchResponse {
            total_hits: result.total.value as i64,
            hits,
            took_ms,
            timed_out: result.timed_out,
        }))
    }

    async fn index(
        &self,
        request: Request<pb::IndexRequest>,
    ) -> Result<Response<pb::IndexResponse>, Status> {
        let req = request.into_inner();

        // Index-on-write: ES auto-creates the index on first document.
        let idx = self
            .state
            .engine
            .get_or_create_index(&req.index)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let source: serde_json::Value = serde_json::from_str(&req.source_json)
            .map_err(|e| Status::invalid_argument(format!("source_json is not valid JSON: {e}")))?;

        let id = if req.id.is_empty() {
            None
        } else {
            Some(req.id.clone())
        };

        let resp = idx
            .index_document(id, source)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        self.state.metrics.record_doc_indexed(&req.index);

        Ok(Response::new(pb::IndexResponse {
            id: resp.id,
            version: resp.version as i64,
            result: resp.result,
        }))
    }

    async fn bulk_index(
        &self,
        request: Request<Streaming<pb::IndexRequest>>,
    ) -> Result<Response<pb::BulkResponse>, Status> {
        let mut stream = request.into_inner();
        let started = std::time::Instant::now();
        let mut indexed = 0i32;
        let mut errors = false;

        while let Some(item) = stream.message().await? {
            let idx = match self.state.engine.get_or_create_index(&item.index) {
                Ok(i) => i,
                Err(_) => {
                    errors = true;
                    continue;
                }
            };
            let source: serde_json::Value = match serde_json::from_str(&item.source_json) {
                Ok(v) => v,
                Err(_) => {
                    errors = true;
                    continue;
                }
            };
            let id = if item.id.is_empty() {
                None
            } else {
                Some(item.id)
            };
            if idx.index_document(id, source).await.is_ok() {
                indexed += 1;
                self.state.metrics.record_doc_indexed(&item.index);
            } else {
                errors = true;
            }
        }

        Ok(Response::new(pb::BulkResponse {
            took_ms: started.elapsed().as_millis() as i64,
            errors,
            indexed,
        }))
    }

    async fn get_document(
        &self,
        request: Request<pb::GetRequest>,
    ) -> Result<Response<pb::GetResponse>, Status> {
        let req = request.into_inner();

        // Unknown index → "not found" (ES GET semantics), not an error.
        let idx = match self.state.engine.get_index(&req.index) {
            Ok(i) => i,
            Err(_) => {
                return Ok(Response::new(pb::GetResponse {
                    found: false,
                    id: req.id,
                    source_json: String::new(),
                    version: 0,
                }));
            }
        };

        match idx
            .get_document(&req.id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
        {
            Some(source) => Ok(Response::new(pb::GetResponse {
                found: true,
                id: req.id,
                source_json: source.to_string(),
                version: 1,
            })),
            None => Ok(Response::new(pb::GetResponse {
                found: false,
                id: req.id,
                source_json: String::new(),
                version: 0,
            })),
        }
    }

    async fn delete_document(
        &self,
        request: Request<pb::DeleteRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let req = request.into_inner();

        let idx = self
            .state
            .engine
            .get_index(&req.index)
            .map_err(|e| Status::not_found(e.to_string()))?;

        let existed = idx
            .delete_document(&req.id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(pb::DeleteResponse {
            result: if existed { "deleted" } else { "not_found" }.to_string(),
        }))
    }

    async fn health(
        &self,
        _request: Request<pb::HealthRequest>,
    ) -> Result<Response<pb::HealthResponse>, Status> {
        let health = self.state.engine.health().await;
        Ok(Response::new(pb::HealthResponse {
            status: health.status,
            num_indices: health.index_count as i32,
            total_docs: health.total_docs as i64,
        }))
    }
}

/// Serve the `XerjSearch` gRPC service on `addr` until `shutdown` resolves.
///
/// Returns `Err` if the port cannot be bound or the transport fails; callers
/// log-and-continue so a gRPC bind failure never takes the whole server down.
pub async fn serve_grpc<F>(addr: SocketAddr, state: AppState, shutdown: F) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let svc = XerjSearchServer::new(GrpcService::new(state));
    info!("gRPC XerjSearch listening on {addr}");
    tonic::transport::Server::builder()
        .add_service(svc)
        .serve_with_shutdown(addr, shutdown)
        .await
        .context("gRPC transport error")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pb::xerj_search_client::XerjSearchClient;
    use super::*;
    use tempfile::TempDir;
    use xerj_common::config::Config;
    use xerj_common::metrics::Metrics;
    use xerj_engine::Engine;

    fn app_state(dir: &TempDir) -> AppState {
        let mut config = Config::default();
        config.server.data_dir = dir.path().to_str().unwrap().to_string();
        let metrics = Metrics::new().expect("metrics init");
        let engine = Engine::new(config.clone()).expect("engine init");
        AppState::new(config, engine, metrics)
    }

    /// Grab an ephemeral port by binding then dropping a std listener.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn connect(addr: &str) -> XerjSearchClient<tonic::transport::Channel> {
        // The server task needs a moment to start listening; retry briefly.
        for _ in 0..50 {
            if let Ok(c) = XerjSearchClient::connect(addr.to_string()).await {
                return c;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("gRPC client could not connect to {addr}");
    }

    #[tokio::test]
    async fn grpc_health_index_get_search_roundtrip() {
        let dir = TempDir::new().unwrap();
        let state = app_state(&dir);

        let port = free_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(serve_grpc(addr, state, async move {
            let _ = rx.await;
        }));

        let url = format!("http://127.0.0.1:{port}");
        let mut client = connect(&url).await;

        // ── Health ───────────────────────────────────────────────────────
        let health = client
            .health(pb::HealthRequest {})
            .await
            .expect("health rpc")
            .into_inner();
        assert!(!health.status.is_empty(), "health status must be set");

        // ── Index → GetDocument round-trip ───────────────────────────────
        let indexed = client
            .index(pb::IndexRequest {
                index: "grpc_test".into(),
                id: "doc1".into(),
                source_json: r#"{"title":"hello grpc","n":7}"#.into(),
            })
            .await
            .expect("index rpc")
            .into_inner();
        assert_eq!(indexed.id, "doc1");
        assert_eq!(indexed.result, "created");

        let got = client
            .get_document(pb::GetRequest {
                index: "grpc_test".into(),
                id: "doc1".into(),
            })
            .await
            .expect("get rpc")
            .into_inner();
        assert!(got.found, "indexed document must be found");
        let source: serde_json::Value = serde_json::from_str(&got.source_json).unwrap();
        assert_eq!(source["title"], "hello grpc");

        // A missing document must report not-found, not error.
        let missing = client
            .get_document(pb::GetRequest {
                index: "grpc_test".into(),
                id: "nope".into(),
            })
            .await
            .expect("get rpc (missing)")
            .into_inner();
        assert!(!missing.found);

        // ── Search must see the freshly indexed (memtable) doc ───────────
        let search = client
            .search(pb::SearchRequest {
                index: "grpc_test".into(),
                query_json: r#"{"match":{"title":"hello"}}"#.into(),
                size: 10,
                from: 0,
            })
            .await
            .expect("search rpc")
            .into_inner();
        assert!(
            search.total_hits >= 1,
            "search should match the indexed doc, got {}",
            search.total_hits
        );
        assert_eq!(search.hits.first().map(|h| h.id.as_str()), Some("doc1"));

        // ── Delete ───────────────────────────────────────────────────────
        let deleted = client
            .delete_document(pb::DeleteRequest {
                index: "grpc_test".into(),
                id: "doc1".into(),
            })
            .await
            .expect("delete rpc")
            .into_inner();
        assert_eq!(deleted.result, "deleted");

        // Shut the server down cleanly.
        let _ = tx.send(());
        let _ = server.await;
    }
}
