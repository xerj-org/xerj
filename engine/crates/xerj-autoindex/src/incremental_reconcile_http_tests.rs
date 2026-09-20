//! Real-HTTP end-to-end coverage for incremental corpus generations.
//!
//! The endpoint below is deliberately stateful. It applies bulk index/update
//! actions, delete-by-query, and exact count searches so `run_index` traverses
//! the same HTTP client and validation barriers as a real server.

use super::*;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

static HTTP_E2E_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct HttpState {
    docs: HashMap<(String, String), Value>,
    requests: Vec<(String, String)>,
    data_bulk_requests: usize,
    fail_next_data_bulk: bool,
    /// Apply the first half of the next data bulk's items and *then* report
    /// `errors: true`. This is the dangerous backend shape the legacy
    /// replacement transaction was hardened against (visibility changed, but
    /// the durable record must not be): `no_graph` corpora take the generated
    /// executor now, so the shape has to be proven here too.
    partially_apply_next_data_bulk: bool,
    fail_embedding_identity: bool,
    stop: bool,
    embedding_identity_sha256: String,
    saw_dataset_mapping_update: bool,
    saw_catalog_mapping_update: bool,
    /// Opt-in (#755/#760): the fields this catalog already holds as `text`,
    /// standing in for an older-build (v1.0.0-rc.15..rc.67) catalog. A catalog
    /// `_mapping` PUT that still declares one of them fails with the exact
    /// `field [X] already exists as [text]` conflict a real engine produces;
    /// a PUT that no longer declares any of them is acknowledged, exactly like
    /// the engine. Empty by default — every other test's faithful-accept
    /// behaviour is unchanged.
    ///
    /// #890: it also governs how a `term` on such a field MATCHES, in searches
    /// and in delete-by-query alike. A legacy field is not merely undeclarable,
    /// it is analyzed, and that is the whole defect — see [`term_matches`].
    legacy_text_catalog_fields: Vec<String>,
    /// Opt-in (#755): fail the catalog `_mapping` PUT with a 503 instead. Not a
    /// type conflict, so it must still abort the run.
    catalog_mapping_unavailable: bool,
    /// The `properties` object of the last catalog `_mapping` PUT the endpoint
    /// ACKNOWLEDGED — what a legacy catalog actually ends up declaring.
    installed_catalog_properties: Option<Value>,
    /// How many catalog `_mapping` PUTs were attempted (the drop-and-retry
    /// loop's cost).
    catalog_mapping_puts: usize,
    /// Opt-in (#929): a DATASET `_mapping` PUT whose index name contains one of
    /// these is refused with the 400 a real engine returns for a field it
    /// cannot map — the literal response that aborted the 48,533-file run.
    /// A refusal is a statement about that one dataset.
    refused_dataset_mappings: Vec<String>,
    /// Opt-in (#929): the same selection, answered 503 instead. That is the
    /// endpoint failing, says nothing about the dataset, and must still abort.
    unavailable_dataset_mappings: Vec<String>,
    /// Every dataset index a `_mapping` PUT was attempted for, in order.
    dataset_mapping_puts: Vec<String>,
    /// Opt-in (#931): fail the Nth data bulk (1-based) the way
    /// `fail_next_data_bulk` fails the next one. Lets a replay be interrupted
    /// AFTER some operations committed, which is the only state in which
    /// "count what remains" differs from "count everything".
    fail_data_bulk_number: Option<usize>,
    /// Opt-in (#944): apply the even-position actions of the next data bulk
    /// and answer the odd-position ones with the per-item 429 the engine's
    /// memory circuit breaker produces while it is engaged. The client must
    /// re-send exactly the odd ones.
    throttle_next_data_bulk_odd_items: bool,
    /// Opt-in (#944, second shape): answer the next N data bulks HTTP 429 as
    /// a whole, applying nothing — how the engine answers when its memory
    /// circuit breaker rejects the request, and the literal end of the resumed
    /// full-corpus run (`error: _bulk: HTTP 429 Too Many Requests`).
    throttle_whole_data_bulks: usize,
    /// Action count of every data bulk answered, in order — how a test proves
    /// a re-send carried only the rejected items.
    data_bulk_item_counts: Vec<usize>,
    /// Opt-in (#955): the engine's `limits.max_actions_per_bulk`. A `_bulk` of
    /// more than twice this many lines — data or catalog — is refused whole,
    /// before anything is applied, with the engine's literal answer: HTTP 200
    /// carrying ONE item of status 413. That response ended the 48,533-file
    /// run in `finalize-catalog` after every operation had been applied.
    max_actions_per_bulk: Option<usize>,
    /// Line count of every bulk refused by `max_actions_per_bulk`, in order.
    oversize_bulks_refused: Vec<usize>,
    /// Line count of every bulk that got past `max_actions_per_bulk`.
    bulk_line_counts: Vec<usize>,
}

struct HttpEndpoint {
    url: String,
    state: Arc<Mutex<HttpState>>,
    join: Option<thread::JoinHandle<()>>,
}

impl HttpEndpoint {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(HttpState {
            embedding_identity_sha256: "a".repeat(64),
            ..HttpState::default()
        }));
        let server_state = Arc::clone(&state);
        let join = thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // BSD/macOS: accepted sockets inherit the listener's
                    // O_NONBLOCK; the handler does blocking reads.
                    stream.set_nonblocking(false).unwrap();
                    handle_http(stream, &server_state)
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if server_state.lock().unwrap().stop {
                        break;
                    }
                    thread::yield_now();
                }
                Err(error) => panic!("HTTP test endpoint accept failed: {error}"),
            }
        });
        Self {
            url,
            state,
            join: Some(join),
        }
    }

    fn data_docs(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .docs
            .iter()
            .filter(|((index, _), _)| index != catalog::CATALOG_INDEX)
            .map(|(_, doc)| doc.clone())
            .collect()
    }

    fn data_bulk_requests(&self) -> usize {
        self.state.lock().unwrap().data_bulk_requests
    }

    /// #755: the catalog mapping the endpoint finally acknowledged.
    fn installed_catalog_properties(&self) -> Value {
        self.state
            .lock()
            .unwrap()
            .installed_catalog_properties
            .clone()
            .expect("no catalog _mapping PUT was acknowledged")
    }

    /// #755: every catalog document the run published.
    fn catalog_docs(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .docs
            .iter()
            .filter(|((index, _), _)| index == catalog::CATALOG_INDEX)
            .map(|(_, doc)| doc.clone())
            .collect()
    }
}

impl Drop for HttpEndpoint {
    fn drop(&mut self) {
        self.state.lock().unwrap().stop = true;
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        self.join.take().unwrap().join().unwrap();
    }
}

fn handle_http(mut stream: TcpStream, state: &Arc<Mutex<HttpState>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap() == 0 {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
        {
            content_length = value.parse().unwrap();
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    state
        .lock()
        .unwrap()
        .requests
        .push((method.to_owned(), path.to_owned()));

    let (status, response) = if method == "GET" && path == "/v1/embedding/identity" {
        let locked = state.lock().unwrap();
        if locked.fail_embedding_identity {
            (
                500,
                json!({"error": {
                    "type": "injected_identity_failure",
                    "reason": "genesis recovery boundary"
                }}),
            )
        } else {
            (
                200,
                json!({"data": {
                    "version": 1,
                    "backend": "lexical",
                    "identity_sha256": locked.embedding_identity_sha256.clone(),
                    "dimensions": 384,
                    "semantic_contract": "semantic_text-derived-vector.v1",
                    "resumable": true
                }, "took_ms": 0, "request_id": "incremental-http-test"}),
            )
        }
    } else if method == "POST" && path == "/_bulk" {
        let locked = state.lock().unwrap();
        assert!(
            locked.saw_dataset_mapping_update && locked.saw_catalog_mapping_update,
            "bulk publication preceded exact generation mapping updates"
        );
        drop(locked);
        bulk_http(&body, state)
    } else if method == "POST" && path.contains("/_delete_by_query") {
        let deleted = delete_http(path, &body, state);
        (200, json!({"deleted": deleted, "failures": []}))
    } else if method == "POST" && path.ends_with("/_search") {
        (200, search_http(path, &body, state))
    } else if method == "GET" && path.ends_with("/_count") {
        let index = path.trim_start_matches('/').trim_end_matches("/_count");
        let count = state
            .lock()
            .unwrap()
            .docs
            .keys()
            .filter(|(candidate, _)| candidate == index)
            .count();
        (200, json!({"count": count}))
    } else if method == "PUT" {
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let mut locked = state.lock().unwrap();
        // #755: the first still-declared field this catalog already holds as
        // `text`, if any — the engine reports one conflict per request.
        let mut conflict: Option<String> = None;
        let mut unavailable = false;
        let mut refused_dataset: Option<String> = None;
        if path.ends_with("/_mapping") {
            assert!(value.get("properties").is_some());
            assert!(value.get("mappings").is_none());
            if path.starts_with(&format!("/{}/", catalog::CATALOG_INDEX)) {
                locked.saw_catalog_mapping_update = true;
                locked.catalog_mapping_puts += 1;
                unavailable = locked.catalog_mapping_unavailable;
                let declared = value["properties"].as_object().cloned().unwrap_or_default();
                conflict = locked
                    .legacy_text_catalog_fields
                    .iter()
                    .find(|field| declared.contains_key(field.as_str()))
                    .cloned();
                if conflict.is_none() && !unavailable {
                    locked.installed_catalog_properties = Some(Value::Object(declared));
                }
            } else {
                locked.saw_dataset_mapping_update = true;
                let index = path
                    .trim_start_matches('/')
                    .trim_end_matches("/_mapping")
                    .to_owned();
                let selected = |needles: &[String]| {
                    needles.iter().any(|needle| index.contains(needle.as_str()))
                };
                unavailable = selected(&locked.unavailable_dataset_mappings);
                if selected(&locked.refused_dataset_mappings) {
                    refused_dataset = Some(index.clone());
                }
                locked.dataset_mapping_puts.push(index);
            }
        } else {
            assert!(value.pointer("/mappings/properties").is_some());
            assert!(value.get("properties").is_none());
        }
        if let Some(index) = refused_dataset {
            // Byte for byte the shape of the refusal in #929.
            let reason = format!(
                "nested semantic_text field [{index}.unmappable] is not supported; map \
                 semantic_text as a top-level field"
            );
            (
                400,
                json!({
                    "error": {
                        "root_cause": [{"type": "mapper_parsing_exception", "reason": reason}],
                        "type": "mapper_parsing_exception",
                        "reason": reason
                    },
                    "status": 400
                }),
            )
        } else if unavailable {
            (
                503,
                json!({"error": {"type": "unavailable_shards_exception", "reason": "no node"}}),
            )
        } else if let Some(field) = conflict {
            let reason = format!("field [{field}] already exists as [text], cannot add [keyword]");
            (
                400,
                json!({
                    "error": {
                        "root_cause": [{"type": "mapper_parsing_exception", "reason": reason}],
                        "type": "mapper_parsing_exception",
                        "reason": reason
                    },
                    "status": 400
                }),
            )
        } else {
            (200, json!({"acknowledged": true}))
        }
    } else {
        // Readiness, index creation/mapping, and refresh.
        (200, json!({"acknowledged": true}))
    };
    let bytes = response.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        match status {
            200 => "OK",
            400 => "Bad Request",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Internal Server Error",
        },
        bytes.len(),
        bytes
    )
    .unwrap();
}

fn bulk_http(body: &[u8], state: &Arc<Mutex<HttpState>>) -> (u16, Value) {
    let lines: Vec<&[u8]> = body
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    let is_data = lines.iter().step_by(2).any(|line| {
        serde_json::from_slice::<Value>(line)
            .ok()
            .and_then(|action| {
                action
                    .pointer("/index/_index")
                    .or_else(|| action.pointer("/update/_index"))
                    .and_then(Value::as_str)
                    .map(|index| index != catalog::CATALOG_INDEX)
            })
            .unwrap_or(false)
    });
    let mut locked = state.lock().unwrap();
    if let Some(max_actions) = locked.max_actions_per_bulk {
        // Checked first and by LINES, as `xerj-engine/src/bulk.rs` does.
        if lines.len() > max_actions * 2 {
            locked.oversize_bulks_refused.push(lines.len());
            return (
                200,
                json!({
                    "took": 0,
                    "errors": true,
                    "items": [{"index": {
                        "_index": "",
                        "_id": "",
                        "status": 413,
                        "error": {
                            "type": "engine_exception",
                            "reason": format!(
                                "bulk request contains {} lines (~{} actions); exceeds \
                                 max_actions_per_bulk of {max_actions}",
                                lines.len(),
                                lines.len() / 2
                            ),
                            "status": 413
                        }
                    }}]
                }),
            );
        }
        locked.bulk_line_counts.push(lines.len());
    }
    if is_data {
        locked.data_bulk_requests += 1;
        let numbered = locked.fail_data_bulk_number == Some(locked.data_bulk_requests);
        if numbered {
            locked.fail_data_bulk_number = None;
        }
        if std::mem::take(&mut locked.fail_next_data_bulk) || numbered {
            return (
                200,
                json!({
                    "errors": true,
                    "items": [{"index": {
                        "status": 500,
                        "error": {
                            "type": "injected_failure",
                            "reason": "leave durable generation pending"
                        }
                    }}]
                }),
            );
        }
    }
    if is_data && locked.throttle_whole_data_bulks > 0 {
        locked.throttle_whole_data_bulks -= 1;
        let mut actions = 0usize;
        let mut cursor = 0;
        while cursor < lines.len() {
            let action: Value = serde_json::from_slice(lines[cursor]).unwrap();
            cursor += if action.get("delete").is_some() { 1 } else { 2 };
            actions += 1;
        }
        locked.data_bulk_item_counts.push(actions);
        return (
            429,
            json!({
                "error": {
                    "type": "circuit_breaking_exception",
                    "reason": "[parent] real memory circuit breaker tripped: rss=15634MB >= watermark=15564MB (94% of limit=16384MB); writes rejected to prevent an out-of-memory kill"
                },
                "status": 429
            }),
        );
    }
    if is_data && std::mem::take(&mut locked.throttle_next_data_bulk_odd_items) {
        let mut items = Vec::new();
        let mut cursor = 0;
        while cursor < lines.len() {
            let action: Value = serde_json::from_slice(lines[cursor]).unwrap();
            cursor += 1;
            let position = items.len();
            if let Some(meta) = action.get("delete") {
                let index = meta["_index"].as_str().unwrap().to_owned();
                let id = meta["_id"].as_str().unwrap().to_owned();
                locked.docs.remove(&(index, id));
                items.push(json!({"delete": {"status": 200}}));
                continue;
            }
            let payload: Value = serde_json::from_slice(lines[cursor]).unwrap();
            cursor += 1;
            if position % 2 == 1 {
                items.push(json!({"index": {
                    "status": 429,
                    "error": {
                        "type": "engine_exception",
                        "reason": "[parent] real memory circuit breaker tripped: rss=15679MB >= watermark=15564MB (94% of limit=16384MB); writes rejected to prevent an out-of-memory kill",
                        "status": 429
                    }
                }}));
                continue;
            }
            if let Some(meta) = action.get("index") {
                let index = meta["_index"].as_str().unwrap().to_owned();
                let id = meta["_id"].as_str().unwrap().to_owned();
                locked.docs.insert((index, id), payload);
            } else if let Some(meta) = action.get("update") {
                let index = meta["_index"].as_str().unwrap().to_owned();
                let id = meta["_id"].as_str().unwrap().to_owned();
                let patch = payload["doc"].as_object().unwrap();
                let target = locked.docs.get_mut(&(index, id)).unwrap();
                let target = target.as_object_mut().unwrap();
                target.extend(patch.clone());
            }
            items.push(json!({"index": {"status": 201}}));
        }
        locked.data_bulk_item_counts.push(items.len());
        return (200, json!({"errors": true, "items": items}));
    }
    // Half-applied bulk: apply the leading actions, then report failure.
    let applied_limit = if is_data && std::mem::take(&mut locked.partially_apply_next_data_bulk) {
        Some(lines.len() / 2)
    } else {
        None
    };
    let mut cursor = 0;
    let mut actions = 0usize;
    while cursor < lines.len() {
        if applied_limit.is_some_and(|limit| cursor >= limit) {
            break;
        }
        let action: Value = serde_json::from_slice(lines[cursor]).unwrap();
        cursor += 1;
        actions += 1;
        if let Some(meta) = action.get("delete") {
            let index = meta["_index"].as_str().unwrap().to_owned();
            let id = meta["_id"].as_str().unwrap().to_owned();
            locked.docs.remove(&(index, id));
            continue;
        }
        let payload: Value = serde_json::from_slice(lines[cursor]).unwrap();
        cursor += 1;
        if let Some(meta) = action.get("index") {
            let index = meta["_index"].as_str().unwrap().to_owned();
            let id = meta["_id"].as_str().unwrap().to_owned();
            locked.docs.insert((index, id), payload);
        } else if let Some(meta) = action.get("update") {
            let index = meta["_index"].as_str().unwrap().to_owned();
            let id = meta["_id"].as_str().unwrap().to_owned();
            let patch = payload["doc"].as_object().unwrap();
            let target = locked.docs.get_mut(&(index, id)).unwrap();
            let target = target.as_object_mut().unwrap();
            target.extend(patch.clone());
        }
    }
    if is_data {
        locked.data_bulk_item_counts.push(actions);
    }
    if applied_limit.is_some() {
        return (
            200,
            json!({
                "errors": true,
                "items": [{"index": {
                    "status": 500,
                    "error": {
                        "type": "injected_failure",
                        "reason": "partial bulk applied before failing"
                    }
                }}]
            }),
        );
    }
    (200, json!({"errors": false, "items": []}))
}

/// Does a `term` query for `expected` on `field` match `doc`, given the fields
/// this catalog holds as legacy `text` (#890)?
///
/// A `term` query is NOT analyzed, but the FIELD is. On a `keyword` field the
/// match is byte-exact, which is what every other test relies on. On a `text`
/// field the term is compared against the stored value's analyzed TOKENS, so
/// `{"term": {"prefix": "ax"}}` matches a document whose prefix is `ax-2`
/// (standard-analyzer tokens `[ax, 2]`) — the cross-corpus reach #890 is about.
/// Tokenisation here is the standard analyzer's shape for these values: split on
/// non-alphanumerics, lowercased.
fn term_matches(doc: &Value, field: &str, expected: &Value, legacy_text: &[String]) -> bool {
    if doc.get(field) == Some(expected) {
        return true;
    }
    if !legacy_text.iter().any(|legacy| legacy == field) {
        return false;
    }
    let (Some(stored), Some(term)) = (doc.get(field).and_then(Value::as_str), expected.as_str())
    else {
        return false;
    };
    stored
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(|token| token.to_lowercase() == term)
}

fn delete_http(path: &str, body: &[u8], state: &Arc<Mutex<HttpState>>) -> usize {
    let index = path
        .trim_start_matches('/')
        .split("/_delete_by_query")
        .next()
        .unwrap();
    let query: Value = serde_json::from_slice(body).unwrap();
    // Collect every term constraint: the single `/query/term` form AND each
    // `/query/bool/filter[].term` (#737 scopes catalog deletes with a
    // `prefix` + `path`/`file_key` bool filter). A doc is deleted iff it
    // matches EVERY collected term.
    let mut terms: Vec<(String, Value)> = Vec::new();
    // #905: and every `/query/bool/filter[].terms` value ARRAY too — the
    // duplicate-alias sweep's path list is one of those, and an endpoint that
    // dropped it deleted at every path instead of at the named ones, which
    // makes a delete look wider than the engine's.
    let mut any_of: Vec<(String, Vec<Value>)> = Vec::new();
    if let Some((field, value)) = query
        .pointer("/query/term")
        .and_then(Value::as_object)
        .and_then(|term| term.iter().next())
    {
        terms.push((field.clone(), value.clone()));
    }
    if let Some(filters) = query
        .pointer("/query/bool/filter")
        .and_then(Value::as_array)
    {
        for filter in filters {
            if let Some((field, value)) = filter
                .pointer("/term")
                .and_then(Value::as_object)
                .and_then(|term| term.iter().next())
            {
                terms.push((field.clone(), value.clone()));
            }
            if let Some((field, values)) = filter
                .pointer("/terms")
                .and_then(Value::as_object)
                .and_then(|terms| terms.iter().next())
            {
                any_of.push((
                    field.clone(),
                    values.as_array().cloned().unwrap_or_default(),
                ));
            }
        }
    }
    // #739: an `ids` query deletes by exact `_id` (the main `file:` doc sweep).
    let ids: Option<Vec<String>> = query
        .pointer("/query/ids/values")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });
    let mut locked = state.lock().unwrap();
    let legacy_text = locked.legacy_text_catalog_fields.clone();
    let before = locked.docs.len();
    locked.docs.retain(|(candidate, id), doc| {
        if candidate != index {
            return true;
        }
        if let Some(ids) = &ids {
            // ids query: delete iff this doc's _id is listed (nothing else).
            return !ids.contains(id);
        }
        if terms.is_empty() && any_of.is_empty() {
            return false; // no constraint: a match-all delete
        }
        let legacy: &[String] = if candidate == catalog::CATALOG_INDEX {
            &legacy_text
        } else {
            &[]
        };
        // Retain (do NOT delete) unless the doc matches every term — #890: on
        // the catalog's legacy text fields, "matches" means the analyzed tokens
        // — and lands inside every `terms` value list (#905).
        !(terms
            .iter()
            .all(|(field, expected)| term_matches(doc, field, expected, legacy))
            && any_of.iter().all(|(field, values)| {
                values
                    .iter()
                    .any(|value| term_matches(doc, field, value, legacy))
            }))
    });
    before - locked.docs.len()
}

