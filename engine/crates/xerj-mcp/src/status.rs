//! `xerj_status`: reachability, cluster health and visible indices (#2515).
//!
//! Uses up to three read-only GETs. A missing autoindex catalog does not mean
//! that the node has no data: ordinary indices and managed corpora have their
//! own discovery paths. Failed or malformed probes remain unknown/errors.

use serde_json::Value;

use crate::map::CATALOG_INDEX;
use crate::{tool_text, transport_failure_text, Ctx};

const MAX_LISTED_INDICES: usize = 20;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Bound each response before parsing, including chunked bodies. A readable
/// HTTP error is evidence of reachability, not a successful status probe.
async fn get(ctx: &Ctx, path: &str) -> Result<(u16, String), String> {
    let url = format!("{}{}", ctx.base_url, path);
    let mut req = ctx.client.get(&url);
    if let Some(auth) = &ctx.auth {
        req = req.header("Authorization", auth);
    }
    let mut response = req
        .send()
        .await
        .map_err(|error| transport_failure_text(&ctx.base_url, &url, &error))?;
    let status = response.status().as_u16();
    let limit_error = || {
        format!(
        "GET {path} answered HTTP {status}, but its response exceeds the {MAX_RESPONSE_BYTES}-byte status-probe limit"
    )
    };
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(limit_error());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        format!(
            "GET {path} answered HTTP {status}, but its response body could not be read: {}",
            transport_failure_text(&ctx.base_url, &url, &error)
        )
    })? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(limit_error());
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(body)
        .map_err(|_| format!("GET {path} answered HTTP {status}, but its body is not UTF-8"))?;
    Ok((status, text))
}

