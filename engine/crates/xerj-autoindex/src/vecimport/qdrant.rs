//! Qdrant source over its REST API.
//!
//! * describe: `GET /collections/{c}` → `result.config.params.vectors`
//!   (`{size, distance}`, or a map of named vectors) and `result.points_count`.
//! * page: `POST /collections/{c}/points/scroll` with `with_vector` and
//!   `with_payload`; the cursor is `next_page_offset` (an id, kept as JSON
//!   text so integer and UUID ids both round-trip exactly).
//! * top1: `POST /collections/{c}/points/search`.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use super::{f32s, id_text, Page, Point, Similarity, Source, SourceInfo};

pub struct Qdrant {
    http: Client,
    base: String,
    collection: String,
    key: Option<String>,
    /// Named vector to read, when the collection has several.
    name: Option<String>,
}

impl Qdrant {
    pub fn new(
        base: &str,
        collection: &str,
        key: Option<String>,
        name: Option<String>,
    ) -> Result<Self> {
        Ok(Qdrant {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            collection: collection.to_string(),
            key,
            name,
        })
    }

    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(k) = &self.key {
            r = r.header("api-key", k);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r
            .send()
            .with_context(|| format!("Qdrant {}{path}", self.base))?;
        let st = resp.status();
        let v: Value = resp.json().unwrap_or(Value::Null);
        if !st.is_success() {
            bail!("Qdrant {path}: HTTP {st}: {v}");
        }
        Ok(v)
    }

    fn vector_of(&self, v: &Value) -> Option<Vec<f32>> {
        match (v, &self.name) {
            (Value::Array(_), _) => f32s(v),
            (Value::Object(m), Some(n)) => m.get(n).and_then(f32s),
            (Value::Object(m), None) if m.len() == 1 => m.values().next().and_then(f32s),
            _ => None,
        }
    }
}

pub(crate) fn distance(d: &str) -> Result<Similarity> {
    match d {
        "Cosine" => Ok(Similarity::Cosine),
        "Dot" => Ok(Similarity::MaxInnerProduct),
        "Euclid" => Ok(Similarity::L2),
        other => Err(anyhow!(
            "Qdrant distance `{other}` has no XERJ equivalent (cosine, dot, euclid); pass \
             --similarity only if you accept a different ranking"
        )),
    }
}

impl Source for Qdrant {
    fn describe(&mut self) -> Result<SourceInfo> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/collections/{}", self.collection),
            None,
        )?;
        let r = v
            .get("result")
            .ok_or_else(|| anyhow!("Qdrant: no `result` in collection info"))?;
        let vecs = r
            .pointer("/config/params/vectors")
            .ok_or_else(|| anyhow!("Qdrant: collection has no dense vectors config"))?;
        let cfg = if vecs.get("size").is_some() {
            if let Some(n) = &self.name {
                bail!("--vector-name {n}: this collection has one unnamed vector");
            }
            vecs.clone()
        } else {
            let m = vecs
                .as_object()
                .ok_or_else(|| anyhow!("Qdrant: unexpected vectors config"))?;
            match &self.name {
                Some(n) => m.get(n).cloned().ok_or_else(|| {
                    anyhow!(
                        "--vector-name {n}: collection has {:?}",
                        m.keys().collect::<Vec<_>>()
                    )
                })?,
                None if m.len() == 1 => {
                    self.name = m.keys().next().cloned();
                    m.values().next().cloned().unwrap()
                }
                None => bail!(
                    "this collection has named vectors {:?}; pick one with --vector-name",
                    m.keys().collect::<Vec<_>>()
                ),
            }
        };
        let dims = cfg
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("Qdrant: vector size missing"))?;
        let sim = distance(cfg.get("distance").and_then(Value::as_str).unwrap_or(""))?;
        Ok(SourceInfo {
            kind: "qdrant",
            name: self.collection.clone(),
            dims: dims as usize,
            similarity: sim,
            count: r.get("points_count").and_then(Value::as_u64),
            similarity_basis: "collection config".into(),
        })
    }

    fn page(&mut self, cursor: Option<&str>, limit: usize) -> Result<Page> {
        let mut body = json!({ "limit": limit, "with_payload": true, "with_vector": true });
        if let Some(n) = &self.name {
            body["with_vector"] = json!([n]);
        }
        if let Some(c) = cursor {
            body["offset"] =
                serde_json::from_str(c).context("Qdrant cursor in the journal is not JSON")?;
        }
        let v = self.call(
            reqwest::Method::POST,
            &format!("/collections/{}/points/scroll", self.collection),
            Some(body),
        )?;
        let r = v
            .get("result")
            .ok_or_else(|| anyhow!("Qdrant scroll: no result"))?;
        let mut points = Vec::new();
        for p in r
            .get("points")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = p
                .get("id")
                .and_then(id_text)
                .ok_or_else(|| anyhow!("Qdrant point without id"))?;
            let vector = p
                .get("vector")
                .and_then(|v| self.vector_of(v))
                .ok_or_else(|| anyhow!("Qdrant point {id} has no dense vector"))?;
            let payload = p
                .get("payload")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            points.push(Point {
                id,
                vector,
                payload,
                document: None,
            });
        }
        let next = match r.get("next_page_offset") {
            None | Some(Value::Null) => None,
            Some(o) => Some(o.to_string()),
        };
        Ok(Page { points, next })
    }

    fn top1(&mut self, q: &[f32]) -> Result<Option<String>> {
        let vector = match &self.name {
            Some(n) => json!({ "name": n, "vector": q }),
            None => json!(q),
        };
        let v = self.call(
            reqwest::Method::POST,
            &format!("/collections/{}/points/search", self.collection),
            Some(json!({ "vector": vector, "limit": 1, "with_payload": false, "params": { "exact": true } })),
        )?;
        Ok(v.pointer("/result/0/id").and_then(id_text))
    }
}