fn search_http(path: &str, body: &[u8], state: &Arc<Mutex<HttpState>>) -> Value {
    let indices = path
        .trim_start_matches('/')
        .trim_end_matches("/_search")
        .split(',')
        .collect::<Vec<_>>();
    let query: Value = serde_json::from_slice(body).unwrap();
    let term = query
        .pointer("/query/term")
        .or_else(|| query.pointer("/query/bool/must/0/term"))
        .and_then(Value::as_object)
        .and_then(|term| term.iter().next());
    let terms = query
        .pointer("/query/terms")
        .and_then(Value::as_object)
        .and_then(|terms| terms.iter().next());
    let filters = query
        .pointer("/query/bool/filter")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let exists = query
        .pointer("/query/bool/must/1/exists/field")
        .and_then(Value::as_str);
    // #694: `invalidate_prior_edges` filters live edges with
    // `must_not:[{exists:{field:"invalid_at"}}]`; honour it so the pass
    // converges (an already-invalidated edge must drop out of the result).
    let must_not_exists = query
        .pointer("/query/bool/must_not/0/exists/field")
        .and_then(Value::as_str);
    let locked = state.lock().unwrap();
    // #890: a catalog field an older build left as `text` is ANALYZED, so a
    // `term` on it matches tokens, not the raw value. `legacy` is empty for
    // every other index, which keeps every other search byte-exact.
    let legacy_text = locked.legacy_text_catalog_fields.clone();
    let legacy_for = |index: &str| -> &[String] {
        if index == catalog::CATALOG_INDEX {
            &legacy_text
        } else {
            &[]
        }
    };
    let mut matching: Vec<_> = locked
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            indices.contains(&index.as_str())
                && term
                    .map(|(field, value)| term_matches(doc, field, value, legacy_for(index)))
                    .unwrap_or(true)
                && terms
                    .map(|(field, values)| {
                        values.as_array().is_some_and(|values| {
                            values.iter().any(|value| doc.get(field) == Some(value))
                        })
                    })
                    .unwrap_or(true)
                && filters.iter().all(|filter| {
                    filter
                        .get("term")
                        .and_then(Value::as_object)
                        .and_then(|term| term.iter().next())
                        .is_none_or(|(field, value)| {
                            term_matches(doc, field, value, legacy_for(index))
                        })
                        // A per-dataset read-back scopes `terms` on `ax_file`
                        // and `term` on `ax_dataset` in the same `bool.filter`,
                        // so an endpoint that honoured only the latter would
                        // count every file's records into every dataset.
                        && filter
                            .get("terms")
                            .and_then(Value::as_object)
                            .and_then(|terms| terms.iter().next())
                            .is_none_or(|(field, values)| {
                                values.as_array().is_some_and(|values| {
                                    values.iter().any(|value| doc.get(field) == Some(value))
                                })
                            })
                })
                && exists.map(|field| doc.get(field).is_some()).unwrap_or(true)
                && must_not_exists
                    .map(|field| doc.get(field).is_none())
                    .unwrap_or(true)
        })
        .collect();
    // #890: honour `from`/`size` when the request states them — the scoped
    // catalog sweep pages, and a page has to be a page. A request that omits
    // them still gets everything, which is what every older fixture assumes.
    // Ordering is by `_id` so a page boundary is not `HashMap` iteration order.
    matching.sort_by_key(|(key, _)| *key);
    let total = matching.len();
    let from = query
        .get("from")
        .and_then(Value::as_u64)
        .map_or(0usize, |from| from as usize);
    let size = query
        .get("size")
        .and_then(Value::as_u64)
        .map_or(total, |size| size as usize);
    let hits = matching
        .iter()
        .skip(from)
        .take(size)
        .map(|((_, id), source)| json!({"_id": id, "_source": source}))
        .collect::<Vec<_>>();
    json!({
        "hits": {
            "total": {"value": total, "relation": "eq"},
            "hits": hits
        },
        "aggregations": {}
    })
}

fn cfg(root: &Path, state_dir: &Path, url: &str, semantic: bool) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        endpoint_url: None,
        stub_globs: Vec::new(),
        url: url.to_owned(),
        api_key: None,
        api_key_file: None,
        workers: 1,
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
        xerj_url_note: None,
        pdf_timeout_secs: 30,
        bulk_mb: 1,
        bulk_timeout_secs: 30,
        snapshot_max_bytes: 64 << 30,
        prefix: "incremental-http".into(),
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        follow_symlinks_outside_root: false,
        ignore: crate::ignore_rules::IgnoreOptions::default(),
        max_file_gb: 1,
        sample: 50,
        no_semantic: !semantic,
        brain: None,
        no_graph: true,
        // Gate off: these fixtures assert reconcile behaviour, not a
        // timing-derived stop. See `gate_tests` and `cli::tests`.
        max_minutes: 0,
        approve: None,
        dry_run: false,
        json: false,
        quiet: true,
        progress: crate::progress::ProgressMode::None,
        progress_interval: None,
        watch: false,
        debounce: std::time::Duration::from_millis(0),
    }
}

fn journal_events(state_dir: &Path, kind: &str) -> usize {
    fs::read_to_string(state_dir.join("journal.ndjson"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some(kind))
        .count()
}

fn paths(docs: &[Value]) -> Vec<String> {
    let mut paths: Vec<_> = docs
        .iter()
        .filter_map(|doc| doc.get("ax_path").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn snapshot_names(state_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(state_dir.join("sync-snapshots"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && !entry.file_name().to_string_lossy().starts_with('.')
                })
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn final_snapshot_count(state_dir: &Path) -> usize {
    fs::read_dir(state_dir.join("sync-snapshots"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && !entry.file_name().to_string_lossy().starts_with('.')
                })
                .count()
        })
        .unwrap_or(0)
}

/// #755: an older-build global catalog holds `prefix` (and, on a fully dynamic
/// catalog, `doc_kind`) mapped as `text`, so the generation's keyword `_mapping`
/// update conflicts. Before this fix the whole run aborted — and because
/// `autoindex-catalog` is ONE index shared by every corpus on the node, that
/// bricked `xerj autoindex` outright for anyone upgrading from v1.0.0-rc.15..67.
///
/// An upgrade must never abort a run over a mapping an earlier release left
/// behind: the run has to complete, every field the engine will accept has to be
/// installed, and corpus scoping has to keep working — which it does by moving
/// onto `catalog::CORPUS_SCOPE_FIELD`, a keyword field no older release wrote.
///
/// End-to-end on purpose (#760): `install_catalog_mapping`'s own unit tests pass
/// even if the wiring at the catalog `update_mapping` is dropped, so this is what
/// actually guards it.
#[test]
fn a_legacy_text_mapped_catalog_completes_the_run_instead_of_aborting() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().legacy_text_catalog_fields =
        vec!["prefix".into(), "doc_kind".into()];
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    // The run COMPLETES. This is the whole bug: it used to abort here.
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(endpoint.data_docs().len(), 1);

    // Exactly the two legacy fields were given up, and nothing else.
    let installed = endpoint.installed_catalog_properties();
    let installed = installed.as_object().expect("properties object");
    assert!(
        !installed.contains_key("prefix") && !installed.contains_key("doc_kind"),
        "a field the catalog already holds as text cannot be re-declared: {installed:?}"
    );
    assert_eq!(
        installed["path"]["type"], "keyword",
        "every non-conflicting field must still be installed: {installed:?}"
    );
    // …including the keyword field corpus scoping moved onto. No release ever
    // wrote it, so a legacy catalog cannot be holding it as text.
    assert_eq!(
        installed[catalog::CORPUS_SCOPE_FIELD]["type"],
        "keyword",
        "the corpus-scope field must survive the legacy conflict: {installed:?}"
    );

    // And the published catalog documents carry the scope on that field, so the
    // #737/#693 scoped sweeps stay exact on this node.
    let scoped = endpoint
        .catalog_docs()
        .into_iter()
        .filter(|doc| doc.get("doc_kind").and_then(Value::as_str) == Some("file"))
        .collect::<Vec<_>>();
    assert!(!scoped.is_empty(), "the run published no file catalog docs");
    for doc in &scoped {
        assert_eq!(
            doc[catalog::CORPUS_SCOPE_FIELD],
            "incremental-http",
            "every file catalog doc must carry the keyword corpus scope: {doc}"
        );
    }
}

/// #755: the conflict tolerance is narrow on purpose. A catalog `_mapping` PUT
/// that fails for any reason OTHER than a legacy field type — an unreachable
/// node, a 503, an unrelated 400 — is a real failure and must still abort,
/// rather than being retried into a silently narrower catalog mapping.
#[test]
fn an_unavailable_catalog_mapping_still_aborts_the_run() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().catalog_mapping_unavailable = true;
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let msg = format!("{:#}", run_index(config).unwrap_err());
    assert!(
        msg.contains("install generation catalog mapping") && msg.contains("503"),
        "an unrelated mapping failure must abort with its own reason: {msg}"
    );
    assert_eq!(
        endpoint.state.lock().unwrap().catalog_mapping_puts,
        1,
        "a non-conflict failure must not be retried: {msg}"
    );
}

/// #755: tolerating the legacy mapping is not the same as hiding it. Documents
/// an older build already wrote keep the legacy type, so a scoped sweep can
/// still miss them — the operator has to be told, through the progress surface
/// that owns stderr (#241), with the reindex that retires the field.
#[test]
fn a_legacy_text_mapped_catalog_warns_through_the_progress_surface() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().legacy_text_catalog_fields = vec!["prefix".into()];
    let es = Es::with_bulk_timeout(&endpoint.url, None, 30).expect("es client");
    let (pr, sink) = progress::Progress::capture(
        progress::Surface::Plain,
        std::time::Duration::from_secs(3600),
    );

    ensure_generation_mappings(&es, &Plan::default(), &pr).expect("legacy catalog must not abort");

    let text = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("older build") && text.contains("`prefix`"),
        "the operator must be told which field is legacy: {text}"
    );
    assert!(
        text.contains("run CONTINUES") && text.contains("_reindex"),
        "…and what happened plus how to retire it: {text}"
    );
    assert!(
        text.contains(catalog::CORPUS_SCOPE_FIELD),
        "…and where corpus scoping moved to: {text}"
    );
}

/// #755: a healthy catalog is untouched — one `_mapping` PUT, nothing dropped.
#[test]
fn a_current_catalog_installs_its_mapping_in_a_single_put() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let endpoint = HttpEndpoint::start();
    let es = Es::with_bulk_timeout(&endpoint.url, None, 30).expect("es client");
    let (pr, sink) = progress::Progress::capture(
        progress::Surface::Plain,
        std::time::Duration::from_secs(3600),
    );

    ensure_generation_mappings(&es, &Plan::default(), &pr).expect("fresh catalog");

    assert_eq!(endpoint.state.lock().unwrap().catalog_mapping_puts, 1);
    assert_eq!(
        endpoint.installed_catalog_properties()["prefix"]["type"],
        "keyword",
        "a catalog with no legacy field keeps the full declared mapping"
    );
    assert!(
        String::from_utf8(sink.lock().unwrap().clone())
            .unwrap()
            .is_empty(),
        "a healthy catalog must not warn"
    );
}

/// Rewrite a journal so every generation in it carries the `index_identity` the
/// PREVIOUSLY SHIPPED release froze for the same plan — the exact on-disk state
/// of an install that is being upgraded.
///
/// The digests that bind the log are recomputed with it (`desired_manifest_digest`
/// on the begin/validate/commit records, and the next generation's
/// `base_manifest_digest`), so the journal stays internally valid and the only
/// thing that has moved is the frozen contract identity. Returns the identity
/// written in (previous release) and the one this build had written (this
/// build); while the contract holds they are equal and the rewrite is a
/// byte-for-byte no-op — that is the property under test.
fn freeze_journal_identities_as_previous_release(state_dir: &Path) -> (String, String) {
    let path = state_dir.join("journal.ndjson");
    let raw = fs::read_to_string(&path).unwrap();
    let mut previous_release = None;
    let mut this_build = None;
    let mut committed_digest: Option<String> = None;
    let mut pending_digest: Option<String> = None;
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(mut record) = serde_json::from_str::<Value>(line) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        match record.get("kind").and_then(Value::as_str) {
            Some("sync_begin") => {
                if let Some(digest) = &committed_digest {
                    record["base_manifest_digest"] = json!(digest);
                }
                let plan: Plan = serde_json::from_value(record["desired"]["plan"].clone()).unwrap();
                let frozen = frozen_contract::index_identity(&plan);
                this_build = record["desired"]["execution"]["index_identity"]
                    .as_str()
                    .map(str::to_owned);
                record["desired"]["execution"]["index_identity"] = json!(frozen);
                previous_release = Some(frozen);
                let manifest: crate::sync::GenerationManifest =
                    serde_json::from_value(record["desired"].clone()).unwrap();
                let digest = crate::sync::manifest_digest(&manifest).unwrap();
                record["desired_manifest_digest"] = json!(digest);
                pending_digest = Some(digest);
            }
            Some("sync_validated") | Some("sync_abort") => {
                record["desired_manifest_digest"] = json!(pending_digest.clone().unwrap());
            }
            Some("sync_commit") => {
                let digest = pending_digest.clone().unwrap();
                record["desired_manifest_digest"] = json!(digest);
                committed_digest = Some(digest);
            }
            _ => {}
        }
        out.push_str(&serde_json::to_string(&record).unwrap());
        out.push('\n');
    }
    fs::write(&path, out).unwrap();
    (
        previous_release.expect("the journal has a sync_begin"),
        this_build.expect("the sync_begin carries an execution identity"),
    )
}

/// #755 upgrade path, end to end: generations frozen by the PREVIOUS RELEASE
/// must still be reconcilable by this build.
///
/// `index_identity` is hashed out of `catalog::catalog_mapping()` and frozen in
/// the journal's execution record, and two hard `ensure!`s then demand equality
/// against it: `provision_generation` when a pending (mid-commit) generation is
/// replayed ("desired generation index identity disagrees with its frozen
/// mappings"), and the incremental-reconcile no-change arm ("autoindex
/// execution configuration changed since the committed generation") — which
/// writes no new generation, so a mismatch there never heals. Declaring one
/// extra catalog property would therefore trade #755's abort for a different
/// abort against the very same upgrading population.
///
/// Both arms run here over a journal rewritten to the previous release's
/// identity: an unchanged corpus first (the no-change arm), then a generation
/// left pending mid-commit (the replay).
/// `catalog_mapping_is_the_frozen_on_disk_contract` pins the contract itself;
/// this test pins the wiring that consumes it.
#[test]
fn a_generation_frozen_by_the_previous_release_still_reconciles() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    fs::write(&source, "id,value\n1,committed\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    // One committed generation, aged to the previous release's contract.
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let (previous_release, this_build) =
        freeze_journal_identities_as_previous_release(state_dir.path());

    // Unchanged corpus → the no-change arm's ensure!. This is the permanent
    // one: it commits no new generation, so a mismatch here never heals.
    let unchanged = run_index(config.clone()).unwrap_or_else(|error| {
        panic!(
            "an unchanged corpus committed by the previous release must reconcile, not abort \
             (previous release froze {previous_release}, this build computes {this_build}): \
             {error:#}"
        )
    });
    assert_eq!(unchanged, 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Now a generation left pending mid-commit, aged the same way.
    fs::write(&source, "id,value\n1,sealed-pending\n2,sealed-second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let (previous_release, this_build) =
        freeze_journal_identities_as_previous_release(state_dir.path());

    // Replay of that pending generation → `provision_generation`'s ensure!.
    let resumed = run_index(config).unwrap_or_else(|error| {
        panic!(
            "a pending generation frozen by the previous release must replay, not abort \
             (previous release froze {previous_release}, this build computes {this_build}): \
             {error:#}"
        )
    });
    assert_eq!(resumed, 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    assert_eq!(endpoint.data_docs().len(), 2);
    assert_eq!(
        previous_release, this_build,
        "this build must compute the identity the previous release froze; the catalog mapping \
         is hashed into it, so a property added there aborts every upgraded state dir"
    );
}

#[test]
fn genesis_recovers_after_snapshot_budget_failure_before_sync_begin() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.snapshot_max_bytes = 1;

    let error = run_index(config.clone()).unwrap_err();
    assert!(format!("{error:#}").contains("snapshot source footprint"));
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_bulk_requests(), 0);

    config.snapshot_max_bytes = 64 << 30;
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(endpoint.data_docs().len(), 1);
}

#[test]
fn no_semantic_generation_does_not_require_embedding_identity_endpoint() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    endpoint.state.lock().unwrap().fail_embedding_identity = true;

    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(journal_events(state_dir.path(), "finish"), 1);
    assert_eq!(final_snapshot_count(state_dir.path()), 1);
    assert_eq!(endpoint.data_docs().len(), 1);
}

#[test]
fn initial_generation_and_noop_are_end_to_end_idempotent() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(code, 0);
    let summary = summary.expect("generated run must return its committed run projection");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 2);
    let first_docs = endpoint.data_docs();
    assert_eq!(first_docs.len(), 2);
    assert_eq!(paths(&first_docs), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let root = corpus.path().canonicalize().unwrap();
    let preflight = state::Journal::preflight(
        state_dir.path(),
        &root.to_string_lossy(),
        &endpoint.url,
        "incremental-http",
        false,
    )
    .unwrap();
    assert!(
        preflight.committed_manifest.is_some(),
        "sync_commit must be visible to the next preflight"
    );
    drop(preflight);

    let bulks = endpoint.data_bulk_requests();
    let (_, noop_summary) = run_index_report(config).unwrap();
    assert_eq!(noop_summary.unwrap()["generation"], 1);
    assert_eq!(endpoint.data_docs(), first_docs);
    assert_eq!(endpoint.data_bulk_requests(), bulks);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(journal_events(state_dir.path(), "finish"), 2);
}

/// Incremental re-index must NOT re-run the Phase-A scan/parse for files whose
/// content is byte-identical to the committed generation — that is the ~100x
/// win for the edit-and-rerun / CI workflow, since the tree-sitter parse
/// dominates per-file cost. Proven directly with the run's own `ScanTally`
/// (0 on an unchanged re-run, exactly the changed count when one file changes),
/// and the committed index stays byte-identical.
///
/// #891: each measurement uses a tally created for THAT run. The counter used
/// to be a process-global `static`, which meant this test read whatever every
/// other test in the binary happened to parse in the same window — see
/// `another_runs_parses_never_land_in_this_runs_tally` below.
#[test]
fn unchanged_reindex_skips_the_parse_and_stays_byte_identical() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // A code file (tree-sitter parse is the expensive Phase-A work) + a data file.
    fs::write(
        corpus.path().join("main.py"),
        "def add(a, b):\n    return a + b\n\nclass Calc:\n    def run(self):\n        return add(1, 2)\n",
    )
    .unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    // Genesis: everything is parsed.
    let genesis = crate::ScanTally::default();
    assert_eq!(run_index_tallied(config.clone(), &genesis).unwrap(), 0);
    assert_eq!(
        genesis.files_parsed(),
        2,
        "genesis has no committed generation to skip against: both files are parsed"
    );
    let first_docs = endpoint.data_docs();
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Re-index with NOTHING changed → zero files parsed, index byte-identical.
    let unchanged = crate::ScanTally::default();
    assert_eq!(run_index_tallied(config.clone(), &unchanged).unwrap(), 0);
    assert_eq!(
        unchanged.files_parsed(),
        0,
        "unchanged re-index must parse zero files"
    );
    assert_eq!(
        endpoint.data_docs(),
        first_docs,
        "unchanged re-index must leave the index byte-identical"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Change ONE file → exactly that file is parsed (gate is digest-driven, not
    // a blanket skip).
    fs::write(
        corpus.path().join("main.py"),
        "def add(a, b):\n    return a + b + 1\n",
    )
    .unwrap();
    let one_changed = crate::ScanTally::default();
    assert_eq!(run_index_tallied(config.clone(), &one_changed).unwrap(), 0);
    assert_eq!(
        one_changed.files_parsed(),
        1,
        "only the one changed file must be parsed; the unchanged file stays skipped"
    );
}

/// #891: a parse measurement must describe the run that made it, and nothing
/// else the process is doing.
///
/// The counter above used to be one process-global `static`. `cargo test` runs
/// this lib test binary multi-threaded, the measuring test only holds
/// `HTTP_E2E_LOCK`, and every other test that parses files — those under
/// `FAILPOINT_TEST_LOCK`, `generation_catalog_http_tests`, plain unit tests —
/// added to the same number. Both assertions above could therefore read a total
/// that was not theirs: 2 of 6 and then 1 of 6 local suite runs on the branch
/// that found it (#886) went red here with no code change between them. It is a
/// scheduling race, so plain `main` can also sit clean for twenty consecutive
/// runs — which is exactly why the proof below does not rely on luck.
///
/// This reproduces that interference deterministically instead of waiting for
/// the scheduler to reproduce it: a SECOND, unrelated corpus is indexed inside
/// the first run's measurement window — after its run finishes, before its
/// number is read. That is the event the flake needed, sequenced rather than
/// raced; a genuinely concurrent second run would trade this flake for a new
/// one, since these fixtures are serialised by `HTTP_E2E_LOCK` for reasons of
/// their own. Against the old global counter the unchanged re-index reads the
/// interfering run's 2 parses instead of its own 0; against a per-run tally
/// each run reads only its own work.
#[test]
fn another_runs_parses_never_land_in_this_runs_tally() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    // The measured corpus, indexed once so that its re-index has something to
    // skip.
    let measured_corpus = tempfile::tempdir().unwrap();
    let measured_state = tempfile::tempdir().unwrap();
    fs::write(
        measured_corpus.path().join("main.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap();
    fs::write(
        measured_corpus.path().join("a.csv"),
        "id,value\n1,alpha\n2,beta\n",
    )
    .unwrap();
    let measured_endpoint = HttpEndpoint::start();
    let measured_config = cfg(
        measured_corpus.path(),
        measured_state.path(),
        &measured_endpoint.url,
        false,
    );
    assert_eq!(
        run_index_tallied(measured_config.clone(), &crate::ScanTally::default()).unwrap(),
        0
    );

    // An unrelated corpus, standing in for whatever else the test binary is
    // running. Its own genesis parses two files.
    let other_corpus = tempfile::tempdir().unwrap();
    let other_state = tempfile::tempdir().unwrap();
    fs::write(
        other_corpus.path().join("other.py"),
        "def mul(a, b):\n    return a * b\n",
    )
    .unwrap();
    fs::write(
        other_corpus.path().join("other.csv"),
        "id,value\n7,gamma\n8,delta\n",
    )
    .unwrap();
    let other_endpoint = HttpEndpoint::start();
    let other_config = cfg(
        other_corpus.path(),
        other_state.path(),
        &other_endpoint.url,
        false,
    );

    // The measurement window: measure, let the other run happen, then read.
    let measured = crate::ScanTally::default();
    assert_eq!(run_index_tallied(measured_config, &measured).unwrap(), 0);
    let other = crate::ScanTally::default();
    assert_eq!(run_index_tallied(other_config, &other).unwrap(), 0);

    assert_eq!(
        other.files_parsed(),
        2,
        "the interfering run really did parse files — otherwise this proves nothing"
    );
    assert_eq!(
        measured.files_parsed(),
        0,
        "an unchanged re-index parses nothing; another run's parses must not be \
         added to this run's tally (#891)"
    );
}

#[test]
fn add_change_delete_and_rename_converge_over_real_http() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(corpus.path().join("b.csv"), "id,value\n2,bravo\n").unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv", "b.csv"]);

    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,alpha-new\n3,charlie\n",
    )
    .unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(endpoint.data_docs().len(), 3);

    fs::remove_file(corpus.path().join("b.csv")).unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);

    fs::rename(
        corpus.path().join("a.csv"),
        corpus.path().join("renamed.csv"),
    )
    .unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(paths(&endpoint.data_docs()), ["renamed.csv"]);
    assert_eq!(endpoint.data_docs().len(), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 5);
}

