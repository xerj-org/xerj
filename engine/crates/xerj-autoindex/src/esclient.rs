//! Thin blocking ES-compat client (reqwest). Retries with exponential
//! backoff on 429/5xx/transport errors; parses per-item bulk errors.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct Es {
    base: String,
    http: reqwest::blocking::Client,
    api_key: Option<String>,
    bulk_timeout: Duration,
    retry_initial_delay: Duration,
    retry_max_delay: Duration,
}

pub struct BulkOutcome {
    pub item_errors: u64,
    /// Per-item 5xx/429 failures are backend/admission failures, not bad source
    /// records. Callers must not journal the source file complete.
    pub server_errors: u64,
    pub first_error: Option<String>,
    pub first_server_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingExecutionIdentity {
    pub version: u32,
    pub backend: String,
    pub identity_sha256: String,
    /// Absent for backends whose vector width the server does not pin
    /// (`neural`, `proxy`). An absent width must not be read as 384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    pub semantic_contract: String,
    pub resumable: bool,
    #[serde(default)]
    pub non_resumable_reason: Option<String>,
}

/// Ensure an autoindex index-create body pins the index to a single WAL shard.
///
/// autoindex creates one index per inferred dataset — hundreds for a large
/// repo. Each index otherwise opens one WAL file per ingest shard (a count that
/// scales with the server's CPU cores), so hundreds of indices exhaust the
/// process file-descriptor limit — fatal on macOS, whose default soft limit is
/// 256. `index.xerj_ingest_shards = 1` keeps each index at a single WAL fd.
/// Non-destructive: only fills the key when absent.
fn with_single_wal_shard(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(obj) = body.as_object_mut() {
        let settings = obj
            .entry("settings")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(sobj) = settings.as_object_mut() {
            let index = sobj.entry("index").or_insert_with(|| serde_json::json!({}));
            if let Some(iobj) = index.as_object_mut() {
                iobj.entry("xerj_ingest_shards")
                    .or_insert_with(|| serde_json::json!(1));
            }
        }
    }
    body
}

impl Es {
    pub fn new(url: &str, api_key: Option<String>) -> Result<Self> {
        Self::with_bulk_timeout(url, api_key, 300)
    }

    pub fn with_bulk_timeout(
        url: &str,
        api_key: Option<String>,
        bulk_timeout_secs: u64,
    ) -> Result<Self> {
        Self::with_bulk_policy(
            url,
            api_key,
            Duration::from_secs(bulk_timeout_secs),
            Duration::from_millis(250),
            Duration::from_secs(8),
        )
    }

    fn with_bulk_policy(
        url: &str,
        api_key: Option<String>,
        bulk_timeout: Duration,
        retry_initial_delay: Duration,
        retry_max_delay: Duration,
    ) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(true)
            .build()
            .context("build http client")?;
        Ok(Es {
            base: url.trim_end_matches('/').to_string(),
            http,
            api_key,
            bulk_timeout,
            retry_initial_delay,
            retry_max_delay,
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(k) = &self.api_key {
            r = r.header("Authorization", format!("ApiKey {k}"));
        }
        r
    }

    pub fn ping(&self) -> Result<Value> {
        let resp = self
            .req(reqwest::Method::GET, "/")
            .send()
            .with_context(|| format!("endpoint unreachable: {}", self.base))?;
        Ok(resp.json().unwrap_or(Value::Null))
    }

    pub fn embedding_execution_identity(&self) -> Result<EmbeddingExecutionIdentity> {
        let response = self
            .req(reqwest::Method::GET, "/v1/embedding/identity")
            .send()
            .context("GET /v1/embedding/identity")?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!(
                "GET /v1/embedding/identity failed: HTTP {status}; semantic autoindex requires \
                 a XERJ server that exposes a resumable embedding identity"
            );
        }
        let value: Value = response
            .json()
            .context("parse embedding identity response")?;
        let identity: EmbeddingExecutionIdentity = serde_json::from_value(
            value
                .get("data")
                .cloned()
                .ok_or_else(|| anyhow!("embedding identity response has no data object"))?,
        )
        .context("parse embedding identity")?;
        if identity.version != 1
            || identity.identity_sha256.len() != 64
            || !identity
                .identity_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || identity.dimensions == Some(0)
            || identity.semantic_contract != "semantic_text-derived-vector.v1"
            || !matches!(
                identity.backend.as_str(),
                "lexical" | "neural" | "proxy" | "onnx-experimental"
            )
        {
            anyhow::bail!("server returned an unsupported embedding execution identity");
        }
        Ok(identity)
    }

