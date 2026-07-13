//! Embedding proxy — async HTTP client for OpenAI-compatible embedding APIs.
//!
//! Supports:
//! - Batch embedding with configurable model
//! - Rate limiting (token bucket)
//! - Retry with exponential backoff
//! - Configurable timeout

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::{debug, warn};
use xerj_common::XerjError;

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the embedding proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingProxyConfig {
    /// API endpoint URL (e.g., `https://api.openai.com/v1/embeddings`).
    pub endpoint: String,
    /// API key (sent as `Authorization: Bearer <key>`).
    pub api_key: Option<String>,
    /// Default embedding model name.
    pub model: String,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Maximum concurrent in-flight requests.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Maximum number of retries on transient failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_timeout_secs() -> u64 {
    30
}
fn default_max_concurrent() -> usize {
    4
}
fn default_max_retries() -> u32 {
    3
}

impl EmbeddingProxyConfig {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: None,
            model: model.into(),
            timeout_secs: default_timeout_secs(),
            max_concurrent: default_max_concurrent(),
            max_retries: default_max_retries(),
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    input: &'a [String],
    model: &'a str,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Debug, Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
    index: usize,
}

/// A single embedding attempt's failure, tagged with whether a retry could
/// plausibly succeed (item 9). Only `retryable` failures are re-attempted by
/// [`EmbeddingProxy::send_with_retry`]; a permanent failure (4xx client error,
/// contract violation) returns immediately instead of stalling ~2 min/doc.
struct EmbedFailure {
    err: XerjError,
    retryable: bool,
}

impl EmbedFailure {
    fn transient(err: XerjError) -> Self {
        Self {
            err,
            retryable: true,
        }
    }
    fn permanent(err: XerjError) -> Self {
        Self {
            err,
            retryable: false,
        }
    }
}

/// Whether an upstream HTTP status warrants a retry (item 9). Server errors
/// (5xx), rate-limit (429), and request-timeout (408) are transient; all
/// other 4xx client errors are permanent (fail fast).
fn status_is_retryable(code: u16) -> bool {
    code >= 500 || code == 429 || code == 408
}

// ─────────────────────────────────────────────────────────────────────────────
// EmbeddingProxy
// ─────────────────────────────────────────────────────────────────────────────

/// Async HTTP embedding proxy.
///
/// Thread-safe — clone-share freely between tasks.
#[derive(Clone)]
pub struct EmbeddingProxy {
    config: EmbeddingProxyConfig,
    client: reqwest::Client,
    /// Concurrency limiter.
    semaphore: Arc<Semaphore>,
}

impl EmbeddingProxy {
    /// Create a new proxy with the given config.
    pub fn new(config: EmbeddingProxyConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| XerjError::embedding(format!("HTTP client init: {e}")))?;

        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));

        Ok(Self {
            config,
            client,
            semaphore,
        })
    }

    /// Embed a batch of texts using the configured model.
    ///
    /// Returns one embedding vector per input text, in the same order.
    pub async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.embed_batch_with_model(texts, &self.config.model.clone())
            .await
    }

    /// Embed texts using a specific model (overrides the config default).
    pub async fn embed_batch_with_model(
        &self,
        texts: Vec<String>,
        model: &str,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| XerjError::embedding(format!("semaphore: {e}")))?;

        let result = self.send_with_retry(&texts, model).await?;
        Ok(result)
    }

    async fn send_with_retry(&self, texts: &[String], model: &str) -> Result<Vec<Vec<f32>>> {
        let mut last_err = XerjError::embedding("no attempts made");
        let mut backoff = Duration::from_millis(200);

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                warn!("embed retry {}/{}", attempt, self.config.max_retries);
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }

            match self.send_once(texts, model).await {
                Ok(result) => return Ok(result),
                Err(EmbedFailure {
                    err,
                    retryable: false,
                }) => {
                    // Non-transient: a 4xx client error (bad model / bad key /
                    // malformed payload) or a contract violation from the
                    // upstream. Retrying just stalls ~max_retries × backoff
                    // (≈2 min/doc) to arrive at the same failure. Fail fast.
                    warn!("embed failed (non-transient, not retrying): {err}");
                    return Err(err);
                }
                Err(EmbedFailure {
                    err,
                    retryable: true,
                }) => {
                    debug!("embed attempt {attempt} failed (transient): {err}");
                    last_err = err;
                }
            }
        }

        Err(last_err)
    }

    async fn send_once(
        &self,
        texts: &[String],
        model: &str,
    ) -> std::result::Result<Vec<Vec<f32>>, EmbedFailure> {
        let body = EmbedRequest {
            input: texts,
            model,
        };

        let mut req = self
            .client
            .post(&self.config.endpoint)
            .header("Content-Type", "application/json");

        if let Some(key) = &self.config.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        // Transport-level failures (connection refused, DNS, timeout) are
        // transient: the proxy may be restarting or briefly unreachable.
        let resp = req.json(&body).send().await.map_err(|e| {
            EmbedFailure::transient(XerjError::embedding(format!("HTTP request: {e}")))
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Classify by status (item 9): 5xx are server-side and transient;
            // 429 (rate limit) and 408 (request timeout) are transient; every
            // other 4xx is a permanent client error — a wrong model, a bad API
            // key, or a malformed request — that no amount of retrying fixes.
            let retryable = status_is_retryable(status.as_u16());
            let body = resp.text().await.unwrap_or_default();
            let err = XerjError::embedding(format!("embedding API returned {status}: {body}"));
            return Err(if retryable {
                EmbedFailure::transient(err)
            } else {
                EmbedFailure::permanent(err)
            });
        }

        // A 200 with an unparseable / short body is a contract violation, not
        // a transient blip — don't spin on it.
        let response: EmbedResponse = resp.json().await.map_err(|e| {
            EmbedFailure::permanent(XerjError::embedding(format!("response parse: {e}")))
        })?;

        // Sort by index to restore original order
        let mut data = response.data;
        data.sort_by_key(|d| d.index);

        if data.len() != texts.len() {
            return Err(EmbedFailure::permanent(XerjError::embedding(format!(
                "expected {} embeddings, got {}",
                texts.len(),
                data.len()
            ))));
        }

        Ok(data.into_iter().map(|d| d.embedding).collect())
    }

    /// Embed a single text.
    pub async fn embed(&self, text: String) -> Result<Vec<f32>> {
        let mut results = self.embed_batch(vec![text]).await?;
        results
            .pop()
            .ok_or_else(|| XerjError::embedding("empty embedding response"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let cfg =
            EmbeddingProxyConfig::new("http://localhost/v1/embeddings", "text-embedding-3-small");
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_concurrent, 4);
        assert_eq!(cfg.max_retries, 3);
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn config_with_api_key() {
        let cfg = EmbeddingProxyConfig::new("http://localhost", "model").with_api_key("sk-test");
        assert_eq!(cfg.api_key.as_deref(), Some("sk-test"));
    }

    #[tokio::test]
    async fn embed_empty_batch_returns_empty() {
        let cfg = EmbeddingProxyConfig::new("http://localhost", "model");
        let proxy = EmbeddingProxy::new(cfg).unwrap();
        let result = proxy.embed_batch(vec![]).await.unwrap();
        assert!(result.is_empty());
    }

    // Note: actual HTTP tests require a live embedding endpoint.
    // Integration tests should use a mock server (e.g., wiremock).

    #[test]
    fn status_classification_item9() {
        // Permanent client errors → fail fast.
        assert!(!status_is_retryable(400)); // bad request
        assert!(!status_is_retryable(401)); // bad api key
        assert!(!status_is_retryable(403)); // forbidden
        assert!(!status_is_retryable(404)); // wrong model / endpoint
        assert!(!status_is_retryable(422)); // unprocessable payload
                                            // Transient → retry.
        assert!(status_is_retryable(408)); // request timeout
        assert!(status_is_retryable(429)); // rate limited
        assert!(status_is_retryable(500)); // internal
        assert!(status_is_retryable(502)); // bad gateway
        assert!(status_is_retryable(503)); // unavailable (proxy restarting)
    }

    #[tokio::test]
    async fn permanent_4xx_fails_fast_no_retry() {
        // A proxy that always 404s a bad model: with max_retries=5 and a
        // 200ms base backoff, the OLD code would sleep 200+400+800+1600+3200ms
        // ≈ 6.2s before giving up. Classified as permanent, we return on the
        // first attempt — well under the first backoff.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            // Serve a few 404s so a retrying client would keep hitting us.
            for _ in 0..8 {
                if let Ok((mut s, _)) = listener.accept() {
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf);
                    let body = "{\"error\":\"model not found\"}";
                    let resp = format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = s.write_all(resp.as_bytes());
                }
            }
        });

        let mut cfg =
            EmbeddingProxyConfig::new(format!("http://{addr}/v1/embeddings"), "bad-model");
        cfg.max_retries = 5;
        let proxy = EmbeddingProxy::new(cfg).unwrap();

        let start = std::time::Instant::now();
        let res = proxy.embed(String::from("hello")).await;
        let elapsed = start.elapsed();

        assert!(res.is_err(), "a 404 must surface as an error");
        // Fail-fast: no backoff sleeps at all (first backoff is 200ms).
        assert!(
            elapsed < Duration::from_millis(200),
            "permanent 4xx should not retry; took {elapsed:?}"
        );
        drop(handle); // listener already closed by loop exit
    }
}