/// #580: re-indexing a **code file** must not abort. The AST file document
/// carries a `symbols` array (of objects); `FieldAcc` does not scalar-type it,
/// so it is pruned from the frozen dataset schema at genesis — and the second
/// generation observed `symbols` again and aborted with `field symbols is
/// absent from frozen dataset`. A field with no scalar values is now allowed to
/// be absent from the frozen schema.
#[test]
fn code_file_reindex_does_not_abort_on_absent_symbols_field() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("lib.rs"),
        "pub struct AlphaConfig {\n    pub retries: u32,\n}\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    // Genesis.
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    // Edit the code file and re-index — before #580 this aborted the whole run
    // on the frozen `symbols` field.
    fs::write(
        corpus.path().join("lib.rs"),
        "pub struct AlphaConfig {\n    pub retries: u64,\n}\n",
    )
    .unwrap();
    assert_eq!(
        run_index(config).unwrap(),
        0,
        "re-indexing a code file must reconcile, not abort on the frozen `symbols` field (#580)"
    );
    // The struct is still indexed (file doc + its symbol doc).
    assert!(
        endpoint
            .data_docs()
            .iter()
            .any(|d| d.get("name").is_some_and(|n| n == "AlphaConfig")),
        "the struct's symbol doc must survive the re-index"
    );
}

#[test]
fn pending_generation_replays_sealed_source_after_live_source_mutates() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    fs::write(&source, "id,value\n1,committed\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(&source, "id,value\n1,sealed-pending\n2,sealed-second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    fs::write(&source, "id,value\n9,mutated-after-sync-begin\n").unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    let docs = endpoint.data_docs();
    assert_eq!(docs.len(), 2);
    let rendered = serde_json::to_string(&docs).unwrap();
    assert!(rendered.contains("sealed-pending"), "{rendered}");
    assert!(rendered.contains("sealed-second"), "{rendered}");
    assert!(!rendered.contains("mutated-after-sync-begin"), "{rendered}");
}

/// Crash/resume with a *partially applied* data bulk, on `--no-graph`.
///
/// `failure_resume_http_tests` used to run its whole injected-failure matrix
/// with `no_graph: true`; since `--no-graph` now takes the generated executor,
/// that module keeps the legacy graph-enabled transaction and this test carries
/// the no-graph half of the coverage. The endpoint applies half of a bulk and
/// *then* reports failure — visibility has changed, so the generation must stay
/// uncommitted, and the retry must converge on exactly the new content with no
/// duplicate or stale document surviving the half-applied attempt.
#[test]
fn partially_applied_data_bulk_leaves_the_generation_pending_and_the_retry_converges() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    fs::write(
        &source,
        "id,value\n1,replaced\n2,second\n3,third\n4,fourth\n",
    )
    .unwrap();
    endpoint
        .state
        .lock()
        .unwrap()
        .partially_apply_next_data_bulk = true;
    assert!(
        run_index(config.clone()).is_err(),
        "a partially applied bulk must fail the run, not be absorbed"
    );
    assert_eq!(
        journal_events(state_dir.path(), "sync_begin"),
        2,
        "the generation was begun"
    );
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "a partially applied bulk must never commit the generation"
    );

    // Retry: the sealed source replays, and the destination converges exactly.
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    let docs = endpoint.data_docs();
    assert_eq!(paths(&docs), ["rows.csv"]);
    assert_eq!(
        docs.len(),
        4,
        "exactly the four new records survive — no half-applied duplicate, no stale first record"
    );
    let rendered = serde_json::to_string(&docs).unwrap();
    assert!(rendered.contains("replaced"), "{rendered}");
    assert!(rendered.contains("fourth"), "{rendered}");
    assert!(
        !rendered.contains("\"value\":\"first\""),
        "the replaced record must not survive: {rendered}"
    );
}

/// #490 (fix #3): a corpus committed by the `--no-graph` generated path must not
/// be silently re-runnable on the default *graph* path. A committed generation
/// manifest is graph-disabled by construction (`sync::validate_manifest`), so
/// the graph path recognises the mismatch and refuses it — the way the
/// graph→no-graph direction is already refused.
///
/// Scope of what this harness proves. Without the guard the re-run still fails,
/// but only later and opaquely: `write_plan`'s own `ensure!` rejects it with
/// `legacy plan write cannot follow a committed generated manifest`, *after*
/// `open_after_preflight` and `gc_snapshots` have run. On real binaries whose
/// graph path reaches Phase B before that point the destination is mutated first
/// (#490 matrix: 79 → 80 docs); this in-process harness is backstopped by that
/// `ensure!`, so it exercises the *message and ordering*, not the remote row.
/// The load-bearing contract pinned here is therefore: the run is refused with
/// the clear cross-path message and NOT the opaque `write_plan` one (i.e. the
/// guard preempts it, before the journal is opened for write). The destination
/// staying clean is asserted as a belt-and-suspenders invariant — true here
/// regardless of the guard, so a future change that lets the graph path reach
/// Phase B before refusing is caught.
#[test]
fn no_graph_committed_corpus_is_refused_on_the_graph_path_with_a_clear_message() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    let endpoint = HttpEndpoint::start();

    // Commit generation 1 with the `--no-graph` generated executor.
    let no_graph = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(no_graph.clone()).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let committed = endpoint.data_docs().len();
    assert!(committed > 0, "the no-graph run published documents");

    // Grow the corpus so a graph re-run would have a new row to publish, then
    // re-run the *same* state dir on the default graph path (`no_graph: false`).
    fs::write(&source, "id,value\n1,first\n2,second\n").unwrap();
    let mut graph = no_graph.clone();
    graph.no_graph = false;
    let error = run_index(graph).unwrap_err();
    let rendered = format!("{error:#}");
    // Load-bearing pair. Fail-before, `rendered` IS the opaque `write_plan`
    // error (thrown after journal-open/gc_snapshots) and lacks this phrase; the
    // guard replaces it with the clear, actionable one *and* preempts the opaque
    // one, proving the refusal now lands earlier.
    assert!(
        rendered.contains("indexed with --no-graph"),
        "the graph re-run must be refused with the clear cross-path message, got: {rendered}"
    );
    assert!(
        !rendered.contains("legacy plan write cannot follow a committed generated manifest"),
        "the guard must preempt write_plan's opaque late error, got: {rendered}"
    );

    // Belt-and-suspenders (true here regardless of the guard, because this
    // harness is backstopped by write_plan's `ensure!`): the destination is left
    // clean — the added row was not published and no second generation committed.
    // Asserted so a future change that lets the graph path reach Phase B before
    // refusing is caught as a mutation.
    assert_eq!(
        endpoint.data_docs().len(),
        committed,
        "the refused graph re-run must not publish the added row"
    );
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "no second generation may be committed by the refused cross-path re-run"
    );
}

#[test]
fn pending_semantic_identity_drift_rejects_before_any_further_bulk() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("report.txt");
    fs::write(
        &source,
        "Initial quarterly report discusses durable subscription revenue and operating income.",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, true);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(
        &source,
        "Updated quarterly report discusses materially stronger subscription revenue and margin.",
    )
    .unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    let bulks_before = endpoint.data_bulk_requests();
    endpoint.state.lock().unwrap().embedding_identity_sha256 = "b".repeat(64);

    let error = run_index(config).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("different embedding execution identity"),
        "{message}"
    );
    assert!(
        message.contains("no remote mutation was attempted"),
        "{message}"
    );
    assert_eq!(
        endpoint.data_bulk_requests(),
        bulks_before,
        "identity drift must be rejected before another data bulk"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
}

/// `--fresh` must be refused before it can destroy generation authority.
///
/// `Journal::open_after_preflight` deletes `journal.ndjson` whenever `fresh` is
/// set, and `gc_snapshots` then sees an empty protected set and removes every
/// sealed snapshot. Both run inside the generated branches, ahead of the legacy
/// plan gate, so without the preflight refusal a single `--fresh` erases the
/// committed manifest and every sealed source the next run needs.
#[test]
fn fresh_cannot_destroy_a_committed_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    fs::write(corpus.path().join("b.csv"), "id,value\n2,bravo\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let committed_docs = endpoint.data_docs();
    assert_eq!(paths(&committed_docs), ["a.csv", "b.csv"]);

    let journal_path = state_dir.path().join("journal.ndjson");
    let journal_before = fs::read(&journal_path).unwrap();
    let snapshots_before = snapshot_names(state_dir.path());
    assert_eq!(snapshots_before.len(), 1, "{snapshots_before:?}");
    let requests_before = endpoint.state.lock().unwrap().requests.len();

    // The folder also changed, which is exactly when a user reaches for
    // --fresh.
    fs::remove_file(corpus.path().join("b.csv")).unwrap();
    config.fresh = true;
    let error = run_index(config.clone()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("`--fresh` cannot discard committed corpus generation 1"),
        "the refusal must name the durable state that blocks it: {message}"
    );
    assert!(
        message.contains("without `--fresh`"),
        "the refusal must point at the incremental path that does work: {message}"
    );

    assert_eq!(
        fs::read(&journal_path).unwrap(),
        journal_before,
        "a refused --fresh must not rewrite the journal"
    );
    assert_eq!(
        snapshot_names(state_dir.path()),
        snapshots_before,
        "a refused --fresh must not garbage-collect sealed snapshots"
    );
    let attempted = endpoint.state.lock().unwrap().requests[requests_before..].to_vec();
    assert_eq!(
        attempted,
        [("GET".to_owned(), "/".to_owned())],
        "a refused --fresh may perform only the endpoint-readiness GET"
    );

    // The surviving authority still reconciles the real folder change.
    config.fresh = false;
    let (code, summary) = run_index_report(config).unwrap();
    assert_eq!(code, 0);
    assert_eq!(summary.unwrap()["generation"], 2);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// Every refusal on the generated path now sends the operator to an isolated
/// rebuild, so that recipe has to work. `--fresh` is no longer a rebuild-in-
/// place escape hatch, and a new `--prefix` alone is not enough when the state
/// directory was named explicitly: `preflight` refuses a journal recorded for a
/// different prefix before the run reaches anything else.
#[test]
fn the_isolated_rebuild_the_refusals_recommend_is_followable() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let committed_state = tempfile::tempdir().unwrap();
    let rebuild_state = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), committed_state.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    // A new --prefix on its own cannot rebuild: the recorded journal owns it.
    let mut prefix_only = config.clone();
    prefix_only.prefix = "incremental-http-rebuild".into();
    let message = format!("{:#}", run_index(prefix_only).unwrap_err());
    assert!(message.contains("was created for root="), "{message}");

    // A new --state-dir and a new --prefix together do rebuild, and leave the
    // original generation and its sealed snapshot untouched.
    let journal_before = fs::read(committed_state.path().join("journal.ndjson")).unwrap();
    let snapshots_before = snapshot_names(committed_state.path());
    let mut rebuild = cfg(corpus.path(), rebuild_state.path(), &endpoint.url, false);
    rebuild.prefix = "incremental-http-rebuild".into();
    let (code, summary) = run_index_report(rebuild).unwrap();
    assert_eq!(code, 0);
    assert_eq!(summary.unwrap()["generation"], 1);
    assert_eq!(journal_events(rebuild_state.path(), "sync_commit"), 1);
    assert_eq!(
        fs::read(committed_state.path().join("journal.ndjson")).unwrap(),
        journal_before,
        "an isolated rebuild must not touch the original journal"
    );
    assert_eq!(
        snapshot_names(committed_state.path()),
        snapshots_before,
        "an isolated rebuild must not touch the original sealed snapshots"
    );
}

/// The same refusal on a crashed generation, where the journal additionally
/// holds the only record of the pending transaction and its sealed source.
#[test]
fn fresh_cannot_destroy_a_pending_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    fs::write(&source, "id,value\n1,committed\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    fs::write(&source, "id,value\n1,sealed-pending\n2,sealed-second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    let journal_path = state_dir.path().join("journal.ndjson");
    let journal_before = fs::read(&journal_path).unwrap();
    let snapshots_before = snapshot_names(state_dir.path());
    assert_eq!(snapshots_before.len(), 2, "{snapshots_before:?}");
    let requests_before = endpoint.state.lock().unwrap().requests.len();

    config.fresh = true;
    let error = run_index(config.clone()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("`--fresh` cannot discard an uncommitted pending corpus generation"),
        "the refusal must name the durable state that blocks it: {message}"
    );
    assert_eq!(
        fs::read(&journal_path).unwrap(),
        journal_before,
        "a refused --fresh must not rewrite a pending journal"
    );
    assert_eq!(
        snapshot_names(state_dir.path()),
        snapshots_before,
        "a refused --fresh must not delete the pending sealed source"
    );
    let attempted = endpoint.state.lock().unwrap().requests[requests_before..].to_vec();
    assert_eq!(
        attempted,
        [("GET".to_owned(), "/".to_owned())],
        "a refused --fresh may perform only the endpoint-readiness GET"
    );

    // The pending transaction is still resumable from its sealed source.
    config.fresh = false;
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    let rendered = serde_json::to_string(&endpoint.data_docs()).unwrap();
    assert!(rendered.contains("sealed-pending"), "{rendered}");
    assert!(rendered.contains("sealed-second"), "{rendered}");
}

/// Two runs started in the same process and the same UTC second must not share
/// a `run_id`.
///
/// The generation catalog keys its managed documents on `run_id`, so a
/// collision makes the second run read the first run's documents back as its
/// own generation. `xerj brain` calls `run_index_report` more than once per
/// process, so this is reachable without any concurrency.
#[test]
fn same_second_runs_in_one_process_do_not_share_a_generation_identity() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();

    assert_eq!(
        run_index(cfg(corpus.path(), first_state.path(), &endpoint.url, false)).unwrap(),
        0
    );
    let second = run_index_report(cfg(
        corpus.path(),
        second_state.path(),
        &endpoint.url,
        false,
    ));
    let (code, summary) = second.expect("a second run in the same second must commit");
    assert_eq!(code, 0);
    assert_eq!(summary.unwrap()["generation"], 1);

    let run_id = |state_dir: &Path| -> String {
        fs::read_to_string(state_dir.join("journal.ndjson"))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|event| {
                event
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .expect("journal records a run_id")
    };
    assert_ne!(run_id(first_state.path()), run_id(second_state.path()));
}

/// An upsert must clear the desired content identity as well as the committed
/// one before it replays.
///
/// A byte-identical retry of the same prepared stream is idempotent on its own,
/// because `ids::doc_id` is derived from dataset slug, content id and locator.
/// The guard earns its keep when a prior aborted attempt left documents under
/// the desired `ax_file` whose ids the current stream does not overwrite — an
/// earlier generation of the same content under a different dataset
/// assignment, or a partially rolled-back attempt. This test models that
/// survivor directly, because the exact record validation at the commit
/// barrier is what turns such a leftover into a hard, unrecoverable failure.
#[test]
fn upsert_clears_a_survivor_under_the_desired_content_identity() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("a.csv");
    let original = "id,value\n1,alpha\n";
    fs::write(&source, original).unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);

    let (index, content_id) = {
        let locked = endpoint.state.lock().unwrap();
        let ((index, _), doc) = locked
            .docs
            .iter()
            .find(|((index, _), _)| index != catalog::CATALOG_INDEX)
            .expect("the first generation published a data document");
        (
            index.clone(),
            doc.get("ax_file")
                .and_then(Value::as_str)
                .expect("data documents carry ax_file")
                .to_owned(),
        )
    };

    // Replace the content, then restore it, so the third generation upserts the
    // original content identity again.
    fs::write(&source, "id,value\n2,bravo\n").unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    fs::write(&source, original).unwrap();

    // A document from an aborted earlier attempt at exactly this content, under
    // an id the prepared stream will not overwrite.
    endpoint.state.lock().unwrap().docs.insert(
        (index.clone(), "aborted-attempt-survivor".to_owned()),
        json!({
            "id": 1,
            "value": "alpha",
            "ax_file": content_id,
            "ax_path": "a.csv",
            "ax_locator": "aborted-attempt-survivor"
        }),
    );

    assert_eq!(run_index(config).unwrap(), 0);
    assert!(
        !endpoint
            .state
            .lock()
            .unwrap()
            .docs
            .contains_key(&(index, "aborted-attempt-survivor".to_owned())),
        "the upsert must delete the desired content identity before replaying it"
    );
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 3);
}

/// `--dry-run` used to be consulted only *after* the generated branches had
/// already published and committed, so on any already-generated state
/// directory the flag was accepted, ignored, and the destination mutated.
#[test]
fn dry_run_on_a_generated_state_directory_publishes_nothing() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let committed_docs = endpoint.data_docs();
    let bulks = endpoint.data_bulk_requests();
    let journal_before = fs::read(state_dir.path().join("journal.ndjson")).unwrap();

    // A real change, so the dry run has something it would publish.
    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,CHANGED\n2,added\n",
    )
    .unwrap();
    let mut dry = config.clone();
    dry.dry_run = true;
    let (code, summary) = run_index_report(dry.clone()).unwrap();
    assert_eq!(code, 0);
    assert!(
        summary.is_none(),
        "--dry-run must not report a committed run summary"
    );
    assert_eq!(
        endpoint.data_bulk_requests(),
        bulks,
        "--dry-run issued a data bulk"
    );
    assert_eq!(
        endpoint.data_docs(),
        committed_docs,
        "--dry-run mutated the destination"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(
        fs::read(state_dir.path().join("journal.ndjson")).unwrap(),
        journal_before,
        "--dry-run appended to the durable journal"
    );

    // An unchanged folder takes the no-op arm of the same branch: still inert.
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    assert_eq!(run_index(dry).unwrap(), 0);
    assert_eq!(endpoint.data_bulk_requests(), bulks);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // And the real run that follows still converges on the change.
    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,CHANGED\n2,added\n",
    )
    .unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(endpoint.data_docs().len(), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// A junk/skipped file has no `plan.files` entry by construction, so demanding
/// a prepared artifact for every inventory entry failed the whole run on any
/// folder containing one unreadable, empty or unrecognised file.
#[test]
fn a_junk_file_is_recorded_and_never_fatal() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    fs::write(corpus.path().join("empty.csv"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config.clone()).unwrap();
    let summary = summary.expect("a corpus with one junk file still commits a generation");
    assert_eq!(
        code, 3,
        "a completed run that skipped a file exits 3, not 0 (cli.rs EXIT CODES)"
    );
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["files_junk"], 1);
    assert_eq!(summary["records_total"], 2);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    let junk_docs: Vec<Value> = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("file")
                && doc.get("path").and_then(Value::as_str) == Some("empty.csv")
        })
        .map(|(_, doc)| doc.clone())
        .collect();
    assert_eq!(
        junk_docs.len(),
        1,
        "the skipped file must still get exactly one catalog entry"
    );

    // The generation reconciles normally afterwards, junk file and all.
    fs::write(corpus.path().join("b.csv"), "id,value\n3,gamma\n").unwrap();
    assert_eq!(run_index(config).unwrap(), 3);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv", "b.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

#[test]
fn issue_283_inferred_float_manifest_digest_replays_on_unchanged_rerun() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // 2,209 bytes / 9 records produces the exact inferred f64 which used to
    // change from 245.44444444444446 to 245.44444444444449 on journal replay.
    let mut csv = String::from("body\n");
    for len in [245, 245, 245, 245, 245, 245, 245, 245, 249] {
        csv.push_str(&"x".repeat(len));
        csv.push('\n');
    }
    fs::write(corpus.path().join("data.csv"), csv).unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (first_code, first_summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(first_code, 0);
    assert_eq!(first_summary.unwrap()["generation"], 1);

    let (second_code, second_summary) = run_index_report(config).unwrap();
    assert_eq!(second_code, 0);
    assert_eq!(second_summary.unwrap()["generation"], 1);
}