    /// GET an arbitrary path and return only the HTTP status code.
    ///
    /// One attempt, no retry/backoff: `xerj brain` uses this to probe
    /// whether a server is listening (`/health/ready`, auth-exempt) and
    /// whether its credentials are accepted (`/`), and a liveness probe
    /// that silently retried for ~16s would misreport "already running"
    /// as "boot took 16s". A transport error means nothing answered.
    pub fn get_status(&self, path: &str) -> Result<u16> {
        let resp = self
            .req(reqwest::Method::GET, path)
            .send()
            .with_context(|| format!("no response from {}{}", self.base, path))?;
        Ok(resp.status().as_u16())
    }

    /// Retry wrapper: 429/5xx/transport → backoff 250ms..8s, 6 attempts.
    fn with_retry<T>(
        &self,
        what: &str,
        mut f: impl FnMut() -> Result<reqwest::blocking::Response>,
        parse: impl Fn(reqwest::blocking::Response) -> Result<T>,
    ) -> Result<T> {
        const MAX_ATTEMPTS: usize = 6;
        let mut delay = self.retry_initial_delay;
        let mut last_err = None;
        for attempt in 0..MAX_ATTEMPTS {
            match f() {
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() == 429 || status.is_server_error() {
                        last_err = Some(anyhow!("{what}: HTTP {status}"));
                    } else {
                        return parse(resp);
                    }
                }
                Err(e) => last_err = Some(e),
            }
            if attempt + 1 < MAX_ATTEMPTS {
                std::thread::sleep(delay);
                delay = (delay * 2).min(self.retry_max_delay);
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("{what}: retries exhausted")))
    }

    /// PUT index with explicit mapping; tolerates already-exists.
    pub fn ensure_index(&self, index: &str, body: &Value) -> Result<()> {
        let body = with_single_wal_shard(body);
        let resp = self
            .req(reqwest::Method::PUT, &format!("/{index}"))
            .json(&body)
            .send()
            .context("PUT index")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().unwrap_or_default();
        if text.contains("resource_already_exists")
            || status.as_u16() == 400 && text.contains("exists")
        {
            return Ok(());
        }
        Err(anyhow!("PUT /{index} failed: {status} {text}"))
    }

    /// Additive mapping update for fields introduced after an index was created.
    pub fn update_mapping(&self, index: &str, body: &Value) -> Result<()> {
        let resp = self
            .req(reqwest::Method::PUT, &format!("/{index}/_mapping"))
            .json(body)
            .send()
            .context("PUT mapping")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().unwrap_or_default();
        Err(anyhow!("PUT /{index}/_mapping failed: {status} {text}"))
    }

