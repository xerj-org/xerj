//! Ordered, read-only batch retrieval over the existing `_msearch` endpoint.
//! No answer generation or confidence threshold is inferred from a BM25 score.
use serde_json::{json, Value};

use crate::{build_search_with_mapping, fetch_mapping, search_index, token_budget, tool_text, Ctx};

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn spec() -> Value {
    json!({
        "name":"xerj_batch_search",
        "description": "Search 1..50 IDs or questions in one call, in input order. Shares the CLI xerj batch-search adapter. Each result has its zero-based position, optional caller key, and status hits, abstain (only a complete empty search), or error. Hits carry citations: index/id plus file/line or record locator when available. This retrieves evidence, not generated answers or calibrated confidence. Use identifier plus an explicitly keyword-mapped field for exact IDs; query takes plain text or an ES query clause. One bad item never discards its siblings; identical inputs remain separate. Uses one _msearch request, plus at most one mapping read; a response over 32 MiB is retried in halves, and an item still over it alone fails alone. Omitted or null max_tokens defaults to 8000 UAX #29 words and caps the entire successful JSON response using StandardTokenizer UAX #29 words, not BPE. Budgeting removes raw hit bodies, then extra hits, retaining each item's status and at least one citation per matched item. If that minimum cannot fit, returns an actionable tool error, never truncated JSON or a false abstention. Errors are not truncated.",
        "inputSchema":{
            "type":"object", "additionalProperties":false,
            "properties":{
                "queries":{"type":"array","minItems":1,"maxItems":50,
                    "description":"Ordered queries. A plain string item is shorthand for {\"query\": <string>}. Duplicate keys are allowed; position is authoritative. Entire input is limited to 1 MiB.",
                    "items":{"anyOf":[{"type":"string","minLength":1},{"type":"object","additionalProperties":false,"properties":{
                        "key":{"type":"string","description":"Optional correlation key, at most 256 UTF-8 bytes."},
                        "query":{"oneOf":[{"type":"string","minLength":1},{"type":"object"}],
                            "description":"Plain text or ES query clause. Cannot be combined with identifier/field."},
                        "identifier":{"type":"string","minLength":1,"description":"Exact value, used verbatim in a term query; requires field."},
                        "field":{"type":"string","minLength":1,"description":"Flat keyword field for identifier. Must be mapped as keyword wherever present; unreadable, absent or conflicting mappings fail this item."}
                    }, "oneOf":[{"required":["query"]},{"required":["identifier","field"]}]}]}},
                "index":{"type":"string","description":"Index or pattern; alternative to prefix. Defaults to ax-*."},
                "prefix":{"type":"string","description":"Autoindex prefix, e.g. project searches project-*. Cannot combine with index."},
                "size":{"type":"integer","minimum":1,"maximum":50,"default":3,"description":"Maximum hits per item."},
                "max_tokens":{"type":"integer","minimum":32,"default":token_budget::DEFAULT_MAX_TOKENS,"description":format!("Whole-response budget counted with {} (not BPE or a byte cap); minimum 32. Omitted or null defaults to 8000 words; pass a larger integer for more. A successful response NEVER exceeds it. Preserves ordered item outcomes and at least one citation per matched item, removing raw bodies first. Refuses if that minimum cannot fit; split the batch or increase the budget. Errors are outside this budget.", token_budget::TOKENIZER_NAME)}
            },"required":["queries"]
        }
    })
}

fn known_keys(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let obj = value.as_object().ok_or("arguments must be a JSON object")?;
    if let Some(key) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("unsupported argument `{key}`"));
    }
    Ok(())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("`{key}` must be a non-blank string"))
}

pub(super) struct Batch<'a> {
    index: String,
    size: usize,
    limit: Option<usize>,
    items: &'a [Value],
}

