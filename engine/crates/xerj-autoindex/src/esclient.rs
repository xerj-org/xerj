//! Thin blocking ES-compat client (reqwest). Retries with exponential
//! backoff on 429/5xx/transport errors; parses per-item bulk errors.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct Es {
    base: String,
    http: reqwest::blocking::Client,
    api_key: Option<String>,
}

pub struct BulkOutcome {
    pub item_errors: u64,
    /// Per-item 5xx/429 failures are backend/admission failures, not bad source
    /// records. Callers must not journal the source file complete.
    pub server_errors: u64,
    pub first_error: Option<String>,
    pub first_server_error: Option<String>,
}

impl Es {
    pub fn new(url: &str, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .danger_accept_invalid_certs(true)
            .build()
            .context("build http client")?;
        Ok(Es {
            base: url.trim_end_matches('/').to_string(),
            http,
            api_key,
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

    /// Retry wrapper: 429/5xx/transport → backoff 250ms..8s, 6 attempts.
    fn with_retry<T>(
        &self,
        what: &str,
        mut f: impl FnMut() -> Result<reqwest::blocking::Response>,
        parse: impl Fn(reqwest::blocking::Response) -> Result<T>,
    ) -> Result<T> {
        let mut delay = Duration::from_millis(250);
        let mut last_err = None;
        for _ in 0..6 {
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
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(8));
        }
        Err(last_err.unwrap_or_else(|| anyhow!("{what}: retries exhausted")))
    }

    /// PUT index with explicit mapping; tolerates already-exists.
    pub fn ensure_index(&self, index: &str, body: &Value) -> Result<()> {
        let resp = self
            .req(reqwest::Method::PUT, &format!("/{index}"))
            .json(body)
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

    pub fn bulk(&self, body: Vec<u8>) -> Result<BulkOutcome> {
        self.with_retry(
            "_bulk",
            || {
                self.req(reqwest::Method::POST, "/_bulk")
                    .header("Content-Type", "application/x-ndjson")
                    .header("X-Turbo", "1")
                    .body(body.clone())
                    .send()
                    .map_err(|e| anyhow!("bulk send: {e}"))
            },
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
