//! Pinecone source over its REST data-plane API.
//!
//! * describe: control plane `GET {controller}/indexes/{name}` → `dimension`,
//!   `metric`, `host`; data plane `POST {host}/describe_index_stats` for the
//!   namespace's vector count.
//! * page: `GET {host}/vectors/list?namespace=&limit=&paginationToken=` for
//!   ids (serverless indexes), then `GET {host}/vectors/fetch?ids=…` for
//!   values and metadata. The cursor is Pinecone's pagination token.
//! * top1: `POST {host}/query`.
//!
//! `--pinecone-host` skips the control-plane lookup, and
//! `PINECONE_CONTROLLER_URL` points the lookup elsewhere (tests use a mock).

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use super::{f32s, Page, Point, Similarity, Source, SourceInfo};

const API_VERSION: &str = "2024-07";

pub struct Pinecone {
    http: Client,
    index: String,
    namespace: Option<String>,
    key: String,
    host: Option<String>,
    controller: String,
}

impl Pinecone {
    pub fn new(
        index: &str,
        namespace: Option<String>,
        key: Option<String>,
        host: Option<String>,
        controller: Option<String>,
    ) -> Result<Self> {
        Ok(Pinecone {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
            index: index.to_string(),
            namespace,
            key: key.ok_or_else(|| anyhow!("Pinecone needs an API key"))?,
            host: host.map(|h| normalize_host(&h)),
            controller: controller
                .unwrap_or_else(|| "https://api.pinecone.io".into())
                .trim_end_matches('/')
                .to_string(),
        })
    }

    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::blocking::RequestBuilder {
        self.http
            .request(method, url)
            .header("Api-Key", &self.key)
            .header("X-Pinecone-API-Version", API_VERSION)
    }

    fn send(&self, rb: reqwest::blocking::RequestBuilder, what: &str) -> Result<Value> {
        let resp = rb.send().with_context(|| format!("Pinecone {what}"))?;
        let st = resp.status();
        let v: Value = resp.json().unwrap_or(Value::Null);
        if !st.is_success() {
            bail!("Pinecone {what}: HTTP {st}: {v}");
        }
        Ok(v)
    }

    fn host(&self) -> Result<&str> {
        self.host
            .as_deref()
            .ok_or_else(|| anyhow!("Pinecone: index host unknown"))
    }

    fn ns(&self) -> &str {
        self.namespace.as_deref().unwrap_or("")
    }
}

fn normalize_host(h: &str) -> String {
    let h = h.trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else {
        format!("https://{h}")
    }
}

pub(crate) fn metric(m: &str) -> Result<Similarity> {
    match m {
        "cosine" => Ok(Similarity::Cosine),
        "dotproduct" => Ok(Similarity::MaxInnerProduct),
        "euclidean" => Ok(Similarity::L2),
        other => Err(anyhow!(
            "Pinecone metric `{other}` is not cosine, dotproduct or euclidean"
        )),
    }
}

impl Source for Pinecone {
    fn describe(&mut self) -> Result<SourceInfo> {
        let d = self.send(
            self.req(
                reqwest::Method::GET,
                &format!("{}/indexes/{}", self.controller, self.index),
            ),
            "describe index",
        )?;
        if self.host.is_none() {
            let h = d
                .get("host")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Pinecone: index has no host"))?;
            self.host = Some(normalize_host(h));
        }
        let dims = d
            .get("dimension")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("Pinecone: no dimension"))?;
        let sim = metric(d.get("metric").and_then(Value::as_str).unwrap_or("cosine"))?;
        let stats = self
            .send(
                self.req(
                    reqwest::Method::POST,
                    &format!("{}/describe_index_stats", self.host()?),
                )
                .json(&json!({})),
                "describe_index_stats",
            )
            .ok();
        let count = stats.as_ref().and_then(|s| {
            s.pointer(&format!("/namespaces/{}/vectorCount", self.ns()))
                .or_else(|| {
                    if self.ns().is_empty() {
                        s.get("totalVectorCount")
                    } else {
                        None
                    }
                })
                .and_then(Value::as_u64)
        });
        Ok(SourceInfo {
            kind: "pinecone",
            name: self.index.clone(),
            dims: dims as usize,
            similarity: sim,
            count,
            similarity_basis: "index metric".into(),
        })
    }

    fn page(&mut self, cursor: Option<&str>, limit: usize) -> Result<Page> {
        let host = self.host()?.to_string();
        // `vectors/list` caps a page at 100 ids; fetch takes up to 1000.
        let mut ids: Vec<String> = Vec::new();
        let mut token = cursor.map(str::to_string);
        let mut ended = false;
        while ids.len() < limit {
            let want = (limit - ids.len()).min(100);
            let mut q: Vec<(&str, String)> = vec![
                ("limit", want.to_string()),
                ("namespace", self.ns().to_string()),
            ];
            if let Some(t) = &token {
                q.push(("paginationToken", t.clone()));
            }
            let v = self.send(
                self.req(reqwest::Method::GET, &format!("{host}/vectors/list"))
                    .query(&q),
                "vectors/list",
            )?;
            let got: Vec<String> = v
                .get("vectors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|x| x.get("id").and_then(Value::as_str).map(str::to_string))
                .collect();
            token = v
                .pointer("/pagination/next")
                .and_then(Value::as_str)
                .map(str::to_string);
            let n = got.len();
            ids.extend(got);
            if token.is_none() || n == 0 {
                ended = true;
                break;
            }
        }
        let mut points = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(200) {
            let mut q: Vec<(&str, String)> = chunk.iter().map(|i| ("ids", i.clone())).collect();
            q.push(("namespace", self.ns().to_string()));
            let v = self.send(
                self.req(reqwest::Method::GET, &format!("{host}/vectors/fetch"))
                    .query(&q),
                "vectors/fetch",
            )?;
            let map = v
                .get("vectors")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for id in chunk {
                let rec = map
                    .get(id)
                    .ok_or_else(|| anyhow!("Pinecone fetch did not return {id}"))?;
                let vector = rec
                    .get("values")
                    .and_then(f32s)
                    .ok_or_else(|| anyhow!("Pinecone {id}: no values"))?;
                let payload = rec
                    .get("metadata")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                points.push(Point {
                    id: id.clone(),
                    vector,
                    payload,
                    document: None,
                });
            }
        }
        Ok(Page {
            points,
            next: if ended { None } else { token },
        })
    }

    fn top1(&mut self, q: &[f32]) -> Result<Option<String>> {
        let host = self.host()?.to_string();
        let v = self.send(
            self.req(reqwest::Method::POST, &format!("{host}/query"))
                .json(&json!({
                "vector": q, "topK": 1, "namespace": self.ns(),
                "includeValues": false, "includeMetadata": false })),
            "query",
        )?;
        Ok(v.pointer("/matches/0/id")
            .and_then(Value::as_str)
            .map(str::to_string))
    }
}