pub(super) fn envelope(args: &Value) -> Result<Batch<'_>, String> {
    known_keys(args, &["queries", "index", "prefix", "size", "max_tokens"])?;
    if args.to_string().len() > MAX_INPUT_BYTES {
        return Err("batch input exceeds 1 MiB; split it into smaller batches".into());
    }
    if args
        .get("index")
        .is_some_and(|v| !v.is_string() && !v.is_null())
    {
        return Err("`index` must be a string".into());
    }
    let index = search_index(args)?;
    let queries = args
        .get("queries")
        .and_then(Value::as_array)
        .filter(|a| (1..=50).contains(&a.len()))
        .ok_or("`queries` must be an array of 1..50 items")?;
    let size = match args.get("size") {
        None => 3,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=50).contains(n))
            .ok_or("`size` must be an integer from 1 to 50")? as usize,
    };
    let budget = token_budget::opt_max_tokens(args)?;
    Ok(Batch {
        index,
        size,
        limit: budget,
        items: queries,
    })
}

fn validate_item(item: &Value) -> Result<(), String> {
    known_keys(item, &["key", "query", "identifier", "field"])?;
    if item
        .get("key")
        .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 256))
    {
        return Err("`key` must be a string of at most 256 UTF-8 bytes".into());
    }
    if let Some(query) = item.get("query") {
        if item.get("identifier").is_some() || item.get("field").is_some() {
            return Err("use either query or identifier plus field, not both".into());
        }
        if !(query.as_str().is_some_and(|s| !s.trim().is_empty())
            || query.as_object().is_some_and(|o| !o.is_empty()))
        {
            return Err("`query` must be non-blank text or a nonempty ES query object".into());
        }
    } else {
        text(item, "identifier")?;
        let field = text(item, "field")?;
        if field.contains('.') {
            return Err(
                "`field` must name a flat keyword field; nested fields are not supported".into(),
            );
        }
    }
    Ok(())
}

fn item_body(
    item: &Value,
    index: &str,
    size: usize,
    mapping: Option<&Value>,
) -> Result<Value, String> {
    if item.get("identifier").is_some() {
        let field = text(item, "field")?;
        let mappings = mapping
            .and_then(Value::as_object)
            .ok_or("cannot verify exact identifier field: mapping unavailable")?;
        let mut present = false;
        for m in mappings.values() {
            if let Some(spec) = m.pointer("/mappings/properties").and_then(|p| p.get(field)) {
                present = true;
                if spec["type"] != "keyword" || spec["index"] == false {
                    return Err(format!("exact identifier field `{field}` must be indexed keyword in every mapping where present"));
                }
            }
        }
        if !present {
            return Err(format!("exact identifier field `{field}` is not mapped"));
        }
        return Ok(json!({"query":{"term":{(field):item["identifier"]}},"size":size}));
    }
    let args = json!({"index":index,"query":item["query"],"size":size,"_source":true});
    let (_, _, body) = build_search_with_mapping(&args, mapping)?;
    let mut body = body.expect("search builder always supplies a body");
    if crate::plain_search_demotes_test_paths(&args, mapping) {
        body["size"] = json!(xerj_autoindex::search::search_fetch_size(size));
    }
    Ok(body)
}

fn item_base(position: usize, item: &Value) -> Value {
    let mut out = json!({"position":position});
    if let Some(key) = item
        .get("key")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 256)
    {
        out["key"] = json!(key);
    }
    out
}

fn failed(mut base: Value, error: Value) -> Value {
    base["status"] = json!("error");
    base["error"] = error;
    base
}