#[test]
fn issue_283_junk_bearing_generation_reconciles_a_shrinking_file_set() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    // Preserve the inferred f64 from the original failure while exercising the
    // user's actual second-run shape: generation one contains junk, then a
    // tracked file disappears without any ignore-policy change.
    let mut csv = String::from("body\n");
    for len in [245, 245, 245, 245, 245, 245, 245, 245, 249] {
        csv.push_str(&"x".repeat(len));
        csv.push('\n');
    }
    fs::write(corpus.path().join("data.csv"), csv).unwrap();
    fs::write(
        corpus.path().join("remove.md"),
        "# Temporary report\n\nThis file is removed before generation two.\n",
    )
    .unwrap();
    fs::write(corpus.path().join("empty.csv"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (first_code, first_summary) = run_index_report(config.clone()).unwrap();
    assert_eq!(first_code, 3, "generation one completes with recorded junk");
    let first_summary = first_summary.unwrap();
    assert_eq!(first_summary["generation"], 1);
    assert_eq!(first_summary["files_junk"], 1);
    assert_eq!(paths(&endpoint.data_docs()), ["data.csv", "remove.md"]);

    fs::remove_file(corpus.path().join("remove.md")).unwrap();
    let (second_code, second_summary) = run_index_report(config).unwrap();
    assert_eq!(second_code, 3, "the smaller tree still records empty.csv");
    let second_summary = second_summary.unwrap();
    assert_eq!(second_summary["generation"], 2);
    assert_eq!(second_summary["files_junk"], 1);
    assert_eq!(paths(&endpoint.data_docs()), ["data.csv"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// Junk *records* were fatal on the generated path
/// (`durable preparation of X produced N junk records`) while the legacy path
/// merely accumulated them — directly contradicting the documented contract
/// "3 completed-with-junk (junk recorded, never fatal)".
#[test]
fn a_junk_record_is_counted_into_the_generation_and_never_fatal() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut log = String::from("!!! rotated by logrotate, not a log line\n");
    for n in 1..=40 {
        log.push_str(&format!(
            "10.0.0.{n} - - [09/Aug/2026:10:00:{n:02} +0000] \"GET /p/{n} HTTP/1.1\" 200 {n}\n"
        ));
    }
    fs::write(corpus.path().join("access.log"), log).unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a corpus with one junk record still commits a generation");
    assert_eq!(code, 3, "completed-with-junk exits 3");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["files_junk"], 0);
    assert_eq!(
        summary["junk_records_total"], 1,
        "the unparseable line must be counted, not hardcoded to zero"
    );
    assert_eq!(summary["records_total"], 40);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // The same count reaches the per-file catalog document.
    let file_doc = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .find(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("file")
                && doc.get("path").and_then(Value::as_str) == Some("access.log")
        })
        .map(|(_, doc)| doc.clone())
        .expect("the indexed file has a catalog document");
    assert_eq!(file_doc["junk"], 1);
    assert_eq!(file_doc["records"], 40);
}

/// #360: one file can feed several datasets — a SQL dump is one file and N
/// tables — but the generated executor sealed a single flat record total per
/// *file* and reconciled that whole-file total against *each* dataset's
/// read-back. Every ordinary multi-table dump therefore aborted at catalog
/// publication with `exit=1` on a run whose data was already complete.
#[test]
fn a_multi_table_dump_reconciles_each_dataset_against_its_own_sealed_count() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("dump.sql"),
        "CREATE TABLE `users` (`id` int, `name` varchar(64));\n\
         INSERT INTO `users` VALUES (1,'ann'),(2,'bob');\n\
         CREATE TABLE `orders` (`id` int, `total` int);\n\
         INSERT INTO `orders` VALUES (10,100),(11,200);\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a two-table dump must commit its generation");
    assert_eq!(code, 0, "a valid multi-table dump is not a failed run");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 4);
    assert_eq!(summary["junk_records_total"], 0);
    assert_eq!(endpoint.data_docs().len(), 4);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Both tables became datasets, and each one's catalog document reports the
    // records it actually holds — not the whole file's total.
    let dataset_docs: BTreeMap<String, Value> = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("dataset")
        })
        .map(|((_, id), doc)| (id.clone(), doc.clone()))
        .collect();
    assert_eq!(
        dataset_docs.len(),
        2,
        "one dataset per table: {dataset_docs:?}"
    );
    for (id, doc) in &dataset_docs {
        assert_eq!(doc["record_count"], 2, "dataset {id} holds two rows");
    }
    // Each table's rows really are in their own index, under their own slug.
    for doc in endpoint.data_docs() {
        let slug = doc["ax_dataset"].as_str().unwrap().to_owned();
        assert!(
            dataset_docs.contains_key(&format!("ds:incremental-http:{slug}")),
            "record published under an uncatalogued dataset {slug}"
        );
    }
}

/// The other everyday shape of #360: a real database file. One `.sqlite` is
/// one content group and one dataset per table, exactly like the dump above.
#[test]
fn a_two_table_sqlite_file_commits_its_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(corpus.path().join("shop.sqlite")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);\n\
             INSERT INTO users VALUES (1,'ann'),(2,'bob');\n\
             CREATE TABLE orders (id INTEGER PRIMARY KEY, total INTEGER);\n\
             INSERT INTO orders VALUES (10,100),(11,200);",
        )
        .unwrap();
    drop(connection);
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a two-table database must commit its generation");
    assert_eq!(code, 0);
    assert_eq!(summary["records_total"], 4);
    assert_eq!(endpoint.data_docs().len(), 4);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
}

/// The shape #360 was actually reported against, and the one that reaches the
/// fan-out through the *sniffer* rather than through an extension: ordinary
/// markdown prose with SQL blocks in it, as in `unum-cloud/usearch`'s
/// `sqlite/README.md`. `sniff` routes any text containing `CREATE TABLE` and a
/// `;` to the `sqldump` family, so a README documenting two tables is one
/// content group feeding two datasets — no dump and no database file involved.
/// Worth pinning separately: the reporter's corpus contained neither.
#[test]
fn a_readme_documenting_two_tables_is_not_a_fatal_condition() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("README.md"),
        "# vectors\n\nStore them like this:\n\n```sql\n\
         CREATE TABLE t1 (id INTEGER PRIMARY KEY, v JSON NOT NULL);\n\
         INSERT INTO t1 (id, v) VALUES (10, '[1.0]'), (11, '[2.0]');\n\
         CREATE TABLE t2 (id INTEGER PRIMARY KEY, v JSON NOT NULL);\n\
         INSERT INTO t2 (id, v) VALUES (20, '[1.0]'), (21, '[2.0]');\n\
         ```\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    let summary = summary.expect("a README describing two tables still commits");
    assert_eq!(code, 0, "a valid README is not a failed run");
    assert_eq!(summary["records_total"], 4);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    let mut counts: Vec<(String, u64)> = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("doc_kind").and_then(Value::as_str) == Some("dataset")
        })
        .map(|(_, doc)| {
            (
                doc["slug"].as_str().unwrap_or_default().to_owned(),
                doc["record_count"]
                    .as_u64()
                    .expect("a dataset catalog document reports its record count"),
            )
        })
        .collect();
    counts.sort();
    assert_eq!(
        counts.len(),
        2,
        "one dataset per declared table: {counts:?}"
    );
    assert!(
        counts.iter().all(|(_, records)| *records == 2),
        "each dataset carries only its own rows: {counts:?}"
    );
}

/// The second abort #360 reported — the one that killed both of the corpora
/// the reporter pointed autoindex at, on one node, one after the other.
///
/// `content::full_digest` derives a file's identity from its CONTENT alone, and
/// `catalog::CATALOG_INDEX` is a single global index that no `--prefix`
/// namespaces. Two unrelated checkouts that happen to share one byte-identical
/// file — an Apache-2.0 `LICENSE`, a `.gitignore`, an empty `__init__.py` —
/// therefore share that file's catalog document IDs. A corpus holding the
/// content twice publishes a canonical document plus one alias document; a
/// corpus holding it once republishes only the canonical, so the *other*
/// corpus's alias document survives under its own `run_id`. The generation-wide
/// barrier counted every catalog document carrying that `file_key`, across every
/// run on the node, against `1 + aliases.len()` for the group in front of it —
/// so the second run aborted with `exit=1` while its data was complete.
#[test]
fn a_file_shared_with_another_corpus_on_the_same_node_is_not_a_fatal_condition() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let shared = "id,value\n1,apache-2.0\n2,copied-verbatim\n";
    let first = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    fs::create_dir(first.path().join("docs")).unwrap();
    fs::write(first.path().join("LICENSE.csv"), shared).unwrap();
    fs::write(first.path().join("docs").join("LICENSE.csv"), shared).unwrap();
    let endpoint = HttpEndpoint::start();

    let (code, summary) =
        run_index_report(cfg(first.path(), first_state.path(), &endpoint.url, false))
            .expect("the first corpus indexes");
    let summary = summary.expect("the first corpus commits its generation");
    assert_eq!(code, 0);
    assert_eq!(
        summary["duplicate_files"], 1,
        "the first corpus must publish one alias document, or nothing is left \
         behind for the second run to trip over: {summary}"
    );

    // A second, unrelated corpus on the same node, isolated exactly the way the
    // CLI's refusals recommend: its own --state-dir and its own --prefix. Its
    // one shared file is a group with no aliases at all.
    let second = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    fs::write(second.path().join("LICENSE.csv"), shared).unwrap();
    fs::write(second.path().join("own.csv"), "id,value\n7,second\n").unwrap();
    let mut config = cfg(second.path(), second_state.path(), &endpoint.url, false);
    config.prefix = "incremental-http-second".into();

    let (code, summary) = run_index_report(config)
        .expect("a file shared with another corpus on the same node is not a fatal condition");
    let summary = summary.expect("the second corpus commits its generation");
    assert_eq!(code, 0, "the second corpus is a clean run: {summary}");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 3);
    assert_eq!(summary["duplicate_files"], 0);
    assert_eq!(journal_events(second_state.path(), "sync_commit"), 1);

    // And the first corpus's alias document is still there — the barrier stopped
    // counting it, it was not swept away from under the other corpus's map.
    let aliases = endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), doc)| {
            index == catalog::CATALOG_INDEX
                && doc.get("status").and_then(Value::as_str) == Some("duplicate")
        })
        .count();
    assert_eq!(
        aliases, 1,
        "the other corpus's alias document must survive this run untouched"
    );
}

/// Where this branch and #241 meet.
///
/// The generated `--no-graph` route returns from inside `run_index_report`,
/// several hundred lines before the legacy `pr.finish` at the bottom of the
/// function. `Ticker::drop` closes an unfinished stream with `ok=false`, so a
/// generated run that merely returned would print an *aborted* terminal line
/// immediately after committing a generation successfully — a silent
/// contradiction between two changes that each looked right on its own.
///
/// This pins all three properties on the generated path: the phase-A scan is
/// narrated (it is a per-file, minutes-long phase here exactly as on the legacy
/// path), the stream closes with a truthful terminal line, and `--progress
/// none` on the same path stays completely silent.
/// #294, at the level where the loss actually happened: a mixed corpus of
/// source code and prose, over the generated (`--no-graph`) path, through the
/// real HTTP client.
///
/// Every code file was walked, sniffed as family `code`, counted — and then
/// prepared as ZERO documents, so the generation committed and the run
/// reported success while the entire code half of the corpus was missing.
/// `sync_executor` pins the extraction half of that regression; this pins what
/// a caller can actually observe: the documents that reach the endpoint carry
/// their AST fields, and the run document and terminal line carry coverage
/// counters that a wholly-junked corpus could not fake.
#[test]
fn code_files_index_with_ast_fields_and_the_run_reports_code_coverage() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("alpha.rs"),
        "pub struct AlphaConfig {\n    pub retries: u32,\n}\n\n\
         pub fn alpha_connect(cfg: &AlphaConfig) -> bool {\n    cfg.retries > 0\n}\n",
    )
    .unwrap();
    fs::write(
        corpus.path().join("gamma.py"),
        "def gamma_load(path):\n    return open(path).read()\n\n\n\
         class GammaStore:\n    def __init__(self, root):\n        self.root = root\n",
    )
    .unwrap();
    fs::write(
        corpus.path().join("notes.md"),
        "# Fixture notes\n\nTwo source files and this prose file, so the corpus is mixed.\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (code, summary) = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index_report(config).unwrap()
    };
    assert_eq!(code, 0, "nothing in this corpus is unparseable");
    let summary = summary.expect("a generated run returns its committed run projection");
    assert_eq!(summary["code_files"], 2, "{summary}");
    assert_eq!(
        summary["code_files_indexed"], 2,
        "both source files must reach the index: {summary}"
    );
    assert_eq!(summary["code_files_junked"], 0, "{summary}");

    let docs = endpoint.data_docs();
    // The per-file AST document carries `defs`/`symbols`; the #500 per-symbol
    // documents carry `code`/`name` but no `defs`, so filter on `defs` to keep
    // the one-per-file assertions honest about the file-level document.
    let mut code_docs: Vec<&Value> = docs
        .iter()
        .filter(|doc| doc.get("language").is_some() && doc.get("defs").is_some())
        .collect();
    code_docs.sort_by_key(|doc| doc["language"].as_str().unwrap_or("").to_owned());
    assert_eq!(
        code_docs.len(),
        2,
        "one AST (file-level) document per source file: {docs:?}"
    );
    // #500: each declaration is also promoted to its own retrievable document
    // (has `code` + `name`, no `defs`). At least the Rust struct + Python class
    // must each be their own symbol document, not only foldable inside a parent.
    let symbol_docs: Vec<&Value> = docs
        .iter()
        .filter(|doc| doc.get("code").is_some() && doc.get("name").is_some())
        .collect();
    assert!(
        symbol_docs
            .iter()
            .any(|d| d["name"] == "AlphaConfig" && d.get("defs").is_none()),
        "the AlphaConfig struct must be its own symbol document (#500): {symbol_docs:?}"
    );
    assert!(
        symbol_docs.iter().any(|d| d["name"] == "GammaStore"),
        "the GammaStore class must be its own symbol document (#500): {symbol_docs:?}"
    );
    assert_eq!(code_docs[0]["language"], "python");
    assert_eq!(code_docs[1]["language"], "rust");
    for doc in &code_docs {
        assert!(
            doc["defs"].as_str().is_some_and(|defs| !defs.is_empty()),
            "{doc}"
        );
        assert!(
            doc["symbols"]
                .as_array()
                .is_some_and(|symbols| !symbols.is_empty()),
            "{doc}"
        );
    }
    // The title comes from the logical file name, never the content-addressed
    // snapshot blob's ordinal — the other half of #294.
    assert_eq!(code_docs[0]["title"], "gamma.py");
    assert_eq!(code_docs[1]["title"], "alpha.rs");
    let rendered = serde_json::to_string(&code_docs).unwrap();
    assert!(rendered.contains("struct AlphaConfig"), "{rendered}");
    assert!(rendered.contains("class GammaStore"), "{rendered}");

    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("{stream}"));
    assert!(
        done.contains("code_files=2 code_files_indexed=2 code_files_junked=0"),
        "the terminal line carries the coverage that made #294 invisible: {done}"
    );
    assert!(!stream.contains("warning:"), "{stream}");
}

#[test]
fn a_generated_run_narrates_its_scan_and_closes_its_own_stream() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    for n in 0..4 {
        fs::write(
            corpus.path().join(format!("rows-{n}.csv")),
            format!("id,value\n{n}0,alpha\n{n}1,beta\n"),
        )
        .unwrap();
    }
    let endpoint = HttpEndpoint::start();

    // Generation 1: the initial commit, through the real progress surface.
    let mut loud = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    loud.quiet = false;
    loud.progress = crate::progress::ProgressMode::Plain;
    loud.progress_interval = Some(std::time::Duration::from_secs(1));
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index(loud.clone()).unwrap()
    };
    assert_eq!(code, 0);
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("a generated run must close its own stream:\n{stream}"));
    assert!(
        done.contains("ok=true") && done.contains("exit=0"),
        "a committed generation is not an aborted run: {done}"
    );
    assert!(done.contains("reason=completed"), "{done}");

    // Generation 2: the reconcile branch, which runs its own phase-A scan.
    fs::write(corpus.path().join("rows-0.csv"), "id,value\n00,CHANGED\n").unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index(loud).unwrap()
    };
    assert_eq!(code, 0);
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(
        stream
            .lines()
            .any(|line| line.starts_with("xerj-progress ") && line.contains("phase=scan")),
        "the reconcile projection re-scans every file and must say so:\n{stream}"
    );
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap_or_else(|| panic!("a reconciled generation must close its own stream:\n{stream}"));
    assert!(done.contains("ok=true"), "{done}");

    // The other half of the contract on the same path: silence means silence.
    fs::write(corpus.path().join("rows-1.csv"), "id,value\n10,CHANGED\n").unwrap();
    let quiet = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(quiet.progress, crate::progress::ProgressMode::None);
    let buffer = Arc::new(Mutex::new(Vec::new()));
    {
        let _sink = crate::progress::install_test_sink(&buffer);
        assert_eq!(run_index(quiet).unwrap(), 0);
    }
    assert!(
        buffer.lock().unwrap().is_empty(),
        "--progress none asked for nothing: {}",
        String::from_utf8_lossy(&buffer.lock().unwrap())
    );
}

/// Two byte-identical junk files — two empty files are the everyday case —
/// made the fresh plan carry a duplicate alias for content that has no
/// `plan.files` entry, so the generation cutover failed its own
/// alias-projection invariant and the whole run aborted with exit 1 (#283's
/// reproducible sibling: junk plus duplicates diverging between the fresh plan
/// and the incremental projection). The fresh plan now projects aliases the
/// same way `reconcile_plan` does, so the folder indexes, a no-op re-run
/// confirms instead of committing a spurious generation, and a shrinking file
/// set on the junk-bearing generation reconciles.
#[test]
fn two_byte_identical_junk_files_commit_reconcile_and_survive_a_shrink() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    fs::write(corpus.path().join("e1.dat"), "").unwrap();
    fs::write(corpus.path().join("e2.dat"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config.clone())
        .expect("a folder with two byte-identical junk files must index, not abort");
    let summary = summary.expect("the junk-bearing generation still commits");
    assert_eq!(code, 3, "junk is recorded, never fatal (cli.rs EXIT CODES)");
    assert_eq!(summary["generation"], 1);
    assert_eq!(summary["records_total"], 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // The fresh plan and the incremental projection must agree byte-for-byte
    // on how junk-content aliases project, or this no-op re-run would see a
    // "changed" plan and commit generation 2 over nothing.
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "a no-op re-run over the junk-bearing generation confirms; it must not re-commit"
    );

    // The issue's headline shape: the file set shrinks on a generation that
    // carries junk records.
    fs::remove_file(corpus.path().join("a.csv")).unwrap();
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
    assert!(
        paths(&endpoint.data_docs()).is_empty(),
        "the deleted dataset's records must not survive the reconcile"
    );

    // And the junk files themselves disappearing reconciles back to clean.
    fs::remove_file(corpus.path().join("e1.dat")).unwrap();
    fs::remove_file(corpus.path().join("e2.dat")).unwrap();
    assert_eq!(run_index(config).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 3);
}

/// The #283 abort itself: a durable generation record that fails its own
/// re-validation used to surface as a bare internal invariant ("desired
/// manifest digest does not match sync_begin payload", exit 1) — text the user
/// can do nothing with. However the journal got that way, the refusal must
/// name the recovery route the way the exec-config guard already does, and
/// keep the invariant attached as the cause.
#[test]
fn a_journal_that_fails_its_own_revalidation_names_the_rebuild_route() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    fs::write(corpus.path().join("empty.csv"), "").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 3);

    // Leave a durable pending generation behind: begin succeeds, the bulk is
    // partially applied, the run fails before commit.
    fs::write(&source, "id,value\n1,replaced\n2,second\n").unwrap();
    endpoint
        .state
        .lock()
        .unwrap()
        .partially_apply_next_data_bulk = true;
    assert!(run_index(config.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

    // Corrupt the pending sync_begin payload in a spot only the manifest
    // digest covers: the junk record's byte count. This makes the next run
    // hit exactly the #283 invariant.
    let journal_path = state_dir.path().join("journal.ndjson");
    let tampered: String = fs::read_to_string(&journal_path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).unwrap();
            if record.get("kind").and_then(Value::as_str) == Some("sync_begin")
                && record
                    .pointer("/desired/generation")
                    .and_then(Value::as_u64)
                    == Some(2)
            {
                let bytes = record
                    .pointer_mut("/desired/plan/junk_files/0/bytes")
                    .expect("the junk-bearing desired manifest records its junk file");
                *bytes = Value::from(bytes.as_u64().unwrap() + 1);
            }
            let mut line = serde_json::to_string(&record).unwrap();
            line.push('\n');
            line
        })
        .collect();
    fs::write(&journal_path, tampered).unwrap();

    let error = run_index(config).expect_err("a tampered generation journal must refuse to run");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("desired manifest digest does not match sync_begin payload"),
        "the internal invariant must stay attached as the cause: {rendered}"
    );
    assert!(
        rendered.contains("Rebuild with a new --state-dir and a new --prefix"),
        "the refusal must name the recovery route, not just the invariant: {rendered}"
    );
}