    pub fn bulk(&self, body: Vec<u8>) -> Result<BulkOutcome> {
        self.with_retry(
            "_bulk",
            || self.send_bulk(body.clone()),
            |resp| {
                let status = resp.status();
                if !status.is_success() {
                    return Err(anyhow!("bulk HTTP {status}"));
                }
                let v: Value = resp.json().context("parse bulk response")?;
                let mut item_errors = 0u64;
                let mut server_errors = 0u64;
                let mut first_error = None;
                let mut first_server_error = None;
                if v.get("errors").and_then(|e| e.as_bool()).unwrap_or(false) {
                    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
                        for it in items {
                            let op = it
                                .get("index")
                                .or_else(|| it.get("create"))
                                .or_else(|| it.get("update"));
                            if let Some(op) = op {
                                if op.get("error").is_some() {
                                    item_errors += 1;
                                    let item_status =
                                        op.get("status").and_then(Value::as_u64).unwrap_or(500);
                                    if item_status == 429 || item_status >= 500 {
                                        server_errors += 1;
                                        if first_server_error.is_none() {
                                            first_server_error = Some(
                                                op["error"].to_string().chars().take(500).collect(),
                                            );
                                        }
                                    }
                                    if first_error.is_none() {
                                        first_error = Some(
                                            op["error"].to_string().chars().take(300).collect(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(BulkOutcome {
                    item_errors,
                    server_errors,
                    first_error,
                    first_server_error,
                })
            },
        )
    }

    fn send_bulk(&self, body: Vec<u8>) -> Result<reqwest::blocking::Response> {
        self.req(reqwest::Method::POST, "/_bulk")
            .timeout(self.bulk_timeout)
            .header("Content-Type", "application/x-ndjson")
            .header("X-Turbo", "1")
            .body(body)
            .send()
            .with_context(|| format!("bulk send (request timeout {:?})", self.bulk_timeout))
    }

    /// Remove every document matching `query` before a file-level replacement.
    /// Refresh is requested so a retry cannot observe or retain an older
    /// locator set alongside the replacement. The server executes ONE bounded
    /// search-and-delete pass per call (size-capped at 10k docs), so a single
    /// response is not complete removal: repeat until a pass deletes nothing.
    pub fn delete_by_query(&self, index: &str, query: &Value) -> Result<()> {
        const MAX_PASSES: usize = 1_000;
        for _ in 0..MAX_PASSES {
            if self.delete_by_query_pass(index, query)? == 0 {
                return Ok(());
            }
        }
        Err(anyhow!(
            "POST /{index}/_delete_by_query still reported deletions after {MAX_PASSES} passes; \
             refusing to treat the previous generation as fully removed"
        ))
    }

    /// One server-side delete pass; returns the reported `deleted` count.
    fn delete_by_query_pass(&self, index: &str, query: &Value) -> Result<u64> {
        self.with_retry(
            "delete_by_query",
            || {
                self.req(
                    reqwest::Method::POST,
                    &format!("/{index}/_delete_by_query?refresh=true"),
                )
                .json(&serde_json::json!({"query": query}))
                .send()
                .map_err(|e| anyhow!("delete_by_query: {e}"))
            },
            |resp| {
                let status = resp.status();
                let body: Value = resp.json().unwrap_or(Value::Null);
                if status.is_success()
                    && body
                        .get("failures")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty)
                {
                    Ok(body.get("deleted").and_then(Value::as_u64).unwrap_or(0))
                } else {
                    Err(anyhow!(
                        "POST /{index}/_delete_by_query failed: HTTP {status}: {body}"
                    ))
                }
            },
        )
    }

    pub fn refresh(&self, pattern: &str) -> Result<()> {
        self.with_retry(
            "refresh",
            || {
                self.req(reqwest::Method::POST, &format!("/{pattern}/_refresh"))
                    .send()
                    .map_err(|e| anyhow!("refresh: {e}"))
            },
            |resp| {
                if resp.status().is_success() {
                    Ok(())
                } else {
                    Err(anyhow!("refresh HTTP {}", resp.status()))
                }
            },
        )
    }

    pub fn search(&self, index: &str, body: &Value) -> Result<Value> {
        self.with_retry(
            "search",
            || {
                self.req(reqwest::Method::POST, &format!("/{index}/_search"))
                    .json(body)
                    .send()
                    .map_err(|e| anyhow!("search: {e}"))
            },
            |resp| {
                let status = resp.status();
                let v: Value = resp.json().unwrap_or(Value::Null);
                if !status.is_success() {
                    return Err(anyhow!("search /{index} HTTP {status}: {v}"));
                }
                Ok(v)
            },
        )
    }

    pub fn count(&self, index: &str) -> Result<u64> {
        // _count may not exist on all builds — use size:0 search with totals.
        let v = self.search(
            index,
            &serde_json::json!({"size": 0, "track_total_hits": true}),
        )?;
        v.pointer("/hits/total/value")
            .and_then(|t| t.as_u64())
            .or_else(|| v.pointer("/hits/total").and_then(|t| t.as_u64()))
            .ok_or_else(|| anyhow!("no total in search response"))
    }

    /// `_cat/indices` is plain text, no header (?format=json is IGNORED —
    /// verified). Returns (index, docs_count) with `.xerj_*` system indices
    /// filtered out.
    pub fn cat_indices(&self) -> Result<Vec<(String, u64)>> {
        let resp = self
            .req(reqwest::Method::GET, "/_cat/indices")
            .send()
            .context("_cat/indices")?;
        let text = resp.text().unwrap_or_default();
        let mut out = Vec::new();
        for line in text.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 3 {
                continue;
            }
            // format: health status index uuid pri rep docs.count deleted size…
            let name = cols[2].to_string();
            if name.starts_with(".xerj") || name.starts_with('.') {
                continue;
            }
            let docs = cols
                .get(6)
                .and_then(|c| c.parse::<u64>().ok())
                // fallback for column-order variants: first integer after the
                // uuid that is not the 1-digit pri/rep pair
                .or_else(|| cols.iter().skip(7).find_map(|c| c.parse::<u64>().ok()))
                .unwrap_or(0);
            out.push((name, docs));
        }
        Ok(out)
    }

    pub fn get_doc(&self, index: &str, id: &str) -> Result<Option<Value>> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/{index}/_doc/{id}"))
            .send()
            .context("GET doc")?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let v: Value = resp.json().unwrap_or(Value::Null);
        if v.get("found").and_then(|f| f.as_bool()).unwrap_or(false) {
            Ok(v.get("_source").cloned())
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Es;
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn bulk_request_uses_custom_timeout_and_drops_timed_out_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).contains("POST /_bulk"));
            std::thread::sleep(Duration::from_millis(1300));
            let closed = stream.read(&mut request).map(|n| n == 0).unwrap_or(true);
            closed_tx.send(closed).unwrap();
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let started = Instant::now();
        let err = es.send_bulk(b"{}\n".to_vec()).unwrap_err();
        let elapsed = started.elapsed();
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert!(elapsed >= Duration::from_millis(800), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
        assert!(closed_rx.recv_timeout(Duration::from_secs(4)).unwrap());
        server.join().unwrap();
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        request
    }

    fn success(stream: &mut std::net::TcpStream) {
        respond_json(stream, br#"{"errors":false,"items":[]}"#);
    }

    fn respond_json(stream: &mut std::net::TcpStream, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    #[test]
    fn embedding_identity_uses_native_endpoint_and_parses_sanitized_data() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(
                String::from_utf8_lossy(&request)
                    .starts_with("GET /v1/embedding/identity HTTP/1.1"),
                "{}",
                String::from_utf8_lossy(&request)
            );
            respond_json(
                &mut stream,
                br#"{"data":{"version":1,"backend":"lexical","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","dimensions":384,"semantic_contract":"semantic_text-derived-vector.v1","resumable":true},"took_ms":0,"request_id":"test"}"#,
            );
        });
        let es = Es::new(&format!("http://{address}"), None).unwrap();
        let identity = es.embedding_execution_identity().unwrap();
        assert_eq!(identity.backend, "lexical");
        assert!(identity.resumable);
        assert_eq!(identity.dimensions, Some(384));
        server.join().unwrap();
    }

    /// `neural` and `proxy` omit the width entirely, so the client must accept
    /// a response without a `dimensions` key rather than failing to parse it.
    /// It still rejects an explicit zero, which no backend should ever send.
    #[test]
    fn embedding_identity_accepts_an_omitted_width_and_rejects_a_zero_one() {
        for (body, expect_ok) in [
            (
                br#"{"data":{"version":1,"backend":"proxy","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","semantic_contract":"semantic_text-derived-vector.v1","resumable":false,"non_resumable_reason":"remote"},"took_ms":0,"request_id":"test"}"#.to_vec(),
                true,
            ),
            (
                br#"{"data":{"version":1,"backend":"lexical","identity_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","dimensions":0,"semantic_contract":"semantic_text-derived-vector.v1","resumable":true},"took_ms":0,"request_id":"test"}"#.to_vec(),
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_request(&mut stream);
                respond_json(&mut stream, &body);
            });
            let es = Es::new(&format!("http://{address}"), None).unwrap();
            let result = es.embedding_execution_identity();
            assert_eq!(result.is_ok(), expect_ok, "{result:?}");
            if let Ok(identity) = result {
                assert_eq!(identity.dimensions, None);
                assert!(!identity.resumable);
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn retry_path_recovers_after_one_bulk_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            read_request(&mut first);
            let first = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(120));
                drop(first);
            });
            let (mut second, _) = listener.accept().unwrap();
            read_request(&mut second);
            success(&mut second);
            first.join().unwrap();
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(50),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let outcome = es.bulk(b"{\"index\":{}}\n{}\n".to_vec()).unwrap();
        assert_eq!(outcome.item_errors, 0);
        server.join().unwrap();
    }

    #[test]
    fn all_timeout_retry_path_is_bounded_to_six_attempts_without_final_sleep() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(Mutex::new(0usize));
        let accepted_server = accepted.clone();
        let server = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                *accepted_server.lock().unwrap() += 1;
                handlers.push(std::thread::spawn(move || {
                    read_request(&mut stream);
                    std::thread::sleep(Duration::from_millis(100));
                }));
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(25),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let started = Instant::now();
        let error = match es.bulk(b"{}\n".to_vec()) {
            Ok(_) => panic!("all six delayed responses unexpectedly succeeded"),
            Err(error) => error,
        };
        let elapsed = started.elapsed();
        assert!(format!("{error:#}").contains("timed out"), "{error:#}");
        assert_eq!(*accepted.lock().unwrap(), 6);
        // Six 25ms attempts plus 10+20+20+20+20ms backoffs. A sixth
        // post-failure sleep would push this past the deliberately tight cap.
        assert!(elapsed < Duration::from_millis(260), "{elapsed:?}");
        server.join().unwrap();
    }