fn citation(hit: &Value) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for key in ["_index", "_id"] {
        out.insert(key.into(), json!(text(hit, key)?));
    }
    if let Some(source) = hit.get("_source") {
        for key in [
            "ax_path",
            "path",
            "ax_file",
            "ax_locator",
            "line",
            "start_line",
            "page",
            "cell",
        ] {
            if let Some(v) = source.get(key).filter(|v| v.is_string() || v.is_number()) {
                out.insert(key.into(), v.clone());
            }
        }
    }
    for key in ["match_line", "match_lines"] {
        if let Some(v) = hit.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

fn item_result(
    mut base: Value,
    mut response: Value,
    item: &Value,
    size: usize,
    mapping: Option<&Value>,
) -> Value {
    if response.get("error").is_some_and(|v| !v.is_null())
        || response["status"].as_u64().is_some_and(|s| s >= 400)
    {
        return failed(base, response);
    }
    if response["timed_out"] == true
        || response
            .pointer("/_shards/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
    {
        return failed(
            base,
            json!({"reason":"search_incomplete","response":response}),
        );
    }
    let query = item.get("query").map(|v| {
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string())
    });
    if let Some(query) = query {
        xerj_common::xccode::passage::annotate_search_match_lines(&mut response, &query);
    }
    if crate::plain_search_demotes_test_paths(&json!({"query":item["query"]}), mapping) {
        if let Some(hits) = response
            .pointer_mut("/hits/hits")
            .and_then(Value::as_array_mut)
        {
            xerj_autoindex::def::penalize_demoted_paths(hits);
            hits.truncate(size);
        }
    }
    let Some(hits) = response.pointer("/hits/hits").and_then(Value::as_array) else {
        return failed(
            base,
            json!({"reason":"invalid_search_response","message":"missing hits.hits"}),
        );
    };
    if hits.is_empty() {
        let total = response
            .pointer("/hits/total/value")
            .and_then(Value::as_u64)
            .or_else(|| response.pointer("/hits/total").and_then(Value::as_u64));
        if total.is_some_and(|n| n > 0) {
            return failed(
                base,
                json!({"reason":"invalid_search_response","message":"empty page despite a positive total"}),
            );
        }
        base["status"] = json!("abstain");
        base["abstain"] = json!(true);
        base["reason"] = json!("no_hits");
        return base;
    }
    let mut cited = Vec::new();
    for hit in hits.iter().take(size) {
        let citation = match citation(hit) {
            Ok(c) => c,
            Err(e) => {
                return failed(
                    base,
                    json!({"reason":"invalid_search_response","message":e}),
                )
            }
        };
        cited.push(json!({"citation":citation,"hit":hit}));
    }
    base["status"] = json!("hits");
    base["abstain"] = json!(false);
    base["hits"] = json!(cited);
    base["hits_omitted"] = json!(hits.len().saturating_sub(cited.len()));
    base
}

/// Why one `_msearch` round trip failed. Only an oversized body is worth
/// retrying smaller; every other failure belongs to the whole request.
enum MsearchError {
    TooLarge,
    Failed(String),
}

/// #2694 — one search per entry (its header and body lines), answered in
/// entry order. A response past [`MAX_RESPONSE_BYTES`] is retried as two
/// halves, so large records cost the items that carry them, not the batch;
/// an entry still too large on its own fails alone. Any other failure fails
/// the whole call, as a single `_msearch` always did.
async fn msearch_split(
    ctx: &Ctx,
    entries: &[String],
) -> Result<Vec<Result<Value, String>>, String> {
    let mut out: Vec<Option<Result<Value, String>>> = vec![None; entries.len()];
    let mut ranges = vec![(0, entries.len())];
    while let Some((start, end)) = ranges.pop() {
        match msearch(ctx, entries[start..end].concat()).await {
            Ok(responses) if responses.len() == end - start => {
                for (slot, response) in out[start..end].iter_mut().zip(responses) {
                    *slot = Some(Ok(response));
                }
            }
            Ok(_) => return Err("batch endpoint returned the wrong number of responses; item correspondence cannot be trusted".into()),
            Err(MsearchError::TooLarge) if end - start > 1 => {
                let mid = start + (end - start) / 2;
                // Popped last-in first: the lower half runs first.
                ranges.push((mid, end));
                ranges.push((start, mid));
            }
            Err(MsearchError::TooLarge) => {
                out[start] = Some(Err(
                    "this item's search response alone exceeds 32 MiB; lower size or narrow the query".into(),
                ));
            }
            Err(MsearchError::Failed(e)) => return Err(e),
        }
    }
    Ok(out
        .into_iter()
        .map(|slot| slot.expect("every range is answered or split"))
        .collect())
}

async fn msearch(ctx: &Ctx, ndjson: String) -> Result<Vec<Value>, MsearchError> {
    let url = format!("{}/_msearch", ctx.base_url);
    let mut req = ctx
        .client
        .post(&url)
        .header("Content-Type", "application/x-ndjson")
        .body(ndjson);
    if let Some(auth) = &ctx.auth {
        req = req.header("Authorization", auth);
    }
    let mut resp = req.send().await.map_err(|e| {
        MsearchError::Failed(crate::transport_failure_text(&ctx.base_url, &url, &e))
    })?;
    let status = resp.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| MsearchError::Failed(crate::error_detail(&e)))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(MsearchError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        if let Some(proxied) =
            crate::proxy_status_failure(&ctx.base_url, &url, status.as_u16(), &body)
        {
            return Err(MsearchError::Failed(proxied));
        }
        return Err(MsearchError::Failed(format!(
            "batch endpoint returned HTTP {status}: {body}"
        )));
    }
    let response: Value = serde_json::from_slice(&bytes)
        .map_err(|_| MsearchError::Failed("batch endpoint returned invalid JSON".into()))?;
    response
        .get("responses")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| MsearchError::Failed("batch endpoint returned no responses array".into()))
}