/// #589: `sweep_excluded_groups` actually deletes an excluded file's published
/// documents (by `ax_file`, across the plan's dataset indices) and its catalog
/// document (by `path`), while leaving every OTHER file's documents intact.
/// Exercised over the real mock HTTP backend so the delete-by-query truly runs.
#[test]
fn sweep_excluded_groups_deletes_only_the_excluded_files_documents() {
    let ep = HttpEndpoint::start();

    // Seed two files' records in one dataset index, plus their catalog docs.
    {
        let mut st = ep.state.lock().unwrap();
        let ds = "incremental-http-ds".to_string();
        st.docs.insert(
            (ds.clone(), "d1".into()),
            json!({"ax_file": "key-excluded", "body": "secret"}),
        );
        st.docs.insert(
            (ds.clone(), "d2".into()),
            json!({"ax_file": "key-kept", "body": "kept"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:excluded".into()),
            json!({"doc_kind": "file", "prefix": "ax", "path": "secret.csv"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:kept".into()),
            json!({"doc_kind": "file", "prefix": "ax", "path": "kept.csv"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");

    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });

    let excluded = vec![InventoryDeltaEntry {
        file_key: "key-excluded".into(),
        path: "secret.csv".into(),
    }];

    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    let st = ep.state.lock().unwrap();
    // The excluded file's dataset record is gone.
    assert!(
        !st.docs
            .values()
            .any(|d| d.get("ax_file").and_then(Value::as_str) == Some("key-excluded")),
        "#589: the excluded file's indexed records must be swept"
    );
    // Its catalog document is gone.
    assert!(
        !st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("path").and_then(Value::as_str) == Some("secret.csv")),
        "#589: the excluded file's catalog document must be swept"
    );
    // Every other file is untouched (no over-deletion).
    assert!(
        st.docs
            .values()
            .any(|d| d.get("ax_file").and_then(Value::as_str) == Some("key-kept")),
        "#589: a different file's records must NOT be swept"
    );
    assert!(
        st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("path").and_then(Value::as_str) == Some("kept.csv")),
        "#589: a different file's catalog document must NOT be swept"
    );
}

/// #694: `sweep_excluded_groups` also soft-invalidates the graph edges an
/// excluded file taught (reusing the replacement hook `invalidate_prior_edges`),
/// while leaving a different file's edges live. The bi-temporal record survives
/// (`as_of` time travel), so the edge is stamped `invalid_at`, not deleted.
/// Fail-before: with the `invalidate_prior_edges` call reverted, the excluded
/// file's edge carries no `invalid_at` and stays searchable.
#[test]
fn sweep_excluded_groups_invalidates_the_excluded_files_edges() {
    let ep = HttpEndpoint::start();
    let edges = ".xerj-memory-testbrain-edges";

    // Two live edges (no `invalid_at`): one taught by the file about to be
    // excluded, one by a file that stays.
    {
        let mut st = ep.state.lock().unwrap();
        // The mock's `_bulk` guard asserts mappings were seen first (ingest
        // ordering); the invalidation re-index is a bulk, so satisfy it — this
        // test exercises edge invalidation, not the mapping-ordering invariant.
        st.saw_dataset_mapping_update = true;
        st.saw_catalog_mapping_update = true;
        st.docs.insert(
            (edges.to_string(), "edge-secret".to_string()),
            json!({"src_file": "secret.csv", "kind": "samedir", "dst_file": "other.csv"}),
        );
        st.docs.insert(
            (edges.to_string(), "edge-kept".to_string()),
            json!({"src_file": "kept.csv", "kind": "samedir", "dst_file": "other.csv"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    let excluded = vec![InventoryDeltaEntry {
        file_key: "key-excluded".into(),
        path: "secret.csv".into(),
    }];

    let now_ms = 1_724_500_000_000i64;
    sweep_excluded_groups(&es, "ax", &plan, &excluded, Some(edges), now_ms).expect("sweep");

    let st = ep.state.lock().unwrap();
    // The excluded file's edge is soft-invalidated (present, stamped invalid_at).
    let secret_edge = st
        .docs
        .get(&(edges.to_string(), "edge-secret".to_string()))
        .expect("excluded file's edge is soft-invalidated, not deleted");
    assert_eq!(
        secret_edge.get("invalid_at").and_then(Value::as_i64),
        Some(now_ms),
        "#694: the excluded file's edge must be stamped invalid_at by the sweep"
    );
    // A different file's edge stays live (no over-invalidation).
    let kept_edge = st
        .docs
        .get(&(edges.to_string(), "edge-kept".to_string()))
        .expect("kept file's edge present");
    assert!(
        kept_edge.get("invalid_at").is_none(),
        "#694: a different file's edge must NOT be invalidated"
    );
}

/// #693: `sweep_excluded_groups` also purges the excluded file's `file-alias:`
/// duplicate catalog docs. Those carry the DUPLICATE's own path (not the
/// canonical `entry.path`), so the `path` term alone misses them — leaving an
/// excluded file's alternate path/filename searchable. Deleting by `file_key`
/// catches them, and cannot strand a live duplicate (a group is only excluded
/// when no surviving file bears its key). Fail-before: without the `file_key`
/// delete, the alias doc (path "dup.csv") survives the canonical-path sweep.
#[test]
fn sweep_excluded_groups_purges_the_excluded_files_alias_catalog_docs() {
    let ep = HttpEndpoint::start();

    {
        let mut st = ep.state.lock().unwrap();
        // Excluded file: a canonical `file:` doc plus a byte-identical
        // `file-alias:` doc under a DIFFERENT path — both carry the same
        // `file_key`.
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:excluded".into()),
            json!({"doc_kind": "file", "prefix": "ax", "file_key": "key-excluded", "path": "canonical.csv"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file-alias:ax:excluded".into()),
            json!({"doc_kind": "file", "prefix": "ax", "file_key": "key-excluded", "path": "dup.csv", "status": "duplicate"}),
        );
        // A different file's alias (distinct file_key) must survive.
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file-alias:ax:kept".into()),
            json!({"doc_kind": "file", "prefix": "ax", "file_key": "key-kept", "path": "kept-dup.csv", "status": "duplicate"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    // entry.path is the CANONICAL rel — deliberately NOT the alias's "dup.csv".
    let excluded = vec![InventoryDeltaEntry {
        file_key: "key-excluded".into(),
        path: "canonical.csv".into(),
    }];

    // 6-arg signature (post-#694 edges, post-#737 prefix): this test does not
    // exercise edges, so pass `None`/`0`; the corpus prefix is "ax".
    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    let st = ep.state.lock().unwrap();
    // The excluded file's alias catalog doc is gone (the #693 fix).
    assert!(
        !st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("path").and_then(Value::as_str) == Some("dup.csv")),
        "#693: the excluded file's file-alias: catalog doc must be swept"
    );
    // Its canonical file doc is gone too.
    assert!(
        !st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("file_key").and_then(Value::as_str) == Some("key-excluded")),
        "#693: the excluded file's catalog docs (by file_key) must be swept"
    );
    // A different file's alias must NOT be swept (file_key is content-scoped).
    assert!(
        st.docs
            .iter()
            .any(|((idx, _), d)| idx == catalog::CATALOG_INDEX
                && d.get("path").and_then(Value::as_str) == Some("kept-dup.csv")),
        "#693: a different file's alias doc must NOT be swept"
    );
}

/// #737: the catalog sweep is scoped to THIS corpus's `prefix`. The
/// `autoindex-catalog` index is shared across corpora; a byte-identical common
/// file (LICENSE, a lockfile) indexed by a still-live SIBLING corpus shares the
/// same `file_key`/`path`, so an unscoped sweep would delete the sibling's live
/// catalog docs too. Fail-before: dropping the `prefix` filter (unscoped
/// `{term:{file_key}}`) deletes the sibling-corpus doc as well.
#[test]
fn sweep_excluded_groups_does_not_delete_a_sibling_corpus_catalog_doc() {
    let ep = HttpEndpoint::start();

    {
        let mut st = ep.state.lock().unwrap();
        // Corpus "ax" excludes a file; corpus "bx" is a live sibling holding a
        // byte-identical copy (same file_key) under its own prefix-scoped id.
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:ax:excluded".into()),
            json!({"doc_kind": "file", "prefix": "ax", "file_key": "shared-key", "path": "LICENSE"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), "file:bx:sibling".into()),
            json!({"doc_kind": "file", "prefix": "bx", "file_key": "shared-key", "path": "LICENSE"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    let excluded = vec![InventoryDeltaEntry {
        file_key: "shared-key".into(),
        path: "LICENSE".into(),
    }];

    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    let st = ep.state.lock().unwrap();
    // Corpus ax's own doc is swept (same file_key/path, matching prefix).
    assert!(
        !st.docs.contains_key(&(
            catalog::CATALOG_INDEX.to_string(),
            "file:ax:excluded".to_string()
        )),
        "#737: the excluding corpus's own catalog doc must still be swept"
    );
    // The live sibling corpus's byte-identical doc SURVIVES (prefix-scoped).
    assert!(
        st.docs.contains_key(&(
            catalog::CATALOG_INDEX.to_string(),
            "file:bx:sibling".to_string()
        )),
        "#737: a sibling corpus's byte-identical catalog doc must NOT be swept"
    );
}

/// #755: the scope the sweep actually relies on is `corpus_scope`, not
/// `prefix`. On a catalog upgraded from v1.0.0-rc.15..rc.67, `prefix` is
/// dynamically mapped `text`, so a `term` against it does not match the raw
/// scope value and the #737/#693 scoped deletes silently no-op — an alias doc
/// that the frozen plan does not name is then left searchable, which is exactly
/// the #439 exposure #589 exists to close. The docs seeded here carry ONLY the
/// keyword scope field, standing in for a catalog whose `prefix` cannot be
/// term-matched; the sweep must still find this corpus's docs and must still
/// leave the live sibling corpus's alone.
///
/// Fail-before: with only the `prefix`-scoped deletes (pre-#755), the `ax` alias
/// doc below survives the sweep.
#[test]
fn sweep_excluded_groups_scopes_on_the_keyword_corpus_scope_field() {
    let ep = HttpEndpoint::start();

    {
        let mut st = ep.state.lock().unwrap();
        // An alias doc the frozen plan does not name, so ONLY the term-scoped
        // `file_key` delete can reach it — the `_id` sweep cannot.
        st.docs.insert(
            (
                catalog::CATALOG_INDEX.to_string(),
                "file-alias:ax:unlisted".into(),
            ),
            json!({
                "doc_kind": "file",
                catalog::CORPUS_SCOPE_FIELD: "ax",
                "file_key": "shared-key",
                "path": "vendor/LICENSE",
                "status": "duplicate",
            }),
        );
        // A live sibling corpus's byte-identical doc under its own scope.
        st.docs.insert(
            (
                catalog::CATALOG_INDEX.to_string(),
                "file-alias:bx:sibling".into(),
            ),
            json!({
                "doc_kind": "file",
                catalog::CORPUS_SCOPE_FIELD: "bx",
                "file_key": "shared-key",
                "path": "vendor/LICENSE",
                "status": "duplicate",
            }),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    let excluded = vec![InventoryDeltaEntry {
        file_key: "shared-key".into(),
        path: "vendor/LICENSE".into(),
    }];

    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    let st = ep.state.lock().unwrap();
    assert!(
        !st.docs.contains_key(&(
            catalog::CATALOG_INDEX.to_string(),
            "file-alias:ax:unlisted".to_string()
        )),
        "#755: an excluded file's catalog doc must be swept on the keyword scope \
         field, not only on a `prefix` an upgraded catalog holds as text"
    );
    assert!(
        st.docs.contains_key(&(
            catalog::CATALOG_INDEX.to_string(),
            "file-alias:bx:sibling".to_string()
        )),
        "#755: the keyword scope must still bound the sweep to THIS corpus"
    );
}

/// #890: the exclusion sweep must not reach ACROSS corpora on a catalog an
/// older build left with `prefix` mapped `text`.
///
/// `autoindex-catalog` is global, and on any install upgraded from
/// v1.0.0-rc.15..rc.67 `prefix` is dynamically inferred `text` (the #755 root
/// cause). A `term` query is not analyzed but the field is, so
/// `{"term": {"prefix": "ax"}}` matches the analyzed TOKENS of `ax-2`
/// (`[ax, 2]`) — and conjoined with a `path`/`file_key` a byte-identical file
/// shares, the #737 prefix-scoped `delete_by_query` deletes the SIBLING
/// corpus's live catalog documents. A delete is not recoverable.
///
/// The endpoint models exactly that: `legacy_text_catalog_fields` now governs
/// how a `term` matches on the catalog, not only which `_mapping` PUT is
/// refused. Fail-before, measured rather than reasoned: with `lib.rs` reverted
/// to the bare `delete_by_query` this replaces, the run reports
/// `Survivors: []` — sweeping corpus `ax` deleted all FOUR catalog documents,
/// both of corpus `ax-2`'s included.
///
/// The last two assertions are the other half, and they are why the fix is an
/// exact-scope delete rather than skipping the `prefix` pass on a legacy
/// catalog: corpus `ax`'s own documents — including an alias the frozen plan
/// does not name, which ONLY the term-scoped pass can reach — must still be
/// swept. Skipping the pass would leave that one searchable, which is the #439
/// exposure #589 exists to close.
#[test]
fn sweep_excluded_groups_cannot_reach_a_sibling_corpus_on_a_text_mapped_prefix() {
    let ep = HttpEndpoint::start();
    // The excluding corpus and a sibling whose prefix shares its first token.
    let ax_main = catalog::file_id("ax", "shared-key");
    let ax_alias = "file-alias:ax:unlisted".to_string();
    let sibling_main = catalog::file_id("ax-2", "shared-key");
    let sibling_alias = "file-alias:ax-2:sibling".to_string();

    {
        let mut st = ep.state.lock().unwrap();
        // A catalog written by rc.15..rc.67: `prefix` is text, and no document
        // carries the keyword `corpus_scope` field, which no such release wrote.
        st.legacy_text_catalog_fields = vec!["prefix".into()];
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), ax_main.clone()),
            json!({"doc_kind": "file", "prefix": "ax", "file_key": "shared-key", "path": "LICENSE"}),
        );
        // An `ax` alias the FROZEN plan does not name, so the by-`_id` sweep
        // (#739) cannot reach it — only the term-scoped `file_key` pass can.
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), ax_alias.clone()),
            json!({
                "doc_kind": "file",
                "prefix": "ax",
                "file_key": "shared-key",
                "path": "vendor/LICENSE",
                "status": "duplicate",
            }),
        );
        // The live sibling corpus's byte-identical copies.
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), sibling_main.clone()),
            json!({"doc_kind": "file", "prefix": "ax-2", "file_key": "shared-key", "path": "LICENSE"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), sibling_alias.clone()),
            json!({
                "doc_kind": "file",
                "prefix": "ax-2",
                "file_key": "shared-key",
                "path": "sibling/LICENSE",
                "status": "duplicate",
            }),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    let excluded = vec![InventoryDeltaEntry {
        file_key: "shared-key".into(),
        path: "LICENSE".into(),
    }];

    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    // Read the four verdicts out and RELEASE the endpoint's lock before
    // asserting: an assert that fires while holding it poisons the mutex the
    // server thread also takes, and the resulting panic-in-a-destructor aborts
    // the whole test binary instead of failing this one test. This test is
    // written to fail on unfixed code, so its failure mode has to be clean.
    let present = |id: &String| {
        ep.state
            .lock()
            .unwrap()
            .docs
            .contains_key(&(catalog::CATALOG_INDEX.to_string(), id.clone()))
    };
    let (sibling_main_kept, sibling_alias_kept) = (present(&sibling_main), present(&sibling_alias));
    let (ax_main_swept, ax_alias_swept) = (!present(&ax_main), !present(&ax_alias));
    // Every message carries the full survivor set, so ONE failing run reports
    // the whole before-picture rather than only the first verdict that broke.
    let survivors = {
        let mut ids: Vec<String> = ep
            .state
            .lock()
            .unwrap()
            .docs
            .keys()
            .filter(|(index, _)| index == catalog::CATALOG_INDEX)
            .map(|(_, id)| id.clone())
            .collect();
        ids.sort();
        format!("{ids:?}")
    };

    assert!(
        sibling_main_kept,
        "#890: a sibling corpus whose prefix shares an analyzed token must keep its \
         catalog document — a text-mapped `prefix` is not a scope. Survivors: {survivors}"
    );
    assert!(
        sibling_alias_kept,
        "#890: the same holds for the sibling's alias doc, reached by `file_key`. \
         Survivors: {survivors}"
    );
    assert!(
        ax_main_swept,
        "#890: the excluding corpus's own document must still be swept. \
         Survivors: {survivors}"
    );
    assert!(
        ax_alias_swept,
        "#890: …including an alias the frozen plan does not name, which only the \
         term-scoped pass reaches — the fix is an exact scope, not a skipped pass. \
         Survivors: {survivors}"
    );
}

/// #890 (review follow-up): the scoped sweep's page walk must end on the
/// candidate set it actually has, not on a short page it may never be sent.
///
/// `delete_catalog_docs_scoped` reads `SCOPED_SWEEP_PAGE` hits at a time for at
/// most `SCOPED_SWEEP_MAX_PAGES` pages. Its first version ended the walk ONLY
/// on a short page, so a scope conjunction matching exactly
/// `SCOPED_SWEEP_MAX_PAGES * SCOPED_SWEEP_PAGE` documents — every page full,
/// the last one included — fell out of the loop and returned the "refusing to
/// report a partial removal" error while `ids` already held the complete,
/// re-checked set: a run aborted on an answer the function was holding. That
/// off-by-one is unreachable from the four-document fixture above, which is why
/// this walks the boundary the way
/// `the_alias_sweep_partitions_its_paths_exactly_across_the_chunk_boundary`
/// walks `ALIAS_SWEEP_CHUNK`: 0 / 1 / PAGE-1 / PAGE / PAGE+1 / MAX*PAGE, plus
/// the one candidate over the cap, which must still refuse AND still delete
/// nothing.
///
/// Every case includes one sibling-corpus document, and it sorts LAST, so the
/// per-hit re-check is proven on the final page of a multi-page walk rather
/// than only inside the first page. `prefix` is legacy `text` here for the
/// reason it is in the test above: that is what makes the sibling a candidate
/// the server returns and the client has to drop.
#[test]
fn the_scoped_catalog_sweep_pages_to_the_exact_end_of_its_candidate_set() {
    let ep = HttpEndpoint::start();
    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    // Sorts after every `file-alias:` id below (`-` < `:`), so it is the last
    // hit of the last page.
    let sibling = "file:ax-2:shared-key".to_string();
    let cap = SCOPED_SWEEP_MAX_PAGES * SCOPED_SWEEP_PAGE;

    for candidates in [
        0,
        1,
        SCOPED_SWEEP_PAGE - 1,
        SCOPED_SWEEP_PAGE,
        SCOPED_SWEEP_PAGE + 1,
        cap,
        cap + 1,
    ] {
        // `candidates` counts what the QUERY returns. One of them is the
        // sibling; the rest are this corpus's own documents, all of which the
        // sweep must remove.
        let own = candidates.saturating_sub(1);
        {
            let mut st = ep.state.lock().unwrap();
            st.docs.clear();
            st.legacy_text_catalog_fields = vec!["prefix".into()];
            for n in 0..own {
                st.docs.insert(
                    (
                        catalog::CATALOG_INDEX.to_string(),
                        format!("file-alias:ax:{n:06}"),
                    ),
                    json!({"doc_kind": "file", "prefix": "ax", "path": "LICENSE"}),
                );
            }
            if candidates > 0 {
                st.docs.insert(
                    (catalog::CATALOG_INDEX.to_string(), sibling.clone()),
                    json!({"doc_kind": "file", "prefix": "ax-2", "path": "LICENSE"}),
                );
            }
        }

        let swept = delete_catalog_docs_scoped(&es, &[("prefix", "ax"), ("path", "LICENSE")]);

        // Read every verdict out and RELEASE the endpoint's lock before
        // asserting, for the reason the test above gives: an assert that fires
        // while holding it poisons the mutex the server thread also takes.
        let (survivors, sibling_kept) = {
            let st = ep.state.lock().unwrap();
            let survivors = st
                .docs
                .keys()
                .filter(|(index, _)| index == catalog::CATALOG_INDEX)
                .count();
            let sibling_kept = st
                .docs
                .contains_key(&(catalog::CATALOG_INDEX.to_string(), sibling.clone()));
            (survivors, sibling_kept)
        };

        if candidates > cap {
            assert!(
                swept.is_err(),
                "#890: past {cap} candidates the sweep must refuse rather than report a \
                 partial removal ({candidates} candidates)"
            );
            assert_eq!(
                survivors, candidates,
                "#890: a refusal must delete NOTHING — a partial sweep reported as a failure \
                 is still a partial sweep ({candidates} candidates)"
            );
            continue;
        }

        swept.unwrap_or_else(|err| {
            panic!("#890: {candidates} candidates is inside the page budget, not past it: {err}")
        });
        assert_eq!(
            survivors,
            usize::from(candidates > 0),
            "#890: every one of this corpus's {own} documents must be swept, across page \
             boundaries as much as inside one page ({candidates} candidates)"
        );
        assert!(
            sibling_kept || candidates == 0,
            "#890: the sibling corpus's document must survive the re-check on whichever page \
             it lands ({candidates} candidates)"
        );
    }
}

/// #905 (data loss), end to end through the real finalize: a run over one
/// corpus must not delete a SIBLING corpus's duplicate-alias catalog document.
///
/// `autoindex-catalog` is global across every corpus on the node and `path` is
/// a logical path INSIDE a corpus, so the duplicate-alias sweep's
/// `{status: duplicate} + {path: [...]}` query reaches every corpus that has a
/// duplicate at the same relative path. Two checkouts that each carry
/// `LICENSE.csv` and `docs/LICENSE.csv` is the ordinary shape of that, not a
/// contrived one — and unlike #890 it needs no legacy mapping: this fixture's
/// catalog is the current `keyword` one.
///
/// The corpus next door is the one in
/// `a_file_shared_with_another_corpus_on_the_same_node_is_not_a_fatal_condition`
/// with ONE line added: its own duplicate at the same relative path. That is
/// what puts `docs/LICENSE.csv` into the second run's alias-sweep list and
/// makes the sweep reach next door.
///
/// Both runs take the DEFAULT graph path (`no_graph: false`), because that is
/// where this sweep lives: a `--no-graph` run returns from the generated path
/// several thousand lines earlier and retires stale catalog documents by their
/// prefix-scoped `_id` instead (`generation_catalog::managed_non_run_ids`),
/// which was never unscoped. That is the honest blast radius of #905.
///
/// FAIL-BEFORE, measured: with the body of `sweep_duplicate_aliases` replaced
/// by the `es.delete_by_query(CATALOG_INDEX, &alias_sweep_query(paths))` this
/// PR removes — i.e. the code as it stands on `main` — the run ends with ONE
/// alias document instead of two: the second corpus swept the first corpus's
/// away and then wrote its own.
#[test]
fn this_corpus_finalize_leaves_a_sibling_corpus_alias_document_alone() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let shared = "id,value\n1,apache-2.0\n2,copied-verbatim\n";
    let endpoint = HttpEndpoint::start();

    let first = tempfile::tempdir().unwrap();
    let first_state = tempfile::tempdir().unwrap();
    fs::create_dir(first.path().join("docs")).unwrap();
    fs::write(first.path().join("LICENSE.csv"), shared).unwrap();
    fs::write(first.path().join("docs").join("LICENSE.csv"), shared).unwrap();
    let mut config = cfg(first.path(), first_state.path(), &endpoint.url, false);
    config.no_graph = false;
    config.brain = Some("testbrain".into());
    let (code, summary) = run_index_report(config).expect("the first corpus indexes");
    let summary = summary.expect("the first corpus reports");
    assert_eq!(code, 0);
    assert_eq!(
        summary["duplicate_files"], 1,
        "the first corpus must publish one alias document, or there is nothing \
         next door for the second run to reach: {summary}"
    );

    // The second corpus: its own state dir and its own --prefix, exactly the
    // isolation the CLI's refusals recommend — and its own duplicate at the
    // SAME relative path, which is what the first corpus's alias sits at.
    let second = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    fs::create_dir(second.path().join("docs")).unwrap();
    fs::write(second.path().join("LICENSE.csv"), shared).unwrap();
    fs::write(second.path().join("docs").join("LICENSE.csv"), shared).unwrap();
    let mut config = cfg(second.path(), second_state.path(), &endpoint.url, false);
    config.prefix = "incremental-http-second".into();
    config.no_graph = false;
    config.brain = Some("testbrain-second".into());
    let (code, summary) = run_index_report(config).expect("the second corpus indexes");
    let summary = summary.expect("the second corpus reports");
    assert_eq!(code, 0, "the second corpus is a clean run: {summary}");
    assert_eq!(
        summary["duplicate_files"], 1,
        "the second corpus publishes its own alias at the same path: {summary}"
    );

    let aliases: Vec<(String, String)> = {
        let st = endpoint.state.lock().unwrap();
        let mut aliases: Vec<(String, String)> = st
            .docs
            .iter()
            .filter(|((index, _), doc)| {
                index == catalog::CATALOG_INDEX
                    && doc.get("status").and_then(Value::as_str) == Some("duplicate")
            })
            .map(|((_, id), doc)| {
                (
                    id.clone(),
                    doc.get(catalog::CORPUS_SCOPE_FIELD)
                        .and_then(Value::as_str)
                        .unwrap_or("<no corpus_scope>")
                        .to_string(),
                )
            })
            .collect();
        aliases.sort();
        aliases
    };
    // Sorted by corpus, not by `_id`: the two ids share a body and differ only
    // in their prefix, where `-` sorts before `:`.
    let mut scopes: Vec<&str> = aliases.iter().map(|(_, scope)| scope.as_str()).collect();
    scopes.sort_unstable();
    assert_eq!(
        scopes,
        vec!["incremental-http", "incremental-http-second"],
        "#905: both corpora must keep their own duplicate-alias document — the \
         second corpus's sweep is scoped to itself, not to the logical path. \
         Aliases: {aliases:?}"
    );
}

/// #905, at the sweep itself: every identity scheme the catalog can still hold.
///
/// The end-to-end test above proves the wiring on documents this build wrote.
/// This one proves the part a naive scope filter would have broken. The sweep
/// exists BECAUSE alias ids changed as identity evolved, so it has to keep
/// reaching this corpus's older documents while stopping at the corpus
/// boundary — and the older a document is, the less corpus evidence it carries:
///
/// * current (`_source.corpus_scope`, #755/rc.72)
/// * `_source.prefix` only (#737/rc.68)
/// * neither, but a prefix-scoped `_id` (#416/rc.57)
/// * neither, and an `_id` with no prefix in it either (rc.10..rc.56) — the
///   case the issue calls out, decided by reconstructing the ids THIS corpus
///   would have written rather than by guessing.
///
/// The last document is the honest residual: anonymous, and NOT one of this
/// corpus's reconstructions (a different `file_key`). It is left in place —
/// deleting a document that names no owner is the reach this fix removes — and
/// it is reported, non-fatally, by `stranded_alias_notes`.
///
/// FAIL-BEFORE, measured: with `sweep_duplicate_aliases`'s body replaced by the
/// unscoped `delete_by_query` this PR removes, all four of the sibling's and
/// the anonymous stranger's documents are deleted too — `Survivors: []`.
#[test]
fn the_duplicate_alias_sweep_scopes_every_identity_scheme_to_this_corpus() {
    let ep = HttpEndpoint::start();
    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    // The logical path both corpora hold a duplicate at.
    let path = "docs/LICENSE.csv";
    let key = "shared-key";
    let path_id = "pid-docs-license";

    // This corpus ("ax"), one document per scheme. All of these must be swept.
    let ax_current = catalog::duplicate_file_id("ax", key, path, path_id);
    let ax_prefix_field = "file-alias:ax:00000000000000000000000000000737".to_string();
    let ax_id_only = "file-alias:ax:00000000000000000000000000000416".to_string();
    let ax_anonymous = catalog::unprefixed_duplicate_file_ids(key, path, path_id);
    // The sibling ("bx"), the same three attributable schemes. All must survive.
    let bx_current = catalog::duplicate_file_id("bx", key, path, path_id);
    let bx_prefix_field = "file-alias:bx:00000000000000000000000000000737".to_string();
    let bx_id_only = "file-alias:bx:00000000000000000000000000000416".to_string();
    // Anonymous, and not reconstructible as ours: a different `file_key`, so a
    // pre-#416 build of SOME corpus wrote it and nothing says which.
    let anonymous_stranger =
        catalog::unprefixed_duplicate_file_ids("someone-elses-key", path, "")[0].clone();

    let alias_doc = |scope: Option<&str>, prefix: Option<&str>| {
        let mut doc = json!({
            "doc_kind": "file",
            "file_key": key,
            "path": path,
            "format": "duplicate",
            "status": "duplicate",
        });
        if let Some(scope) = scope {
            doc[catalog::CORPUS_SCOPE_FIELD] = json!(scope);
        }
        if let Some(prefix) = prefix {
            doc["prefix"] = json!(prefix);
        }
        doc
    };
    let mut seeded: Vec<String> = vec![
        ax_current.clone(),
        ax_prefix_field.clone(),
        ax_id_only.clone(),
        bx_current.clone(),
        bx_prefix_field.clone(),
        bx_id_only.clone(),
        anonymous_stranger.clone(),
    ];
    seeded.extend(ax_anonymous.iter().cloned());
    {
        let mut st = ep.state.lock().unwrap();
        let mut put = |id: &String, doc: Value| {
            st.docs
                .insert((catalog::CATALOG_INDEX.to_string(), id.clone()), doc);
        };
        put(&ax_current, alias_doc(Some("ax"), Some("ax")));
        put(&ax_prefix_field, alias_doc(None, Some("ax")));
        put(&ax_id_only, alias_doc(None, None));
        for id in &ax_anonymous {
            put(id, alias_doc(None, None));
        }
        put(&bx_current, alias_doc(Some("bx"), Some("bx")));
        put(&bx_prefix_field, alias_doc(None, Some("bx")));
        put(&bx_id_only, alias_doc(None, None));
        put(&anonymous_stranger, alias_doc(None, None));
    }

    // What the finalize passes: the ids THIS corpus would have written for its
    // own aliases under the pre-#416 scheme, reconstructed from its plan.
    let own: HashSet<String> = catalog::unprefixed_duplicate_file_ids(key, path, path_id)
        .into_iter()
        .collect();
    let mut stranded: Vec<String> = Vec::new();
    let swept = sweep_duplicate_aliases(&es, "ax", &[path], &own, &mut stranded);

    // Read every verdict out and RELEASE the endpoint's lock before asserting:
    // an assert that fires while holding it poisons the mutex the server thread
    // also takes, and the panic-in-a-destructor that follows aborts the whole
    // test binary instead of failing this one test. This test is written to
    // fail on unfixed code, so its failure mode has to stay clean.
    let (survivors, alive): (String, Vec<String>) = {
        let st = ep.state.lock().unwrap();
        let mut alive: Vec<String> = seeded
            .iter()
            .filter(|id| {
                st.docs
                    .contains_key(&(catalog::CATALOG_INDEX.to_string(), (*id).clone()))
            })
            .cloned()
            .collect();
        alive.sort();
        (format!("{alive:?}"), alive)
    };
    swept.expect("the sweep completes");

    for (id, what) in [
        (&ax_current, "current-scheme"),
        (&ax_prefix_field, "#737 `prefix`-field"),
        (&ax_id_only, "#416 prefix-in-id"),
    ] {
        assert!(
            !alive.contains(id),
            "#905: this corpus's own {what} alias must still be swept — the scope is \
             applied per hit, not by narrowing the query. Survivors: {survivors}"
        );
    }
    for id in &ax_anonymous {
        assert!(
            !alive.contains(id),
            "#905: this corpus's pre-#416 alias carries no corpus anywhere, so it is swept \
             on a reconstructed id — not stranded. Survivors: {survivors}"
        );
    }
    for (id, what) in [
        (&bx_current, "current-scheme"),
        (&bx_prefix_field, "#737 `prefix`-field"),
        (&bx_id_only, "#416 prefix-in-id"),
    ] {
        assert!(
            alive.contains(id),
            "#905: a sibling corpus's {what} alias at the same logical path must survive — \
             deleting it is the data loss. Survivors: {survivors}"
        );
    }
    assert!(
        alive.contains(&anonymous_stranger),
        "#905: an alias that names no corpus and is not one of this corpus's own \
         reconstructions is left in place, not deleted on a guess. Survivors: {survivors}"
    );

    // …and it is reported. Silently stranding it would be the other half of the
    // trade the issue warned about.
    assert_eq!(stranded.len(), 1, "{stranded:?}");
    assert!(stranded[0].contains(path), "{stranded:?}");
    let notes = stranded_alias_notes(&stranded).join("\n");
    assert!(notes.contains("LEFT IN PLACE"), "{notes}");
    assert!(notes.contains(&anonymous_stranger), "{notes}");
    assert!(
        stranded_alias_notes(&[]).is_empty(),
        "an ordinary corpus with nothing stranded says nothing"
    );
}

/// #739: a `file:` catalog doc written by a pre-#737 binary has NO `prefix`
/// field, so the #737 prefix-scoped sweep misses it on the first upgraded run.
/// The by-`_id` delete catches it (the id encodes the prefix), while a sibling
/// corpus's same-key legacy doc — a different `_id` — survives.
#[test]
fn sweep_excluded_groups_deletes_a_legacy_main_doc_by_id() {
    let ep = HttpEndpoint::start();

    {
        let mut st = ep.state.lock().unwrap();
        // Legacy docs: no `prefix` field (pre-#737). Corpus "ax" excludes the
        // file; corpus "bx" holds a same-key copy under its own id.
        st.docs.insert(
            (
                catalog::CATALOG_INDEX.to_string(),
                "file:ax:legacy-key".to_string(),
            ),
            json!({"doc_kind": "file", "path": "legacy.csv"}),
        );
        st.docs.insert(
            (
                catalog::CATALOG_INDEX.to_string(),
                "file:bx:legacy-key".to_string(),
            ),
            json!({"doc_kind": "file", "path": "legacy.csv"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    let excluded = vec![InventoryDeltaEntry {
        file_key: "legacy-key".into(),
        path: "legacy.csv".into(),
    }];

    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    let st = ep.state.lock().unwrap();
    // ax's legacy main doc is swept by id, even without a `prefix` field.
    assert!(
        !st.docs.contains_key(&(
            catalog::CATALOG_INDEX.to_string(),
            "file:ax:legacy-key".to_string()
        )),
        "#739: the excluding corpus's legacy file: doc must be swept by id"
    );
    // The sibling corpus's same-key legacy doc survives (different id).
    assert!(
        st.docs.contains_key(&(
            catalog::CATALOG_INDEX.to_string(),
            "file:bx:legacy-key".to_string()
        )),
        "#739: a sibling corpus's legacy doc must NOT be swept"
    );
}

/// #739 (alias half): a `file-alias:` catalog doc written by a pre-#737
/// binary has NO `prefix` field either, so it survives both the #737
/// prefix-scoped alias sweep AND the #693 term-scoped one on the first
/// upgraded run — the residual the main-doc fix (#739 above) left open. The
/// frozen `plan.duplicate_files` still carries the alias's `rel`/`path_id`,
/// so the sweep can reconstruct its exact `_id` and delete it directly.
#[test]
fn sweep_excluded_groups_deletes_a_legacy_alias_doc_by_id() {
    let ep = HttpEndpoint::start();
    let legacy_alias_id = catalog::duplicate_file_id("ax", "legacy-key", "alias.csv", "pid1");
    let sibling_alias_id = catalog::duplicate_file_id("bx", "legacy-key", "alias.csv", "pid1");

    {
        let mut st = ep.state.lock().unwrap();
        // Legacy alias docs: no `prefix` field (pre-#737). Corpus "ax"
        // excludes the canonical file; corpus "bx" holds a same-key alias
        // under its own (differently prefix-encoded) id.
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), legacy_alias_id.clone()),
            json!({"doc_kind": "file", "path": "alias.csv"}),
        );
        st.docs.insert(
            (catalog::CATALOG_INDEX.to_string(), sibling_alias_id.clone()),
            json!({"doc_kind": "file", "path": "alias.csv"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    plan.duplicate_files.push(crate::state::DuplicateFile {
        file_key: "legacy-key".into(),
        rel: "alias.csv".into(),
        path_id: "pid1".into(),
        is_symlink: None,
        duplicate_of: "legacy.csv".into(),
        bytes: 0,
    });
    let excluded = vec![InventoryDeltaEntry {
        file_key: "legacy-key".into(),
        path: "legacy.csv".into(),
    }];

    sweep_excluded_groups(&es, "ax", &plan, &excluded, None, 0).expect("sweep");

    let st = ep.state.lock().unwrap();
    // ax's legacy alias doc is swept by its reconstructed id, even without a
    // `prefix` field.
    assert!(
        !st.docs
            .contains_key(&(catalog::CATALOG_INDEX.to_string(), legacy_alias_id)),
        "#739: the excluding corpus's legacy file-alias: doc must be swept by id"
    );
    // The sibling corpus's same-key legacy alias doc survives (different id).
    assert!(
        st.docs
            .contains_key(&(catalog::CATALOG_INDEX.to_string(), sibling_alias_id)),
        "#739: a sibling corpus's legacy file-alias: doc must NOT be swept"
    );
}

/// #736: the sweep also soft-invalidates INBOUND edges — ones a surviving file
/// taught that point AT the excluded file's anchor node (`dst`). Without it they
/// stay live, pointing at a node that is gone. An edge to a different node stays
/// live. Fail-before: with the dst-side invalidation reverted, the inbound edge
/// keeps no `invalid_at`.
#[test]
fn sweep_excluded_groups_invalidates_inbound_edges() {
    let ep = HttpEndpoint::start();
    let edges = ".xerj-memory-testbrain-edges";
    // The excluded file anchors in dataset slug "ds"; edges point at this id.
    let anchor = crate::ids::doc_id("ds", "key-excluded", "file");

    {
        let mut st = ep.state.lock().unwrap();
        st.saw_dataset_mapping_update = true;
        st.saw_catalog_mapping_update = true;
        // A surviving file's edge INTO the excluded file's anchor.
        st.docs.insert(
            (edges.to_string(), "edge-inbound".to_string()),
            json!({"src_file": "survivor.csv", "dst": anchor, "kind": "samedir"}),
        );
        // A surviving file's edge to some OTHER node must stay live.
        st.docs.insert(
            (edges.to_string(), "edge-other".to_string()),
            json!({"src_file": "survivor.csv", "dst": "other-node-id", "kind": "samedir"}),
        );
    }

    let es = Es::with_bulk_timeout(&ep.url, None, 30).expect("es client");
    let mut plan = Plan::default();
    plan.datasets.push(crate::state::PlanDataset {
        slug: "ds".into(),
        index: "incremental-http-ds".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 1,
        file_count: 1,
    });
    // plan.files carries the excluded file's slug so the dst anchor resolves.
    plan.files.insert(
        "key-excluded".to_string(),
        crate::state::FileAssignment {
            rel: "secret.csv".into(),
            path_id: String::new(),
            is_symlink: None,
            family: "csv".into(),
            gzip: false,
            content_digest: None,
            assignments: vec![(None, "ds".into())],
            as_document: false,
        },
    );
    let excluded = vec![InventoryDeltaEntry {
        file_key: "key-excluded".into(),
        path: "secret.csv".into(),
    }];

    let now_ms = 1_724_500_000_000i64;
    sweep_excluded_groups(&es, "ax", &plan, &excluded, Some(edges), now_ms).expect("sweep");

    let st = ep.state.lock().unwrap();
    let inbound = st
        .docs
        .get(&(edges.to_string(), "edge-inbound".to_string()))
        .expect("inbound edge present (soft-invalidated, not deleted)");
    assert_eq!(
        inbound.get("invalid_at").and_then(Value::as_i64),
        Some(now_ms),
        "#736: an inbound edge to the excluded file's anchor must be invalidated"
    );
    let other = st
        .docs
        .get(&(edges.to_string(), "edge-other".to_string()))
        .expect("other edge present");
    assert!(
        other.get("invalid_at").is_none(),
        "#736: an edge to a different node must NOT be invalidated"
    );
}

/// #736 (replacement half): a run that SUPERSEDES a file's anchor node must
/// soft-invalidate the edges pointing AT the old anchor, not only the edges the
/// old generation taught.
///
/// The reachable trigger is `--fresh`. An ordinary resume maps every file back
/// onto its planned key (`select_resume_plan_keys`), so an in-place edit keeps
/// its `file_key` and its anchor id — nothing is superseded and the inbound
/// edges keep pointing at a live node. `--fresh` discards the plan and rebuilds
/// every key from current content, so an edited file gets a NEW key and a NEW
/// anchor, while `cleanup_required` (the journal's record of what is live) has
/// just been wiped with the journal — so before this fix the run invalidated
/// nothing at all, and every edge a surviving file taught into the old anchor
/// stayed live and searchable, pointing at a superseded node.
///
/// Fail-before: with the superseded-anchor invalidation reverted, the two
/// `a.md → old b anchor` edges come back with no `invalid_at` and the first
/// assertion fires.
#[test]
fn fresh_run_invalidates_inbound_edges_to_a_superseded_anchor() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("a.md"),
        "# A\n\nSee [[b]] for the details of the target document.\n",
    )
    .unwrap();
    fs::write(
        corpus.path().join("b.md"),
        "# B\n\nThe target document says one thing.\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.no_graph = false;
    config.brain = Some("testbrain".into());
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let edges_index = detect::edges_index_name("testbrain");

    // Anchor ids are read off the published file cards (`ax_locator: "file"`),
    // so the test never has to recompute `ids::doc_id` itself.
    let anchors_of = |path: &str| -> Vec<String> {
        let st = endpoint.state.lock().unwrap();
        let mut ids: Vec<String> = st
            .docs
            .iter()
            .filter(|((index, _), doc)| {
                index != catalog::CATALOG_INDEX
                    && doc.get("ax_locator").and_then(Value::as_str) == Some("file")
                    && doc.get("ax_path").and_then(Value::as_str) == Some(path)
            })
            .map(|((_, id), _)| id.clone())
            .collect();
        ids.sort();
        ids
    };
    let edges_of = |predicate: &dyn Fn(&Value) -> bool| -> Vec<(String, Value)> {
        let st = endpoint.state.lock().unwrap();
        let mut rows: Vec<(String, Value)> = st
            .docs
            .iter()
            .filter(|((index, _), doc)| *index == edges_index && predicate(doc))
            .map(|((_, id), doc)| (id.clone(), doc.clone()))
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows
    };

    let old_b_anchor = {
        let mut found = anchors_of("b.md");
        assert_eq!(found.len(), 1, "run 1 publishes exactly one b.md file card");
        found.pop().unwrap()
    };
    // The inbound edges the fix has to reach: taught by the SURVIVING file a.md,
    // pointing at b.md's anchor. Asserted before the change so a corpus that
    // stopped producing them could never make this test vacuously pass.
    let inbound_before = edges_of(&|doc| {
        doc.get("src_file").and_then(Value::as_str) == Some("a.md")
            && doc.get("dst").and_then(Value::as_str) == Some(old_b_anchor.as_str())
    });
    assert!(
        !inbound_before.is_empty(),
        "#736: the fixture must actually teach a.md → b.md edges"
    );
    // a.md's other edges (its own card → section chain) must survive untouched:
    // a.md is byte-identical across the two runs, and #868's skip means an
    // unchanged file must neither re-teach nor lose its edges.
    let a_internal_before: Vec<String> = edges_of(&|doc| {
        doc.get("src_file").and_then(Value::as_str) == Some("a.md")
            && doc.get("dst").and_then(Value::as_str) != Some(old_b_anchor.as_str())
    })
    .into_iter()
    .map(|(id, _)| id)
    .collect();
    assert!(
        !a_internal_before.is_empty(),
        "#736: the fixture must have edges that the sweep MUST NOT touch"
    );

    // Edit b.md, then rebuild in place with --fresh: b.md's content key, and so
    // its anchor node id, is superseded.
    fs::write(
        corpus.path().join("b.md"),
        "# B\n\nThe target document now says something completely different today.\n",
    )
    .unwrap();
    let mut fresh = config.clone();
    fresh.fresh = true;
    assert_eq!(run_index(fresh).unwrap(), 0);

    let new_b_anchor = {
        let found = anchors_of("b.md");
        let fresh_ids: Vec<&String> = found.iter().filter(|id| **id != old_b_anchor).collect();
        assert_eq!(
            fresh_ids.len(),
            1,
            "--fresh must publish a new b.md file card after the edit, got {found:?}"
        );
        fresh_ids[0].clone()
    };
    assert_ne!(
        new_b_anchor, old_b_anchor,
        "the fixture must actually supersede b.md's anchor"
    );

    // The bug: every live edge pointing at the superseded anchor.
    let inbound_after =
        edges_of(&|doc| doc.get("dst").and_then(Value::as_str) == Some(old_b_anchor.as_str()));
    assert_eq!(
        inbound_after.len(),
        inbound_before.len(),
        "#736: prior edges are soft-invalidated, never deleted — the bi-temporal \
         record must still be there for `as_of`"
    );
    for (id, doc) in &inbound_after {
        assert!(
            doc.get("invalid_at").is_some(),
            "#736: edge {id} still points at b.md's superseded anchor {old_b_anchor} \
             and was left live: {doc}"
        );
    }
    // The old generation's own outbound edges go too — same root cause, same
    // pass: `src_file == b.md` was never invalidated under --fresh either.
    for (id, doc) in
        edges_of(&|doc| doc.get("src").and_then(Value::as_str) == Some(old_b_anchor.as_str()))
    {
        assert!(
            doc.get("invalid_at").is_some(),
            "#736: edge {id} is taught BY b.md's superseded anchor and was left live: {doc}"
        );
    }

    // …and the graph is still a graph: a.md links to the NEW anchor, live.
    let inbound_new = edges_of(&|doc| {
        doc.get("src_file").and_then(Value::as_str) == Some("a.md")
            && doc.get("dst").and_then(Value::as_str) == Some(new_b_anchor.as_str())
            && doc.get("invalid_at").is_none()
    });
    assert!(
        !inbound_new.is_empty(),
        "#736: the re-run must re-teach a.md → b.md against the new anchor"
    );
    // #868: the unchanged file's unrelated edges are untouched.
    for id in &a_internal_before {
        let st = endpoint.state.lock().unwrap();
        let doc = st
            .docs
            .get(&(edges_index.clone(), id.clone()))
            .unwrap_or_else(|| panic!("#868: a.md's edge {id} must survive the re-run"));
        assert!(
            doc.get("invalid_at").is_none(),
            "#868: byte-identical a.md's own edge {id} must stay live: {doc}"
        );
    }
}

/// #585 (case 1): a pending (uncommitted) `--no-graph` generation must NOT be
/// resumed and committed on the default graph path — doing so silently commits
/// a no-graph generation under a graph authority (the #584 hazard, but for a
/// not-yet-committed generation the committed-manifest guard never sees it
/// because the pending-sync branch returns first). The re-run must be refused
/// before any mutation, symmetric to #584.
#[test]
fn pending_no_graph_generation_is_refused_on_the_graph_path() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let source = corpus.path().join("rows.csv");
    fs::write(&source, "id,value\n1,first\n").unwrap();
    let endpoint = HttpEndpoint::start();

    // Commit generation 1 with the `--no-graph` generated executor.
    let no_graph = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(no_graph.clone()).unwrap(), 0);

    // Interrupt a second `--no-graph` generation after sync_begin: the data bulk
    // fails, leaving a pending sync (no-graph, uncommitted).
    fs::write(&source, "id,value\n1,first\n2,second\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    assert!(run_index(no_graph.clone()).is_err());
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 2);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    // The destination as the refused re-run will find it (the interrupted run's
    // delete-before-replace already mutated it — irrelevant to the guard).
    let docs_before_rerun = endpoint.data_docs().len();

    // Re-run the SAME state dir on the default graph path. The pending no-graph
    // generation must be refused, not resumed-and-committed.
    let mut graph = no_graph.clone();
    graph.no_graph = false;
    let error = run_index(graph).unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("pending sync") && rendered.contains("different graph authority"),
        "#585: a pending no-graph generation must be refused on the graph path with the clear message, got: {rendered}"
    );
    // No second generation committed, and the guard mutated nothing further.
    assert_eq!(
        journal_events(state_dir.path(), "sync_commit"),
        1,
        "#585: the refused graph re-run must not commit the pending no-graph generation"
    );
    assert_eq!(
        endpoint.data_docs().len(),
        docs_before_rerun,
        "#585: the refused re-run must mutate nothing"
    );
}

/// #585 (case 2): a `--no-graph` generation's genesis bootstrap (`sync_bootstrap`
/// committed, interrupted before `sync_begin`) must NOT be continued on the
/// default graph path either. Genesis carries no execution identity of its own
/// (`validate_genesis` requires `execution: None`), so unlike case 1 the guard
/// cannot compare a recorded mode — it instead relies on `sync_bootstrap` having
/// actually replayed, which only `begin_non_graph_generation` ever writes, and
/// only under `--no-graph`.
#[test]
fn no_graph_genesis_bootstrap_is_refused_on_the_graph_path() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(corpus.path().join("a.csv"), "id,value\n1,alpha\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let mut no_graph = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    no_graph.snapshot_max_bytes = 1;

    // Interrupt the very first --no-graph run between sync_bootstrap and
    // sync_begin: generation 0 (genesis) is committed, nothing else is.
    let error = run_index(no_graph.clone()).unwrap_err();
    assert!(format!("{error:#}").contains("snapshot source footprint"));
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_docs().len(), 0);

    // Re-run the SAME state dir on the default graph path. The pending
    // --no-graph genesis bootstrap must be refused, not continued.
    let mut graph = no_graph.clone();
    graph.no_graph = false;
    graph.snapshot_max_bytes = 64 << 30;
    let error = run_index(graph.clone()).unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("genesis bootstrap") && rendered.contains("different graph authority"),
        "#585 case 2: a --no-graph genesis bootstrap must be refused on the graph path with the \
         clear message, got: {rendered}"
    );
    // #718: the message must name every valid recovery route, including
    // `--fresh` — the guard exempts it (nothing is committed past genesis), so
    // omitting it hides a working recovery from the operator.
    assert!(
        rendered.contains("--fresh"),
        "#718: the recovery message must offer --fresh, which the guard exempts, got: {rendered}"
    );
    // No mutation: nothing indexed, no new durable transaction beyond the
    // original sync_bootstrap.
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_docs().len(), 0);

    // #718: `--dry-run` does not exempt the refusal. A graph-path dry run over
    // the pending --no-graph genesis is refused the same way (there is no graph
    // genesis projection to show), with the same recovery message, and mutates
    // nothing — a dry run of a refused run is a refusal.
    let mut graph_dry = graph;
    graph_dry.dry_run = true;
    let dry_error = run_index(graph_dry).unwrap_err();
    let dry_rendered = format!("{dry_error:#}");
    assert!(
        dry_rendered.contains("genesis bootstrap") && dry_rendered.contains("--fresh"),
        "#718: --dry-run over a genesis on the graph path is refused too, got: {dry_rendered}"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    assert_eq!(endpoint.data_docs().len(), 0);

    // Positive control: re-running in the SAME (--no-graph) mode continues
    // the interrupted genesis normally.
    no_graph.snapshot_max_bytes = 64 << 30;
    assert_eq!(run_index(no_graph).unwrap(), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_bootstrap"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(endpoint.data_docs().len(), 1);
}

// ── #929: one dataset the server refuses must cost that dataset, not the run ──

/// A two-dataset corpus: tabular rows and prose. Which dataset the plan lists
/// first is an implementation detail, so the tests below refuse each in turn —
/// one of the two orders necessarily puts the refusal BEFORE an accepted
/// dataset, which is the order that used to lose the accepted one.
fn write_two_dataset_corpus(root: &Path) {
    fs::write(root.join("a.csv"), "id,value\n1,alpha\n2,beta\n").unwrap();
    fs::write(root.join("b.csv"), "id,value\n3,gamma\n").unwrap();
    fs::write(
        root.join("notes.md"),
        "# Notes\n\nTiered merge policy picks segments of similar size.\n",
    )
    .unwrap();
}

/// The two dataset indices that corpus produces, learned from a clean run so
/// the tests do not hard-code a slug scheme: `(tabular, prose)`.
fn two_dataset_indices() -> (String, String) {
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(
        run_index(config).unwrap(),
        0,
        "the clean corpus indexes whole"
    );
    let docs = endpoint.data_docs();
    let index_of = |path: &str| {
        docs.iter()
            .find(|doc| doc["ax_path"] == path)
            .and_then(|doc| doc["ax_dataset"].as_str())
            .map(|slug| format!("incremental-http-{slug}"))
            .unwrap_or_else(|| panic!("{path} was not indexed by the clean run"))
    };
    let (tabular, prose) = (index_of("a.csv"), index_of("notes.md"));
    assert_ne!(tabular, prose, "the fixture must produce two datasets");
    (tabular, prose)
}

fn refused_detail(summary: &Value) -> Vec<Value> {
    serde_json::from_str(
        summary["refused_datasets_json"]
            .as_str()
            .expect("a run that refused a dataset carries refused_datasets_json"),
    )
    .unwrap()
}

/// #929, the defect itself. One dataset's mapping is refused with the literal
/// 400 from the report; the other dataset must be indexed, the run must end 3
/// (not 1), and the generation that commits must SAY it lacks a dataset — in
/// its summary, in the catalog's run document and on each file it cost.
#[test]
fn a_refused_dataset_mapping_costs_that_dataset_not_the_run() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (tabular, prose) = two_dataset_indices();

    for (refused_index, lost, kept) in [
        (&tabular, vec!["a.csv", "b.csv"], vec!["notes.md"]),
        (&prose, vec!["notes.md"], vec!["a.csv", "b.csv"]),
    ] {
        let corpus = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write_two_dataset_corpus(corpus.path());
        let endpoint = HttpEndpoint::start();
        endpoint.state.lock().unwrap().refused_dataset_mappings = vec![refused_index.clone()];
        let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

        let (code, summary) = run_index_report(config.clone())
            .unwrap_or_else(|error| panic!("refusing {refused_index} aborted the run: {error:#}"));
        assert_eq!(code, 3, "a refused dataset is exit 3, never 0 and never 1");
        let summary = summary.expect("generated run returns its committed run projection");

        // Every dataset's mapping was attempted: the loop did not stop at the
        // refusal, whichever side of it the accepted dataset sat on.
        let attempted = endpoint.state.lock().unwrap().dataset_mapping_puts.clone();
        assert!(
            attempted.contains(&tabular) && attempted.contains(&prose),
            "both datasets must be attempted, got {attempted:?}"
        );

        // The rest of the corpus is indexed and nothing of the refused dataset is.
        let docs = endpoint.data_docs();
        assert_eq!(
            paths(&docs),
            kept,
            "refusing {refused_index} must index exactly the other dataset"
        );
        assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);

        // The committed generation names what it lacks.
        assert_eq!(summary["datasets_refused"], 1);
        assert_eq!(summary["files_refused"], lost.len());
        assert_eq!(summary["files_junk"], lost.len());
        // Every file is still accounted for, and none is counted twice: what
        // is indexed is exactly what was kept, and the rest is the refusal.
        assert_eq!(summary["files_indexed"], kept.len(), "{summary}");
        assert_eq!(summary["files_total"], kept.len() + lost.len(), "{summary}");
        let detail = refused_detail(&summary);
        assert_eq!(detail.len(), 1);
        assert_eq!(detail[0]["index"], refused_index.as_str());
        assert_eq!(detail[0]["files"], lost.len());
        let reason = detail[0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("400") && reason.contains("nested semantic_text field"),
            "the server's own words travel with the refusal, got: {reason}"
        );

        // …and so does every file it cost, in the catalog a reader starts from.
        let catalog = endpoint.catalog_docs();
        let mut junked: Vec<&str> = catalog
            .iter()
            .filter(|doc| doc["doc_kind"] == "file" && doc["status"] == "junk")
            .map(|doc| {
                assert!(
                    doc["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("was refused by the server")),
                    "a refused file says why it is not indexed: {doc}"
                );
                doc["path"].as_str().or(doc["rel"].as_str()).unwrap_or("")
            })
            .collect();
        junked.sort_unstable();
        assert_eq!(junked, lost, "catalog file documents for the refused files");
        assert!(
            !catalog
                .iter()
                .any(|doc| doc["doc_kind"] == "dataset" && doc["index"] == refused_index.as_str()),
            "a refused dataset must not be published as a dataset document"
        );

        // A no-op re-run is still exit 3, still names the dataset, and writes
        // nothing: the refusal is part of the committed generation, not a log
        // line only the first run printed.
        let bulks = endpoint.data_bulk_requests();
        let (again, again_summary) = run_index_report(config).unwrap();
        assert_eq!(again, 3);
        let again_summary = again_summary.unwrap();
        assert_eq!(again_summary["generation"], 1);
        assert_eq!(again_summary["datasets_refused"], 1);
        assert_eq!(refused_detail(&again_summary), detail);
        assert_eq!(endpoint.data_bulk_requests(), bulks);
        assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    }
}

/// #929: the tolerance is narrow on purpose. A 503 on the same request is the
/// ENDPOINT failing — it says nothing about the dataset — so routing around it
/// would publish a corpus that silently lacks data for a transient reason.
#[test]
fn an_unavailable_dataset_mapping_still_aborts_the_run() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (tabular, _) = two_dataset_indices();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().unavailable_dataset_mappings = vec![tabular.clone()];
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let error = run_index(config).unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("503") && rendered.contains(&tabular),
        "an endpoint failure names the request that failed, got: {rendered}"
    );
    assert_eq!(endpoint.data_docs().len(), 0, "nothing is published");
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
}

/// #929: a refusal must not turn into "this corpus can never be re-indexed".
/// The refused files are junk in the committed plan, so an incremental run has
/// to carry the refusal forward — for the unchanged file, for a CHANGED file of
/// the refused shape, and for a NEW one — while everything else reconciles.
#[test]
fn a_refused_dataset_stays_refused_across_an_incremental_generation() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (tabular, _) = two_dataset_indices();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().refused_dataset_mappings = vec![tabular.clone()];
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 3);
    assert_eq!(paths(&endpoint.data_docs()), ["notes.md"]);

    fs::write(
        corpus.path().join("a.csv"),
        "id,value\n1,alpha\n2,beta\n9,changed\n",
    )
    .unwrap();
    fs::write(corpus.path().join("c.csv"), "id,value\n4,delta\n").unwrap();
    fs::write(
        corpus.path().join("more.md"),
        "# More\n\nA log-structured merge tree compacts sorted runs.\n",
    )
    .unwrap();

    let (code, summary) = run_index_report(config)
        .unwrap_or_else(|error| panic!("the incremental run aborted: {error:#}"));
    assert_eq!(code, 3);
    let summary = summary.unwrap();
    assert_eq!(summary["generation"], 2);
    assert_eq!(summary["datasets_refused"], 1);
    assert_eq!(
        summary["files_refused"], 3,
        "unchanged b.csv, changed a.csv and new c.csv are all refused"
    );
    let detail = refused_detail(&summary);
    assert_eq!(detail[0]["index"], tabular.as_str());
    assert_eq!(paths(&endpoint.data_docs()), ["more.md", "notes.md"]);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 2);
}

/// #929: once a generation is sealed a dataset can no longer leave it, so a
/// server that starts refusing a dataset it accepted before is fatal — but the
/// error names EVERY refused dataset and the one recovery that works, and the
/// committed generation is left exactly as it was.
#[test]
fn a_refusal_of_an_already_committed_dataset_is_fatal_and_names_it() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (tabular, prose) = two_dataset_indices();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    assert_eq!(run_index(config.clone()).unwrap(), 0);
    let committed_docs = endpoint.data_docs();

    endpoint.state.lock().unwrap().refused_dataset_mappings = vec![tabular.clone(), prose.clone()];
    fs::write(corpus.path().join("c.csv"), "id,value\n4,delta\n").unwrap();

    let error = run_index(config).unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("2 dataset(s)") && rendered.contains("already sealed"),
        "got: {rendered}"
    );
    for index in [&tabular, &prose] {
        assert!(
            rendered.contains(index.as_str()),
            "every refused dataset is named, not just the first; missing {index} in: {rendered}"
        );
    }
    assert!(rendered.contains("new --state-dir") && rendered.contains("new --prefix"));
    assert_eq!(
        endpoint.data_docs(),
        committed_docs,
        "no document was published"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
}

