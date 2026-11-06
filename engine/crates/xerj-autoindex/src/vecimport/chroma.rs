//! Chroma source over its v2 REST API (Chroma ≥ 0.6 / 1.x).
//!
//! * describe: `GET /api/v2/tenants/{t}/databases/{d}/collections/{name}` →
//!   `id`, `dimension`, distance from `configuration_json.hnsw.space`
//!   (1.x) or `metadata["hnsw:space"]` (0.6); `.../collections/{id}/count`.
//! * page: `POST .../collections/{id}/get` with `limit`/`offset` and
//!   `include: [embeddings, metadatas, documents]`. The cursor is the offset.
//!   Chroma pages by offset, so a collection written to during the import can
//!   shift rows between pages; the import says so up front.
//! * top1: `POST .../collections/{id}/query`.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use super::{f32s, Page, Point, Similarity, Source, SourceInfo};

pub struct Chroma {
    http: Client,
    base: String,
    name: String,
    coll_path: String,
    id: Option<String>,
    token: Option<String>,
}

impl Chroma {
    pub fn new(
        base: &str,
        name: &str,
        tenant: &str,
        database: &str,
        token: Option<String>,
    ) -> Result<Self> {
        Ok(Chroma {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            name: name.to_string(),
            coll_path: format!("/api/v2/tenants/{tenant}/databases/{database}/collections"),
            id: None,
            token,
        })
    }

    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(t) = &self.token {
            r = r.header("x-chroma-token", t).bearer_auth(t);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r
            .send()
            .with_context(|| format!("Chroma {}{path}", self.base))?;
        let st = resp.status();
        let v: Value = resp.json().unwrap_or(Value::Null);
        if !st.is_success() {
            bail!("Chroma {path}: HTTP {st}: {v}");
        }
        Ok(v)
    }

    fn cid(&self) -> Result<&str> {
        self.id
            .as_deref()
            .ok_or_else(|| anyhow!("Chroma: describe() not called"))
    }
}

pub(crate) fn space(s: &str) -> Result<Similarity> {
    match s {
        "cosine" => Ok(Similarity::Cosine),
        "ip" => Ok(Similarity::MaxInnerProduct),
        "l2" => Ok(Similarity::L2),
        other => Err(anyhow!("Chroma space `{other}` is not cosine, ip or l2")),
    }
}

impl Source for Chroma {
    fn describe(&mut self) -> Result<SourceInfo> {
        let c = self.call(
            reqwest::Method::GET,
            &format!("{}/{}", self.coll_path, self.name),
            None,
        )?;
        let id = c
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Chroma: collection has no id"))?;
        self.id = Some(id.to_string());
        let (sp, basis) = if let Some(s) = c
            .pointer("/configuration_json/hnsw/space")
            .and_then(Value::as_str)
        {
            (s.to_string(), "collection configuration")
        } else if let Some(s) = c.pointer("/metadata/hnsw:space").and_then(Value::as_str) {
            (s.to_string(), "collection metadata hnsw:space")
        } else {
            (
                "l2".to_string(),
                "Chroma default (l2): the collection names no space",
            )
        };
        let count = self
            .call(
                reqwest::Method::GET,
                &format!("{}/{}/count", self.coll_path, id),
                None,
            )
            .ok()
            .and_then(|v| v.as_u64());
        let dims = match c.get("dimension").and_then(Value::as_u64) {
            Some(d) => d as usize,
            None => {
                let first = self.page(None, 1)?;
                first
                    .points
                    .first()
                    .map(|p| p.vector.len())
                    .ok_or_else(|| {
                        anyhow!(
                            "Chroma: collection `{}` is empty and declares no dimension",
                            self.name
                        )
                    })?
            }
        };
        Ok(SourceInfo {
            kind: "chroma",
            name: self.name.clone(),
            dims,
            similarity: space(&sp)?,
            count,
            similarity_basis: basis.into(),
        })
    }

    fn page(&mut self, cursor: Option<&str>, limit: usize) -> Result<Page> {
        let offset: u64 = cursor
            .map(|c| c.parse())
            .transpose()
            .context("Chroma cursor")?
            .unwrap_or(0);
        let path = format!("{}/{}/get", self.coll_path, self.cid()?);
        let v = self.call(
            reqwest::Method::POST,
            &path,
            Some(json!({ "limit": limit, "offset": offset,
                         "include": ["embeddings", "metadatas", "documents"] })),
        )?;
        let ids = v
            .get("ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let embs = v
            .get("embeddings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let metas = v
            .get("metadatas")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let docs = v
            .get("documents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut points = Vec::with_capacity(ids.len());
        for (i, idv) in ids.iter().enumerate() {
            let id = idv
                .as_str()
                .ok_or_else(|| anyhow!("Chroma id is not a string"))?
                .to_string();
            let vector = embs
                .get(i)
                .and_then(f32s)
                .ok_or_else(|| anyhow!("Chroma record {id} has no embedding"))?;
            let payload = metas
                .get(i)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let document = docs.get(i).and_then(Value::as_str).map(str::to_string);
            points.push(Point {
                id,
                vector,
                payload,
                document,
            });
        }
        let next = if points.len() < limit {
            None
        } else {
            Some((offset + points.len() as u64).to_string())
        };
        Ok(Page { points, next })
    }

    fn top1(&mut self, q: &[f32]) -> Result<Option<String>> {
        let path = format!("{}/{}/query", self.coll_path, self.cid()?);
        let v = self.call(
            reqwest::Method::POST,
            &path,
            Some(json!({ "query_embeddings": [q], "n_results": 1, "include": ["distances"] })),
        )?;
        Ok(v.pointer("/ids/0/0")
            .and_then(Value::as_str)
            .map(str::to_string))
    }
}