/// Never discard an item's outcome to meet a budget. At least one citation
/// survives each matched item; errors and abstentions retain their meanings.
fn budget(mut response: Value, limit: Option<usize>) -> Result<String, String> {
    let Some(limit) = limit else {
        return Ok(response.to_string());
    };
    let count = |v: &Value| token_budget::count_tokens(&v.to_string());
    if count(&response) <= limit {
        return Ok(response.to_string());
    }
    response["_token_budget"] = json!({"tokenizer":token_budget::TOKENIZER_ID,"max_tokens":limit});
    let mut used = count(&response);
    // Lower ranked items lose raw bodies first, but never their citations.
    let n = response["results"].as_array().unwrap().len();
    for i in (0..n).rev() {
        let len = response["results"][i]["hits"]
            .as_array()
            .map_or(0, Vec::len);
        for j in (0..len).rev() {
            if let Some(hit) = response["results"][i]["hits"][j]
                .as_object_mut()
                .unwrap()
                .remove("hit")
            {
                // JSON punctuation separates fields into word boundaries. Count
                // just the removed subtree rather than serializing a 32 MiB
                // response once for every hit. Final serialization is checked.
                used = used.saturating_sub(count(&json!({"hit":hit})));
            }
            if used <= limit {
                return checked_budget(&response, limit);
            }
        }
    }
    for i in (0..n).rev() {
        while response["results"][i]["hits"]
            .as_array()
            .is_some_and(|a| a.len() > 1)
        {
            let hit = response["results"][i]["hits"]
                .as_array_mut()
                .unwrap()
                .pop()
                .unwrap();
            used = used.saturating_sub(count(&hit));
            let omitted = response["results"][i]["hits_omitted"].as_u64().unwrap_or(0);
            response["results"][i]["hits_omitted"] = json!(omitted + 1);
            if used <= limit {
                return checked_budget(&response, limit);
            }
        }
    }
    let minimum = token_budget::count_tokens(&response.to_string());
    Err(format!("max_tokens {limit} cannot preserve every item outcome and one citation per matched item; this response needs at least {minimum} UAX #29 words. Increase max_tokens or split the batch. Nothing was written."))
}

fn checked_budget(response: &Value, limit: usize) -> Result<String, String> {
    let text = response.to_string();
    if token_budget::count_tokens(&text) <= limit {
        Ok(text)
    } else {
        Err("batch budget accounting could not preserve valid JSON; increase max_tokens or split the batch".into())
    }
}