// ── #931: the generated path reports the phase it is actually in ─────────────

/// Every `xerj-progress` line of a captured stream, as `key -> value` maps, in
/// the order they were emitted.
fn progress_lines(stream: &str) -> Vec<BTreeMap<String, String>> {
    stream
        .lines()
        .filter_map(|line| line.strip_prefix("xerj-progress "))
        .map(|line| {
            line.split_whitespace()
                .filter_map(|pair| pair.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect()
        })
        .collect()
}

/// #931, the defect itself. On the generated `--no-graph` route the stream used
/// to say `phase=scan … pct=100.0 … eta_quality=stalled` from the end of the
/// scan until the run exited — through mapping installation, sealing, every
/// bulk request and the read-back barrier — which is byte for byte what the
/// real end-of-scan hang prints. Each of those steps is now a phase of its own
/// with a real denominator, so `scan` can only ever mean scan.
#[test]
fn the_generated_path_reports_indexing_as_indexing_not_as_a_stalled_scan() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index(config).unwrap()
    };
    assert_eq!(code, 0);
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    let lines = progress_lines(&stream);

    // The phases, de-duplicated in order of first appearance.
    let mut phases: Vec<&str> = Vec::new();
    for line in &lines {
        let phase = line["phase"].as_str();
        if phases.last() != Some(&phase) {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        [
            "walk",
            "hash",
            "scan",
            "prepare",
            "snapshot",
            "index",
            "finalize-catalog",
            "finalize-refresh",
            "finalize-verify"
        ],
        "each long step of the generated route is its own phase, in order:\n{stream}"
    );

    // `scan` never reappears once the run has moved on — the exact symptom.
    let left_scan = lines
        .iter()
        .position(|line| line["phase"] == "prepare")
        .unwrap();
    assert!(
        lines[left_scan..]
            .iter()
            .all(|line| line["phase"] != "scan"),
        "a line after `prepare` still claims the scan phase:\n{stream}"
    );

    // The `index` phase has an honest denominator in both units: the three
    // files still to apply, and the sealed bulk bytes they will send.
    let index_opened = lines
        .iter()
        .find(|line| line["phase"] == "index")
        .expect("the index phase opens with a line of its own");
    assert_eq!(index_opened["items"], "0/3", "{index_opened:?}");
    assert_eq!(index_opened["basis"], "bytes", "{index_opened:?}");
    let (done, total) = index_opened["bytes"].split_once('/').unwrap();
    assert_eq!(done, "0");
    assert!(
        total.parse::<u64>().unwrap() > 0,
        "the index phase counts sealed bulk bytes, got {index_opened:?}"
    );
    // `finalize-refresh` is counted in indices: both datasets and the catalog.
    // Without a phase of its own this step held `finalize-verify` at `0/N`.
    let refresh_opened = lines
        .iter()
        .find(|line| line["phase"] == "finalize-refresh")
        .unwrap();
    assert_eq!(refresh_opened["items"], "0/3", "{refresh_opened:?}");
    // `snapshot` is denominated in the SOURCE bytes it copies and seals.
    let source_bytes: u64 = ["a.csv", "b.csv", "notes.md"]
        .iter()
        .map(|name| fs::metadata(corpus.path().join(name)).unwrap().len())
        .sum();
    let snapshot_opened = lines
        .iter()
        .find(|line| line["phase"] == "snapshot")
        .unwrap();
    assert_eq!(snapshot_opened["items"], "0/3");
    assert_eq!(snapshot_opened["bytes"], format!("0/{source_bytes}"));

    // The run closes its own stream, successfully, exactly once.
    let done: Vec<&str> = stream
        .lines()
        .filter(|line| line.starts_with("xerj-done "))
        .collect();
    assert_eq!(done.len(), 1, "{stream}");
    assert!(
        done[0].contains("ok=true") && done[0].contains("exit=0"),
        "{}",
        done[0]
    );
}