fn parse(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// Keep remote names to a single bounded display line.
fn display(value: &str) -> String {
    let mut chars = value.chars();
    let mut out: String = chars
        .by_ref()
        .take(200)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn index_rows(text: &str) -> Result<Vec<(String, Option<u64>)>, String> {
    let value = parse(text).ok_or("index list is not valid JSON")?;
    let rows = value.as_array().ok_or("index list is not a JSON array")?;
    rows.iter()
        .map(|row| {
            let name = row
                .get("index")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or("index list contains a row without an index name")?;
            let docs = row.get("docs.count").and_then(|count| {
                count
                    .as_u64()
                    .or_else(|| count.as_str()?.parse::<u64>().ok())
            });
            Ok((name.to_string(), docs))
        })
        .collect()
}

pub(crate) async fn run(ctx: &Ctx, _args: &Value) -> Value {
    let (status, banner_text) = match get(ctx, "/").await {
        Ok(response) => response,
        Err(error) => {
            return tool_text(
                format!(
                    "xerj_status: could not read node status at {}.\n{error}",
                    ctx.base_url
                ),
                true,
            )
        }
    };
    if status == 401 || status == 403 {
        return tool_text(
            format!(
                "xerj_status: the HTTP service at {} is UP but refused the root request \
             (HTTP {status}).\nThis is an auth problem, not a connection problem. \
             Check XERJ_AUTH (or `xerj mcp --auth`) and the permissions for this endpoint. \
             Other endpoints have not been probed.",
                ctx.base_url
            ),
            true,
        );
    }
    if !(200..300).contains(&status) {
        return tool_text(
            format!(
                "xerj_status: an HTTP service answered at {}, but the root probe failed \
             with HTTP {status}. Node status is unknown.",
                ctx.base_url
            ),
            true,
        );
    }
    let banner = parse(&banner_text);
    let version = banner
        .as_ref()
        .and_then(|b| b.get("version"))
        .and_then(|v| v.get("number"))
        .and_then(Value::as_str);
    let Some(version) = version else {
        return tool_text(
            format!(
                "xerj_status: an HTTP service answered at {}, but the root endpoint did not \
             return a JSON node banner with version.number. Node status is unknown.",
                ctx.base_url
            ),
            true,
        );
    };
    let mut lines = vec![format!(
        "xerj_status: the node at {} is UP (root endpoint answered HTTP {status}).", ctx.base_url
    ), format!(
        "  version: {} (the Elasticsearch-compatible number this node reports, not XERJ's own version)", display(version)
    )];
    if let Some(name) = banner
        .as_ref()
        .and_then(|b| b.get("cluster_name"))
        .and_then(Value::as_str)
    {
        lines.push(format!("  cluster name: {}", display(name)));
    }
    let mut incomplete = false;
    let health = match get(ctx, "/_cluster/health").await {
        Ok((code, text)) if (200..300).contains(&code) => parse(&text)
            .filter(|h| h.get("status").and_then(Value::as_str).is_some())
            .ok_or_else(|| "GET /_cluster/health did not return a health status".to_string()),
        Ok((code, _)) => Err(format!("GET /_cluster/health answered HTTP {code}")),
        Err(error) => Err(error),
    };
    match health {
        Ok(h) => {
            let state = h["status"].as_str().expect("validated health status");
            let mut facts = Vec::new();
            if let Some(n) = h.get("number_of_nodes").and_then(Value::as_u64) {
                facts.push(format!("{n} node{}", if n == 1 { "" } else { "s" }));
            }
            if let Some(n) = h.get("active_shards").and_then(Value::as_u64) {
                facts.push(format!("{n} active shards"));
            }
            lines.push(format!(
                "  cluster health: {}{}",
                display(state),
                if facts.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", facts.join(", "))
                }
            ));
        }
        Err(error) => {
            incomplete = true;
            lines.push(format!("  cluster health: unavailable ({error})"));
        }
    }
    let indices = match get(ctx, "/_cat/indices?format=json&h=index,docs.count").await {
        Ok((code, text)) if (200..300).contains(&code) => index_rows(&text),
        Ok((code, _)) => Err(format!("GET /_cat/indices answered HTTP {code}")),
        Err(error) => Err(error),
    };
    match indices {
        Ok(rows) => {
            if rows.is_empty() {
                lines.push("  indices: none returned for these credentials".into());
            } else {
                lines.push(format!(
                    "  indices: {} visible{}",
                    rows.len(),
                    if rows.len() > MAX_LISTED_INDICES {
                        format!(" (first {MAX_LISTED_INDICES} listed)")
                    } else {
                        String::new()
                    }
                ));
                for (name, docs) in rows.iter().take(MAX_LISTED_INDICES) {
                    lines.push(match docs {
                        Some(n) => format!("    {}  {n} docs", display(name)),
                        None => format!("    {}  document count unknown", display(name)),
                    });
                }
            }
            if let Some((_, docs)) = rows.iter().find(|(name, _)| name == CATALOG_INDEX) {
                lines.push(format!(
                    "  autoindex catalog: PRESENT in the index list (`{CATALOG_INDEX}`{}).",
                    docs.map(|n| format!(" holds {n} docs")).unwrap_or_default()
                ));
                lines.push("    Catalog presence alone does not establish dataset availability; use xerj_map to inspect it.".into());
            } else {
                lines.push("  autoindex catalog: ABSENT from the returned index list.".into());
                lines.push("    This is not a connection failure. Ordinary indices may still be searchable, and managed corpora have a separate namespace.".into());
                lines.push("    Use `xerj autoindex <folder>` to create an autoindex catalog; use xerj_map to inspect catalog datasets.".into());
            }
        }
        Err(error) => {
            incomplete = true;
            lines.push(format!("  indices: unavailable ({error})"));
            lines.push(format!("  autoindex catalog: unknown — whether `{CATALOG_INDEX}` exists was not established."));
        }
    }
    tool_text(lines.join("\n"), incomplete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    /// Serve a fixed route table on a loopback port. Keys are paths without
    /// their query string, because the real calls carry `?format=json&...`.
    async fn serve(routes: HashMap<&'static str, (u16, String)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                    .await
                    .unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = request.split_whitespace().nth(1).unwrap_or("/");
                let path = target.split('?').next().unwrap_or("/").to_string();
                let (status, body) = routes
                    .get(path.as_str())
                    .cloned()
                    .unwrap_or((404, "{}".to_string()));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\
                     Content-Type: application/json\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, reply.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    fn ctx(base_url: String) -> Ctx {
        Ctx {
            client: crate::node_http_builder(&base_url)
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
            base_url,
            auth: None,
        }
    }

    fn banner() -> String {
        json!({
            "name": "node-1",
            "cluster_name": "xerj",
            "version": { "number": "8.13.0", "distribution": "xerj" },
            "tagline": "You Know, for Search"
        })
        .to_string()
    }

    fn health(state: &str) -> String {
        json!({
            "cluster_name": "xerj",
            "status": state,
            "number_of_nodes": 1,
            "active_shards": 15
        })
        .to_string()
    }

    fn cat(rows: Value) -> String {
        rows.to_string()
    }

    fn text(v: &Value) -> String {
        v["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// The bug in #2515: `xerj_map` 404s on an unindexed node, and from the
    /// agent's side that reads like the node is not there. The status tool has
    /// to say UP and EMPTY in one answer, and name the 404 as the empty case.
    #[tokio::test]
    async fn an_unindexed_node_reports_up_and_empty_not_unreachable() {
        let mut routes = HashMap::new();
        routes.insert("/", (200, banner()));
        routes.insert("/_cluster/health", (200, health("green")));
        routes.insert("/_cat/indices", (200, cat(json!([]))));
        let base = serve(routes).await;

        let out = run(&ctx(base), &json!({})).await;
        assert_eq!(out["isError"], false, "{}", text(&out));
        let body = text(&out);
        assert!(body.contains("is UP"), "{body}");
        assert!(body.contains("indices: none"), "{body}");
        assert!(body.contains("autoindex catalog: ABSENT"), "{body}");
        assert!(
            body.contains("not a connection failure"),
            "the empty node must not read as a broken one: {body}"
        );
        assert!(body.contains("xerj autoindex <folder>"), "{body}");
        assert!(body.contains("8.13.0"), "{body}");
        assert!(body.contains("green"), "{body}");
    }

    #[tokio::test]
    async fn an_indexed_node_names_the_catalog_and_its_doc_count() {
        let mut routes = HashMap::new();
        routes.insert("/", (200, banner()));
        routes.insert("/_cluster/health", (200, health("yellow")));
        routes.insert(
            "/_cat/indices",
            (
                200,
                cat(json!([
                    { "index": "autoindex-catalog", "docs.count": "12" },
                    { "index": "ax", "docs.count": "812" }
                ])),
            ),
        );
        let base = serve(routes).await;

        let out = run(&ctx(base), &json!({})).await;
        assert_eq!(out["isError"], false, "{}", text(&out));
        let body = text(&out);
        assert!(body.contains("indices: 2"), "{body}");
        assert!(body.contains("autoindex-catalog"), "{body}");
        assert!(body.contains("12 docs"), "{body}");
        assert!(body.contains("autoindex catalog: PRESENT"), "{body}");
        assert!(!body.contains("ABSENT"), "{body}");
        assert!(body.contains("yellow"), "{body}");
    }

    /// Nothing listening: the one case that *is* a connectivity failure, and it
    /// has to say so rather than printing an empty successful answer.
    #[tokio::test]
    async fn nothing_listening_is_the_only_unreachable_answer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let out = run(&ctx(format!("http://{addr}")), &json!({})).await;
        assert_eq!(out["isError"], true, "{}", text(&out));
        let body = text(&out);
        assert!(body.contains("could not read node status"), "{body}");
    }

    /// A 401 means the node is running. Reporting that as "unreachable" would
    /// send an agent looking at ports when the problem is the key.
    #[tokio::test]
    async fn a_refused_key_is_up_with_an_auth_problem_not_down() {
        let mut routes = HashMap::new();
        routes.insert("/", (401, "{\"error\":\"unauthorized\"}".to_string()));
        let base = serve(routes).await;

        let out = run(&ctx(base), &json!({})).await;
        let body = text(&out);
        assert!(body.contains("is UP but refused"), "{body}");
        assert!(
            body.contains("auth problem, not a connection problem"),
            "{body}"
        );
        assert!(body.contains("XERJ_AUTH"), "{body}");
    }

    /// A node that answers the banner but not `_cat/indices` is up. The catalog
    /// question is then unknown, which must not be reported as absent.
    #[tokio::test]
    async fn an_unreadable_index_list_is_unknown_not_absent() {
        let mut routes = HashMap::new();
        routes.insert("/", (200, banner()));
        routes.insert("/_cluster/health", (200, health("green")));
        routes.insert("/_cat/indices", (500, "boom".to_string()));
        let base = serve(routes).await;

        let out = run(&ctx(base), &json!({})).await;
        assert_eq!(out["isError"], true, "{}", text(&out));
        let body = text(&out);
        assert!(body.contains("autoindex catalog: unknown"), "{body}");
        assert!(!body.contains("ABSENT"), "{body}");
    }
    #[tokio::test]
    async fn ordinary_indices_without_a_catalog_are_not_an_empty_node() {
        let routes = HashMap::from([
            ("/", (200, banner())),
            ("/_cluster/health", (200, health("green"))),
            (
                "/_cat/indices",
                (200, cat(json!([{"index":"logs", "docs.count":"42"}]))),
            ),
        ]);
        let out = run(&ctx(serve(routes).await), &json!({})).await;
        assert_eq!(out["isError"], false);
        let body = text(&out);
        assert!(body.contains("logs"), "{body}");
        assert!(body.contains("42 docs"), "{body}");
        assert!(!body.contains("has nothing indexed"), "{body}");
        assert!(!body.contains("That is an empty node"), "{body}");
        assert!(!body.contains("xerj_search answer"), "{body}");
    }

    #[tokio::test]
    async fn malformed_index_rows_remain_unknown_instead_of_becoming_empty() {
        let routes = HashMap::from([
            ("/", (200, banner())),
            ("/_cluster/health", (200, health("green"))),
            ("/_cat/indices", (200, cat(json!([{"docs.count":"42"}])))),
        ]);
        let out = run(&ctx(serve(routes).await), &json!({})).await;
        assert_eq!(out["isError"], true, "{}", text(&out));
        let body = text(&out);
        assert!(body.contains("autoindex catalog: unknown"), "{body}");
        assert!(!body.contains("indices: none"), "{body}");
        assert!(!body.contains("ABSENT"), "{body}");
    }

    #[tokio::test]
    async fn root_failures_and_non_node_banners_are_errors() {
        for (code, reply) in [
            (503, "{}"),
            (404, "{}"),
            (200, "<html>other service</html>"),
            (200, "[]"),
        ] {
            let routes = HashMap::from([("/", (code, reply.to_string()))]);
            let out = run(&ctx(serve(routes).await), &json!({})).await;
            assert_eq!(out["isError"], true, "{}", text(&out));
            assert!(!text(&out).contains("node at"), "{}", text(&out));
        }
    }

    #[tokio::test]
    async fn endpoint_auth_refusals_are_errors_not_absence() {
        for endpoint in ["/_cluster/health", "/_cat/indices"] {
            for code in [401, 403] {
                let mut routes = HashMap::from([
                    ("/", (200, banner())),
                    ("/_cluster/health", (200, health("green"))),
                    ("/_cat/indices", (200, cat(json!([])))),
                ]);
                routes.insert(endpoint, (code, "{}".to_string()));
                let out = run(&ctx(serve(routes).await), &json!({})).await;
                assert_eq!(out["isError"], true, "{}", text(&out));
                assert!(
                    text(&out).contains(&format!("HTTP {code}")),
                    "{}",
                    text(&out)
                );
                if endpoint == "/_cat/indices" {
                    assert!(!text(&out).contains("ABSENT"), "{}", text(&out));
                }
            }
        }
    }

    #[tokio::test]
    async fn an_empty_catalog_does_not_promise_searchable_datasets() {
        let routes = HashMap::from([
            ("/", (200, banner())),
            ("/_cluster/health", (200, health("green"))),
            (
                "/_cat/indices",
                (200, cat(json!([{"index":CATALOG_INDEX,"docs.count":"0"}]))),
            ),
        ]);
        let out = run(&ctx(serve(routes).await), &json!({})).await;
        assert_eq!(out["isError"], false);
        assert!(text(&out).contains("PRESENT"));
        assert!(text(&out).contains("0 docs"));
        assert!(!text(&out).contains("have data to read"));
    }

    #[tokio::test]
    async fn an_oversized_index_response_is_unknown_not_empty() {
        let rows = cat(json!([{"index":"x".repeat(256 * 1024),"docs.count":"1"}]));
        let routes = HashMap::from([
            ("/", (200, banner())),
            ("/_cluster/health", (200, health("green"))),
            ("/_cat/indices", (200, rows)),
        ]);
        let out = run(&ctx(serve(routes).await), &json!({})).await;
        assert_eq!(out["isError"], true, "{}", text(&out));
        assert!(text(&out).contains("status-probe limit"));
        assert!(text(&out).contains("catalog: unknown"));
    }

    async fn raw_root(reply: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while request.len() < 8192 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let n = tokio::io::AsyncReadExt::read(&mut socket, &mut chunk)
                    .await
                    .unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, &reply).await;
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn truncated_http_bodies_are_errors_instead_of_empty_text() {
        let base = raw_root(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}".to_vec(),
        )
        .await;
        let result = get(&ctx(base), "/").await;
        assert!(
            result.is_err(),
            "an incomplete response must not become an empty successful body"
        );
    }

    #[tokio::test]
    async fn chunked_responses_are_also_bounded_before_parsing() {
        let payload = "x".repeat(256 * 1024 + 1);
        let reply = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{payload}\r\n0\r\n\r\n", payload.len());
        let base = raw_root(reply.into_bytes()).await;
        let result = get(&ctx(base), "/").await;
        assert!(
            result.is_err(),
            "a missing Content-Length must not bypass the body limit"
        );
    }
}