    #[test]
    fn delete_by_query_repeats_until_a_pass_deletes_nothing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_server = requests.clone();
        let server = std::thread::spawn(move || {
            // A previous generation larger than one server pass: the first
            // pass removes its 10k cap and only the second reports done.
            for body in [
                br#"{"deleted":10000,"failures":[]}"#.as_slice(),
                br#"{"deleted":0,"failures":[]}"#.as_slice(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                requests_server
                    .lock()
                    .unwrap()
                    .push(read_request(&mut stream));
                respond_json(&mut stream, body);
            }
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        es.delete_by_query("data", &serde_json::json!({"term": {"ax_file": "key"}}))
            .unwrap();
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let text = String::from_utf8_lossy(request);
            assert!(text.contains("POST /data/_delete_by_query"), "{text}");
        }
    }

    #[test]
    fn delete_by_query_that_never_reaches_zero_fails_at_the_pass_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = 0usize;
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                if request.starts_with(b"STOP") {
                    break;
                }
                respond_json(&mut stream, br#"{"deleted":10000,"failures":[]}"#);
                served += 1;
            }
            served
        });
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        let error = es
            .delete_by_query("data", &serde_json::json!({"term": {"ax_file": "key"}}))
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("still reported deletions"),
            "{error:#}"
        );
        let mut stop = std::net::TcpStream::connect(address).unwrap();
        stop.write_all(b"STOP\r\n\r\n").unwrap();
        drop(stop);
        assert_eq!(server.join().unwrap(), 1_000);
    }

    #[test]
    fn ambiguous_dropped_response_retries_byte_identical_bulk_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_server = requests.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            requests_server
                .lock()
                .unwrap()
                .push(read_request(&mut first));
            drop(first); // backend may have committed, but response is ambiguous
            let (mut second, _) = listener.accept().unwrap();
            requests_server
                .lock()
                .unwrap()
                .push(read_request(&mut second));
            success(&mut second);
        });
        let body = b"{\"index\":{\"_index\":\"i\",\"_id\":\"stable-id\"}}\n{\"v\":1}\n".to_vec();
        let es = Es::with_bulk_policy(
            &format!("http://{address}"),
            None,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();
        es.bulk(body.clone()).unwrap();
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            let offset = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            assert_eq!(&request[offset..], body.as_slice());
        }
    }
}