/// #931 + #929 together: a refusal is announced on the progress surface (it is
/// a `warn`, which `--quiet` does not silence) and the terminal line carries
/// the counts, so an agent reading only the machine stream learns that the
/// corpus lacks a dataset without parsing prose.
#[test]
fn a_refusal_is_on_the_progress_stream_and_the_terminal_line() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (tabular, _) = two_dataset_indices();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().refused_dataset_mappings = vec![tabular.clone()];
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&buffer);
        run_index(config).unwrap()
    };
    assert_eq!(code, 3);
    let stream = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(
        stream.contains("REFUSED by the server")
            && stream.contains("every other dataset continues"),
        "the refusal is announced while the run continues:\n{stream}"
    );
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .expect("the run closes its stream");
    assert!(
        done.contains("ok=true")
            && done.contains("exit=3")
            && done.contains("datasets_refused=1")
            && done.contains("files_refused=2"),
        "the terminal line carries the refusal counts, got: {done}"
    );
    // The index phase counts only what is left to index: the one prose file.
    let index_opened = progress_lines(&stream)
        .into_iter()
        .find(|line| line["phase"] == "index")
        .unwrap();
    assert_eq!(index_opened["items"], "0/1", "{index_opened:?}");
}

/// #929, the degenerate end: the server refuses EVERY dataset. Nothing can be
/// indexed, and the run must say exactly that — a committed generation with no
/// documents, exit 3, every file accounted for as refused — rather than panic
/// on an empty plan or, worse, report an empty corpus as a clean success. A
/// wrapper that verifies with `_count > 0` (xc-index.sh) then keeps its old
/// index, which is the right outcome.
#[test]
fn refusing_every_dataset_commits_nothing_searchable_and_is_not_a_success() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (tabular, prose) = two_dataset_indices();
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().refused_dataset_mappings = vec![tabular, prose];
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config)
        .unwrap_or_else(|error| panic!("refusing every dataset aborted the run: {error:#}"));
    assert_eq!(code, 3, "an empty corpus is never exit 0");
    let summary = summary.unwrap();
    assert_eq!(summary["datasets_refused"], 2, "{summary}");
    assert_eq!(summary["files_refused"], 3, "{summary}");
    assert_eq!(summary["files_indexed"], 0, "{summary}");
    assert_eq!(summary["records_total"], 0, "{summary}");
    assert_eq!(
        endpoint.data_docs().len(),
        0,
        "nothing was searchable to publish"
    );
    assert_eq!(endpoint.data_bulk_requests(), 0);
}

/// #931: a resumed run must not credit itself with an earlier attempt's writes.
///
/// The replay is interrupted after two of three operations committed. The
/// resumed run then has to (a) open at `replay`, not at a `scan` it is not
/// doing, and (b) size its `index` phase by what is LEFT — one operation — so
/// it starts at 0% of the remaining work. Counting all three would open the
/// phase at 0/3 and finish it at 1/3: a bar that stops at 33% on a run that
/// completed. Counting the two committed ones as done would open it at 67% for
/// work this invocation never did.
#[test]
fn a_resumed_replay_opens_at_replay_and_counts_only_what_remains() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _replay_guard = sync_executor::REPLAY_FAILPOINT_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _sink_guard = crate::progress::SINK_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_two_dataset_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().fail_data_bulk_number = Some(3);
    let mut config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);
    config.quiet = false;
    config.progress = crate::progress::ProgressMode::Plain;

    let first = Arc::new(Mutex::new(Vec::new()));
    {
        let _sink = crate::progress::install_test_sink(&first);
        run_index(config.clone()).expect_err("the third data bulk is refused");
    }
    assert_eq!(journal_events(state_dir.path(), "sync_begin"), 1);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 0);
    let first = String::from_utf8(first.lock().unwrap().clone()).unwrap();
    assert!(
        first
            .lines()
            .any(|line| line.starts_with("xerj-done ") && line.contains("ok=false")),
        "the interrupted attempt closes its own stream as a failure:\n{first}"
    );

    let resumed = Arc::new(Mutex::new(Vec::new()));
    let code = {
        let _sink = crate::progress::install_test_sink(&resumed);
        run_index(config).unwrap()
    };
    assert_eq!(code, 0);
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(paths(&endpoint.data_docs()), ["a.csv", "b.csv", "notes.md"]);

    let stream = String::from_utf8(resumed.lock().unwrap().clone()).unwrap();
    let lines = progress_lines(&stream);
    assert_eq!(
        lines.first().map(|line| line["phase"].as_str()),
        Some("replay"),
        "a resumed run says it is replaying:\n{stream}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line["phase"] != "scan" && line["phase"] != "snapshot"),
        "a resume re-scans and re-seals nothing, so it must not claim to:\n{stream}"
    );
    let index_lines: Vec<_> = lines
        .iter()
        .filter(|line| line["phase"] == "index")
        .collect();
    assert_eq!(
        index_lines.first().map(|line| line["items"].as_str()),
        Some("0/1"),
        "two of three operations were already committed; one remains:\n{stream}"
    );
    let done = stream
        .lines()
        .find(|line| line.starts_with("xerj-done "))
        .unwrap();
    assert!(
        done.contains("ok=true") && done.contains("exit=0"),
        "{done}"
    );
}

/// #944 on the generated (`--no-graph`) path: one data bulk comes back with
/// half its items answered 429 by the engine's memory circuit breaker.
/// `checked_bulk` used to abort the run on the spot — the literal end of a
/// 48,533-file run at 60%. The rejected half is re-sent, and only that half;
/// every record lands; the run commits its generation and exits 0; and the
/// summary says it happened.
#[test]
fn a_per_item_429_in_a_sealed_bulk_is_resent_and_the_generation_commits() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("rows.csv"),
        "id,value\n1,first\n2,second\n3,third\n4,fourth\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    endpoint
        .state
        .lock()
        .unwrap()
        .throttle_next_data_bulk_odd_items = true;
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    assert_eq!(code, 0, "a transient per-item 429 must not end the run");
    let summary = summary.unwrap();
    assert_eq!(summary["bulk_retries"], 1, "{summary}");
    assert_eq!(summary["bulk_items_reissued"], 2, "{summary}");
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(
        endpoint.data_docs().len(),
        4,
        "every rejected record landed on the re-send"
    );
    let counts = endpoint.state.lock().unwrap().data_bulk_item_counts.clone();
    assert_eq!(
        &counts[..2],
        &[4, 2],
        "the re-send carried exactly the two rejected items: {counts:?}"
    );
}