pub(crate) async fn execute(ctx: &Ctx, args: &Value) -> Result<String, String> {
    let Batch {
        index,
        size,
        limit,
        items,
    } = envelope(args)?;
    // #2497: a bare string item is shorthand for {"query": <string>}.
    let items: Vec<Value> = items
        .iter()
        .map(|item| match item {
            Value::String(s) => json!({ "query": s }),
            other => other.clone(),
        })
        .collect();
    // Validate locally before any I/O; invalid items do not sink siblings.
    let validation: Vec<_> = items.iter().map(validate_item).collect();
    let needs_mapping = items.iter().zip(&validation).any(|(item, valid)| {
        valid.is_ok() && (item.get("identifier").is_some() || item["query"].is_string())
    });
    let mapping = if needs_mapping {
        fetch_mapping(ctx, &index).await
    } else {
        None
    };
    let mut results: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, item)| item_base(i, item))
        .collect();
    let mut pending = Vec::new();
    let mut batch_error = None;
    let mut entries = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let built = validation[i]
            .clone()
            .and_then(|()| item_body(item, &index, size, mapping.as_ref()));
        match built {
            Ok(body) => {
                entries.push(format!("{}\n{}\n", json!({"index":index}), body));
                pending.push(i);
            }
            Err(message) => {
                results[i] = failed(
                    results[i].clone(),
                    json!({"reason":"invalid_item","message":message}),
                )
            }
        }
    }
    if !pending.is_empty() {
        match msearch_split(ctx, &entries).await {
            Ok(responses) => {
                for (i, response) in pending.into_iter().zip(responses) {
                    results[i] = match response {
                        Ok(response) => item_result(
                            results[i].clone(),
                            response,
                            &items[i],
                            size,
                            mapping.as_ref(),
                        ),
                        Err(message) => failed(
                            results[i].clone(),
                            json!({"reason":"response_too_large","message":message}),
                        ),
                    };
                }
            }
            Err(message) => {
                for i in pending {
                    results[i] = failed(
                        results[i].clone(),
                        json!({"reason":"batch_request_failed","details":"see batch_error"}),
                    );
                }
                // Preserve the complete transport error once, rather than
                // multiplying a large HTTP error body by up to 50 items.
                batch_error = Some(message);
            }
        }
    }
    let mut response = json!({"results":results});
    if let Some(error) = batch_error {
        response["batch_error"] = json!(error);
    }
    budget(response, limit)
}

pub(crate) async fn run(ctx: &Ctx, args: &Value) -> Value {
    match execute(ctx, args).await {
        Ok(text) => tool_text(text, false),
        Err(error) => tool_text(error, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_budget_keeps_outcomes_and_citations_or_refuses() {
        let response = json!({"results":[
            {"position":0,"status":"hits","abstain":false,"hits_omitted":0,
             "hits":[{"citation":{"_index":"docs","_id":"one"},"hit":{"body":"word ".repeat(1000)}},
                     {"citation":{"_index":"docs","_id":"two"},"hit":{"body":"other ".repeat(1000)}}]},
            {"position":1,"status":"abstain","abstain":true,"reason":"no_hits"},
            {"position":2,"status":"error","error":{"reason":"backend_failed"}}
        ]});
        assert_eq!(
            budget(response.clone(), Some(10000)).unwrap(),
            response.to_string()
        );
        for limit in [32, 64, 80, 120, 300, 3000] {
            match budget(response.clone(), Some(limit)) {
                Ok(text) => {
                    assert!(token_budget::count_tokens(&text) <= limit);
                    let v: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(v["results"].as_array().unwrap().len(), 3);
                    assert_eq!(v["results"][0]["hits"][0]["citation"]["_id"], "one");
                    assert_eq!(v["results"][1]["status"], "abstain");
                    assert_eq!(v["results"][2]["status"], "error");
                }
                Err(e) => assert!(e.contains("needs at least"), "{e}"),
            }
        }
    }
}