/// #955 on the generated (`--no-graph`) path — the literal end of the
/// full-corpus verification run, scaled down. The engine refuses a `_bulk` of
/// more than `max_actions_per_bulk` actions whole, with ONE item of status 413.
/// The catalog projection holds a document per file and went out as one
/// request, so on a 48,533-file corpus (51,129 actions against a limit of
/// 50,000) the run ended `exit=1 reason=aborted` in `finalize-catalog`, after
/// all 47,444 operations had been applied:
///
/// ```text
/// error: prepared bulk contained 1 rejected items: {"type":"engine_exception",
/// "reason":"bulk request contains 102258 lines (~51129 actions); exceeds
/// max_actions_per_bulk of 50000","status":413}
/// ```
///
/// Here the limit is 2 actions and the corpus has seven files. Every refused
/// request — a data bulk and the catalog alike — is halved until it lands; the
/// generation commits; the catalog the run publishes is the one an unlimited
/// server gets; and the summary says the server pushed back on size.
#[test]
fn a_bulk_the_server_refuses_as_too_large_is_halved_and_the_generation_commits() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let write_corpus = |root: &Path| {
        fs::write(
            root.join("rows.csv"),
            "id,value\n1,first\n2,second\n3,third\n4,fourth\n5,fifth\n",
        )
        .unwrap();
        for name in ["a", "b", "c", "d", "e", "f"] {
            fs::write(
                root.join(format!("{name}.md")),
                format!("# {name}\n\nNotes about {name}, long enough to be prose.\n"),
            )
            .unwrap();
        }
    };
    let catalog_shape = |endpoint: &HttpEndpoint| {
        let mut kinds: Vec<String> = endpoint
            .catalog_docs()
            .iter()
            .map(|doc| {
                format!(
                    "{}:{}:{}",
                    doc["doc_kind"].as_str().unwrap_or("?"),
                    doc["path"]
                        .as_str()
                        .or_else(|| doc["slug"].as_str())
                        .unwrap_or(""),
                    doc["records"]
                        .as_u64()
                        .or_else(|| doc["record_count"].as_u64())
                        .unwrap_or(0)
                )
            })
            .collect();
        kinds.sort();
        kinds
    };

    // Control: the same corpus against a server with no limit.
    let control_corpus = tempfile::tempdir().unwrap();
    let control_state = tempfile::tempdir().unwrap();
    write_corpus(control_corpus.path());
    let control = HttpEndpoint::start();
    let (code, summary) = run_index_report(cfg(
        control_corpus.path(),
        control_state.path(),
        &control.url,
        false,
    ))
    .unwrap();
    assert_eq!(code, 0);
    assert!(
        summary.unwrap().get("bulk_splits").is_none(),
        "a run the server never refused for size reports nothing"
    );
    let expected_catalog = catalog_shape(&control);
    assert!(
        expected_catalog.len() > 4,
        "the control catalog must be larger than the limit for this test to mean anything: \
         {expected_catalog:?}"
    );

    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    write_corpus(corpus.path());
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().max_actions_per_bulk = Some(2);
    let (code, summary) =
        run_index_report(cfg(corpus.path(), state_dir.path(), &endpoint.url, false)).unwrap();
    assert_eq!(
        code, 0,
        "a request the server calls too large must not end the run"
    );
    let summary = summary.unwrap();
    assert!(
        summary["bulk_splits"].as_u64().unwrap_or(0) >= 1,
        "{summary}"
    );
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(
        endpoint.data_docs().len(),
        control.data_docs().len(),
        "every record landed"
    );
    assert_eq!(
        catalog_shape(&endpoint),
        expected_catalog,
        "the catalog is the one an unlimited server gets"
    );
    let locked = endpoint.state.lock().unwrap();
    assert!(
        !locked.oversize_bulks_refused.is_empty(),
        "the limit was actually met"
    );
    assert!(
        locked.bulk_line_counts.iter().all(|lines| *lines <= 4),
        "{:?}",
        locked.bulk_line_counts
    );
    // The bound is learned, not rediscovered by every request: with a limit
    // of 2 the run halves its way down once per dimension it meets, not once
    // per bulk.
    assert!(
        locked.oversize_bulks_refused.len() <= 6,
        "refused {} requests: {:?}",
        locked.oversize_bulks_refused.len(),
        locked.oversize_bulks_refused
    );
}

/// #944, second shape, on the generated path: the engine answers the whole
/// `_bulk` HTTP 429 — the literal end of the resumed full-corpus run
/// (`error: _bulk: HTTP 429 Too Many Requests`), where the transport retry gave
/// up after six attempts. Three whole-request 429s in a row are waited out
/// through the same patience as per-item ones, every record lands, the
/// generation commits, and the summary counts the re-sends.
#[test]
fn a_whole_request_429_on_a_sealed_bulk_is_resent_and_the_generation_commits() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("rows.csv"),
        "id,value\n1,first\n2,second\n3,third\n4,fourth\n",
    )
    .unwrap();
    let endpoint = HttpEndpoint::start();
    endpoint.state.lock().unwrap().throttle_whole_data_bulks = 3;
    let config = cfg(corpus.path(), state_dir.path(), &endpoint.url, false);

    let (code, summary) = run_index_report(config).unwrap();
    assert_eq!(
        code, 0,
        "a whole-request 429 that clears must not end the run"
    );
    let summary = summary.unwrap();
    assert_eq!(summary["bulk_retries"], 3, "{summary}");
    assert_eq!(summary["bulk_items_reissued"], 12, "{summary}");
    assert_eq!(journal_events(state_dir.path(), "sync_commit"), 1);
    assert_eq!(
        endpoint.data_docs().len(),
        4,
        "every record landed once the node accepted the re-send"
    );
    let counts = endpoint.state.lock().unwrap().data_bulk_item_counts.clone();
    assert_eq!(
        &counts[..4],
        &[4, 4, 4, 4],
        "the whole body is re-sent each time, nothing was accepted: {counts:?}"
    );
}

// ===========================================================================
// `--watch`: the same incremental reconcile, driven by filesystem events.
//
// The contract these prove is the one that makes the feature safe to ship: after
// ANY sequence of changes, a watched session holds the index a fresh full
// `xerj autoindex` of the same tree would have produced — same document ids,
// same record count, same sources. They drive `watch::one_pass`, which is the
// same function the live session loop calls, with the burst a working watcher
// would have reported for the change the test just made. The watcher's own
// event plumbing is covered by `watch`'s unit tests and by the live measurement
// in docs/reference/autoindex-watch.md; what is proven HERE is convergence,
// which is the part a timing-dependent test could never prove reliably.
// ===========================================================================

/// Every published data document, keyed by `(index, _id)`, with the one
/// run-scoped field removed.
///
/// `ax_run` names the invocation that wrote the document, so it differs between
/// a watched session and a fresh run by construction; everything else must be
/// identical or the two indexes are not the same index.
fn published(endpoint: &HttpEndpoint) -> BTreeMap<(String, String), Value> {
    endpoint
        .state
        .lock()
        .unwrap()
        .docs
        .iter()
        .filter(|((index, _), _)| index != catalog::CATALOG_INDEX)
        .map(|((index, id), doc)| {
            let mut doc = doc.clone();
            if let Some(object) = doc.as_object_mut() {
                object.remove("ax_run");
            }
            ((index.clone(), id.clone()), doc)
        })
        .collect()
}

/// The property that actually guards the digest cache: after a watched pass, a
/// plain `xerj autoindex` re-run — which re-hashes every byte, trusting nothing
/// — must be a no-op that changes not one document.
///
/// This is stronger than comparing against a fresh index and it isolates exactly
/// what `--watch` adds. If a carried digest ever let a changed file skip its
/// re-hash, the re-run reads the bytes, sees the difference, and republishes;
/// this assertion fails. It also holds for corpus shapes where a FRESH index
/// legitimately differs (see `fresh_full_index`).
fn a_rerun_changes_nothing(config: &IndexCfg, endpoint: &HttpEndpoint, context: &str) {
    let before = published(endpoint);
    let code = run_index(config.clone()).unwrap();
    assert!(
        matches!(code, 0 | 3),
        "{context}: the verifying re-run must complete (exit {code})"
    );
    assert_eq!(
        published(endpoint),
        before,
        "{context}: a full-hash re-run after a watched pass must change nothing — \
         the watched pass missed something"
    );
}

/// A fresh, independent full index of `root`.
///
/// Useful as ground truth only where dataset identity cannot move. An
/// incremental run — watched or not — deliberately PRESERVES the committed
/// dataset and schema identity, while a fresh run re-elects dataset slugs from
/// the corpus it sees (`reconcile_plan.rs`, module docs). So after a change that
/// alters slug election — renaming the directory a dataset was named after — a
/// fresh index writes `to/one.csv` into a dataset called `to` where the
/// incremental corpus still calls it `from`. Same paths, same bytes, different
/// dataset name and therefore different index and document id. That is the
/// incremental route's rule, not the watcher's, and a manual re-run does exactly
/// the same thing; `a_rerun_changes_nothing` is what those cases assert instead.
fn fresh_full_index(root: &Path) -> (HttpEndpoint, tempfile::TempDir) {
    let endpoint = HttpEndpoint::start();
    let state_dir = tempfile::tempdir().unwrap();
    let config = cfg(root, state_dir.path(), &endpoint.url, false);
    assert!(
        matches!(run_index(config), Ok(0) | Ok(3)),
        "the reference full index must complete"
    );
    (endpoint, state_dir)
}

/// Deterministic, dependency-free PRNG. The seed is printed with every failure
/// so a red run is replayable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*, enough for choosing between six edit kinds.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn a_watch_pass_rehashes_only_what_changed_and_publishes_it() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::write(root.join("a.csv"), "id,value\n1,alpha\n").unwrap();
    fs::write(root.join("b.csv"), "id,value\n2,beta\n").unwrap();
    fs::write(root.join("c.csv"), "id,value\n3,gamma\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);

    // Pass 1 is the full one: nothing is cached yet.
    let mut carry = crate::watch::Carry::default();
    let (outcome, (carried, _, hashed, _)) =
        crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full());
    assert_eq!(outcome.unwrap().0, 0);
    assert_eq!(
        (hashed, carried),
        (3, 0),
        "the first pass must hash the corpus"
    );
    assert_eq!(carry.len(), 3, "and cache what it hashed");

    // Pass 2: one file changed, and the watcher says so.
    fs::write(root.join("b.csv"), "id,value\n2,beta-rewritten\n").unwrap();
    let burst = crate::watch::ChangeSet::from_paths(&[root.join("b.csv")]);
    let (outcome, (carried, _, hashed, _)) =
        crate::watch::one_pass(&config, &root, &mut carry, &burst);
    assert_eq!(outcome.unwrap().0, 0);
    assert_eq!(
        (hashed, carried),
        (1, 2),
        "only the changed file may be read again"
    );

    // …and the new bytes are what the index holds.
    let values: Vec<String> = endpoint
        .data_docs()
        .iter()
        .filter_map(|doc| doc.get("value").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    assert!(
        values.iter().any(|v| v == "beta-rewritten"),
        "the changed file's new content must be live: {values:?}"
    );
    assert!(
        !values.iter().any(|v| v == "beta"),
        "and its old content must not: {values:?}"
    );

    // Nothing was skipped: a full-hash re-run changes nothing.
    a_rerun_changes_nothing(&config, &endpoint, "one file changed");
    // And on a corpus whose dataset identity cannot move, the watched index is
    // byte-for-byte the index a fresh full run would have built.
    let (reference, _reference_state) = fresh_full_index(&root);
    assert_eq!(
        published(&endpoint),
        published(&reference),
        "a watched index must equal a fresh full index"
    );
}

#[test]
fn a_deleted_files_records_stop_appearing() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::write(root.join("keep.csv"), "id,value\n1,keep\n").unwrap();
    fs::write(root.join("gone.csv"), "id,value\n2,vanishes\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full())
        .0
        .unwrap();
    assert_eq!(paths(&endpoint.data_docs()), vec!["gone.csv", "keep.csv"]);

    fs::remove_file(root.join("gone.csv")).unwrap();
    let burst = crate::watch::ChangeSet::from_paths(&[root.join("gone.csv")]);
    crate::watch::one_pass(&config, &root, &mut carry, &burst)
        .0
        .unwrap();
    assert_eq!(
        paths(&endpoint.data_docs()),
        vec!["keep.csv"],
        "a deleted file's records must stop appearing in search"
    );
    a_rerun_changes_nothing(&config, &endpoint, "deleted file");
}

#[test]
fn a_deleted_directorys_documents_stop_appearing() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("doomed/deeper")).unwrap();
    fs::write(root.join("keep.csv"), "id,value\n1,keep\n").unwrap();
    fs::write(root.join("doomed/a.csv"), "id,value\n2,doomed-a\n").unwrap();
    fs::write(root.join("doomed/deeper/b.csv"), "id,value\n3,doomed-b\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full())
        .0
        .unwrap();
    assert_eq!(
        paths(&endpoint.data_docs()),
        vec!["doomed/a.csv", "doomed/deeper/b.csv", "keep.csv"]
    );

    // A whole directory, gone. The watcher is told about the directory; the
    // files inside it produce no events of their own.
    fs::remove_dir_all(root.join("doomed")).unwrap();
    let burst = crate::watch::ChangeSet::from_paths(&[root.join("doomed")]);
    crate::watch::one_pass(&config, &root, &mut carry, &burst)
        .0
        .unwrap();
    assert_eq!(
        paths(&endpoint.data_docs()),
        vec!["keep.csv"],
        "every document under a deleted directory must stop appearing in search"
    );
    a_rerun_changes_nothing(&config, &endpoint, "deleted directory");
}

#[test]
fn an_atomic_save_rename_dance_lands_as_the_new_content() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    let target = root.join("notes.csv");
    fs::write(&target, "id,value\n1,before\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full())
        .0
        .unwrap();

    // What an editor actually does: write a sibling temp file, then rename it
    // over the target. The inode changes, the size may not.
    let temp = root.join("notes.csv.tmp~");
    fs::write(&temp, "id,value\n1,after-\n").unwrap();
    fs::rename(&temp, &target).unwrap();
    let burst = crate::watch::ChangeSet::from_paths(&[temp.clone(), target.clone()]);
    crate::watch::one_pass(&config, &root, &mut carry, &burst)
        .0
        .unwrap();

    assert_eq!(
        paths(&endpoint.data_docs()),
        vec!["notes.csv"],
        "the temp file must not survive as a document of its own"
    );
    a_rerun_changes_nothing(&config, &endpoint, "atomic save");
    let (reference, _reference_state) = fresh_full_index(&root);
    assert_eq!(published(&endpoint), published(&reference));
}

#[test]
fn a_file_replaced_by_a_directory_converges() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::write(root.join("thing"), "id,value\n1,a-file\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full())
        .0
        .unwrap();

    fs::remove_file(root.join("thing")).unwrap();
    fs::create_dir(root.join("thing")).unwrap();
    fs::write(root.join("thing/inner.csv"), "id,value\n2,now-a-dir\n").unwrap();
    let burst =
        crate::watch::ChangeSet::from_paths(&[root.join("thing"), root.join("thing/inner.csv")]);
    crate::watch::one_pass(&config, &root, &mut carry, &burst)
        .0
        .unwrap();

    assert_eq!(paths(&endpoint.data_docs()), vec!["thing/inner.csv"]);
    a_rerun_changes_nothing(&config, &endpoint, "file replaced by a directory");
}

#[test]
fn a_moved_directory_converges() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::create_dir(root.join("from")).unwrap();
    fs::write(root.join("from/one.csv"), "id,value\n1,one\n").unwrap();
    fs::write(root.join("from/two.csv"), "id,value\n2,two\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full())
        .0
        .unwrap();

    fs::rename(root.join("from"), root.join("to")).unwrap();
    // A directory rename is ONE event naming the two directories; the files
    // inside it produce none.
    let burst = crate::watch::ChangeSet::from_paths(&[root.join("from"), root.join("to")]);
    crate::watch::one_pass(&config, &root, &mut carry, &burst)
        .0
        .unwrap();

    assert_eq!(
        paths(&endpoint.data_docs()),
        vec!["to/one.csv", "to/two.csv"],
        "a moved directory's documents must follow it"
    );
    a_rerun_changes_nothing(&config, &endpoint, "moved directory");
}

/// A pass that dies mid-publish must not lose or duplicate documents: the next
/// pass — which, after a restart, has no cache at all and therefore hashes in
/// full — has to arrive at the same index a fresh run would build.
#[test]
fn a_pass_that_fails_mid_publish_converges_on_the_next_one() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::write(root.join("a.csv"), "id,value\n1,alpha\n").unwrap();
    fs::write(root.join("b.csv"), "id,value\n2,beta\n").unwrap();
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full())
        .0
        .unwrap();

    // Change a file, and make the publish of that change fail.
    fs::write(root.join("b.csv"), "id,value\n2,beta-two\n").unwrap();
    endpoint.state.lock().unwrap().fail_next_data_bulk = true;
    let burst = crate::watch::ChangeSet::from_paths(&[root.join("b.csv")]);
    let (outcome, _) = crate::watch::one_pass(&config, &root, &mut carry, &burst);
    assert!(
        outcome.is_err(),
        "the injected bulk failure must fail the pass"
    );

    // The process dies here: a restart starts with an empty cache and a full
    // burst, which is exactly what `watch::run` does on its first pass.
    let mut restarted = crate::watch::Carry::default();
    crate::watch::one_pass(
        &config,
        &root,
        &mut restarted,
        &crate::watch::ChangeSet::full(),
    )
    .0
    .unwrap();

    let live = published(&endpoint);
    let (reference, _reference_state) = fresh_full_index(&root);
    assert_eq!(
        live,
        published(&reference),
        "a crash mid-burst must not lose or duplicate documents"
    );
    let values: Vec<String> = endpoint
        .data_docs()
        .iter()
        .filter_map(|doc| doc.get("value").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    assert!(values.iter().any(|v| v == "beta-two"), "{values:?}");
    assert!(!values.iter().any(|v| v == "beta"), "{values:?}");
}

/// The property that justifies the feature: any sequence of creates, writes,
/// renames, deletes and recreates, applied one burst at a time, converges on the
/// index a fresh full run would produce.
#[test]
fn watch_converges_under_a_randomised_change_sequence() {
    let _guard = HTTP_E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Fixed seed: a property test that changes shape between runs cannot be
    // debugged. Bump it deliberately to explore more sequences.
    const SEED: u64 = 0x5745_5443_4831_3237;
    let mut rng = Rng(SEED);
    let corpus = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let root = corpus.path().canonicalize().unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    for i in 0..4 {
        fs::write(
            root.join(format!("f{i}.csv")),
            format!("id,value\n{i},start-{i}\n"),
        )
        .unwrap();
    }
    let endpoint = HttpEndpoint::start();
    let config = cfg(&root, state_dir.path(), &endpoint.url, false);
    let mut carry = crate::watch::Carry::default();
    let (outcome, _) =
        crate::watch::one_pass(&config, &root, &mut carry, &crate::watch::ChangeSet::full());
    assert!(matches!(outcome, Ok((0, _)) | Ok((3, _))), "seed {SEED:#x}");

    let mut serial = 0u64;
    for step in 0..14u64 {
        let mut touched: Vec<PathBuf> = Vec::new();
        let live: Vec<PathBuf> = walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf())
            .collect();
        match rng.below(6) {
            // create
            0 | 1 => {
                serial += 1;
                let dir = if rng.below(2) == 0 {
                    root.clone()
                } else {
                    root.join("sub")
                };
                let path = dir.join(format!("new{serial}.csv"));
                fs::write(&path, format!("id,value\n{serial},made-{serial}\n")).unwrap();
                touched.push(path);
            }
            // modify
            2 => {
                if let Some(path) = live.first() {
                    serial += 1;
                    fs::write(path, format!("id,value\n{serial},edited-{serial}\n")).unwrap();
                    touched.push(path.clone());
                }
            }
            // rename
            3 => {
                if let Some(path) = live.first() {
                    serial += 1;
                    let renamed = root.join(format!("moved{serial}.csv"));
                    fs::rename(path, &renamed).unwrap();
                    touched.push(path.clone());
                    touched.push(renamed);
                }
            }
            // delete
            4 => {
                if let Some(path) = live.first() {
                    fs::remove_file(path).unwrap();
                    touched.push(path.clone());
                }
            }
            // delete and recreate the same path with different content: the
            // shape that makes a path-keyed cache wrong.
            _ => {
                if let Some(path) = live.first() {
                    serial += 1;
                    fs::remove_file(path).unwrap();
                    fs::write(path, format!("id,value\n{serial},reborn-{serial}\n")).unwrap();
                    touched.push(path.clone());
                }
            }
        }
        if touched.is_empty() {
            continue;
        }
        let burst = crate::watch::ChangeSet::from_paths(&touched);
        let (outcome, _) = crate::watch::one_pass(&config, &root, &mut carry, &burst);
        assert!(
            matches!(outcome, Ok((0, _)) | Ok((3, _))),
            "seed {SEED:#x} step {step}: pass failed: {:?}",
            outcome.err().map(|e| format!("{e:#}"))
        );
    }

    // The property that always holds: a full-hash re-run after the sequence
    // changes nothing, so no carried digest ever hid a real change.
    a_rerun_changes_nothing(&config, &endpoint, &format!("seed {SEED:#x}"));
    let (reference, _reference_state) = fresh_full_index(&root);
    let watched = published(&endpoint);
    let fresh = published(&reference);
    assert_eq!(
        watched.keys().collect::<Vec<_>>(),
        fresh.keys().collect::<Vec<_>>(),
        "seed {SEED:#x}: document ids diverged"
    );
    assert_eq!(
        watched.len(),
        fresh.len(),
        "seed {SEED:#x}: record count diverged"
    );
    assert_eq!(watched, fresh, "seed {SEED:#x}: document contents diverged");
}
