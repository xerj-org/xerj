//! End-to-end throughput harness for the binary-protocol `_bulk` handler
//! (issue #903), driven over a real TCP connection through
//! [`xerj_api::binary_protocol::serve_binary_protocol`].
//!
//! `xerj-ai/examples/neural_throughput.rs` measures the encoder in isolation.
//! This one measures what a client actually gets: documents per second from
//! the moment the BULK frame is written to the moment its `RESP_OK` comes
//! back, with the whole ingest path (embedding, WAL, memtable, HNSW) in the
//! middle. It exists so the #903 before/after can be quoted from a run rather
//! than reasoned about from the call shape.
//!
//! It is only interesting with an ACTIVE embedding backend — the default
//! lexical feature-hashing embedder does no batched inference at all, so its
//! numbers say nothing about batching. Run it as:
//!
//! ```sh
//! cargo run --release -p xerj-api --features neural \
//!     --example binary_bulk_throughput -- <documents> <documents-per-bulk>
//! ```
//!
//! Environment:
//!
//! * `XERJ_BULK_CORPUS` — a text file, one document per line. Lines shorter
//!   than 40 or longer than 400 bytes are skipped so every document is exactly
//!   one passage (the semantic chunker cuts at 512 characters), which keeps
//!   documents/s and passages/s the same number. Without it the harness
//!   synthesizes short lines and says so.
//! * `XERJ_NEURAL_LOCAL_DIR` — load MiniLM weights from a local directory
//!   instead of the HuggingFace cache.
//! * `XERJ_BULK_MODE` — `neural` (default), `lexical`, or `proxy`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use xerj_api::binary_protocol::{op, serve_binary_protocol};
use xerj_api::state::AppState;
use xerj_common::types::{FieldConfig, FieldType, Schema};

/// all-MiniLM-L6-v2's hidden size, which is also the lexical fallback's width.
const DIMS: usize = 384;

fn corpus(count: usize) -> (Vec<String>, &'static str) {
    match std::env::var("XERJ_BULK_CORPUS") {
        Ok(path) => {
            let text = std::fs::read_to_string(&path).expect("read XERJ_BULK_CORPUS");
            let docs: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| (40..=400).contains(&line.len()))
                .map(str::to_owned)
                .take(count)
                .collect();
            assert!(
                docs.len() == count,
                "corpus {path} yielded {} usable lines, needed {count}",
                docs.len()
            );
            (docs, "real")
        }
        Err(_) => (
            (0..count)
                .map(|i| {
                    format!(
                        "synthetic record {i}: a short line of prose standing in for a log \
                         message or a chat turn"
                    )
                })
                .collect(),
            "synthetic",
        ),
    }
}

fn frame(stream: &mut TcpStream, op_code: u8, payload: &Value) -> Value {
    let body = serde_json::to_vec(payload).expect("serialize");
    let mut header = [0u8; 5];
    header[..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
    header[4] = op_code;
    stream.write_all(&header).expect("write header");
    stream.write_all(&body).expect("write payload");
    stream.flush().expect("flush");

    let mut resp_header = [0u8; 5];
    stream.read_exact(&mut resp_header).expect("read header");
    let length = u32::from_le_bytes([
        resp_header[0],
        resp_header[1],
        resp_header[2],
        resp_header[3],
    ]) as usize;
    let mut resp = vec![0u8; length];
    stream.read_exact(&mut resp).expect("read payload");
    assert_eq!(
        resp_header[4],
        op::RESP_OK,
        "{}",
        String::from_utf8_lossy(&resp)
    );
    serde_json::from_slice(&resp).expect("response json")
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let documents: usize = args
        .next()
        .map(|a| a.parse().expect("documents"))
        .unwrap_or(1000);
    let per_bulk: usize = args
        .next()
        .map(|a| a.parse().expect("documents-per-bulk"))
        .unwrap_or(64);
    let mode = std::env::var("XERJ_BULK_MODE").unwrap_or_else(|_| "neural".to_string());

    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = xerj_common::config::Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.storage.wal_sync = xerj_common::config::WalSync::Async;
    config.embedding.mode = mode.clone();
    if let Ok(local) = std::env::var("XERJ_NEURAL_LOCAL_DIR") {
        config.embedding.local_model_dir = local;
    }

    let metrics = xerj_common::metrics::Metrics::new().expect("metrics");
    let engine = xerj_engine::Engine::new(config.clone()).expect("engine");

    let mut schema = Schema::empty();
    let mut body = FieldConfig::new("body", FieldType::Text);
    body.options.dimensions = Some(DIMS);
    body.options.similarity = Some("cosine".into());
    body.embedding = Some(xerj_common::types::EmbeddingConfig {
        endpoint: None,
        model: None,
        target_field: Some("body_vector".into()),
    });
    schema.add_field(body).expect("semantic field");
    let mut companion = FieldConfig::new("body_vector", FieldType::Vector);
    companion.options.dimensions = Some(DIMS);
    companion.options.similarity = Some("cosine".into());
    schema.add_field(companion).expect("companion field");
    engine.create_index("bulkbench", schema).expect("create");

    let state = Arc::new(AppState::new(config, engine, metrics));

    // Take an ephemeral port from the kernel, release it, and hand the address
    // to the server — `serve_binary_protocol` binds for itself.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe addr");
    drop(probe);
    tokio::spawn(serve_binary_protocol(addr, Arc::clone(&state)));

    let mut stream = None;
    for _ in 0..200 {
        match TcpStream::connect(addr) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
    let mut stream = stream.expect("connect to binary protocol");
    stream.set_nodelay(true).expect("nodelay");

    // Warm-up: the first bulk pays model load and tokenizer init, which is a
    // one-off cost and not what this measures. Its documents are separate ids
    // so the timed run is all inserts, never updates.
    let (warm, _) = corpus(per_bulk);
    let warm_docs: Vec<Value> = warm
        .iter()
        .enumerate()
        .map(|(i, text)| json!({ "_id": format!("warm-{i}"), "body": text }))
        .collect();
    let warm_resp = frame(
        &mut stream,
        op::BULK,
        &json!({ "index": "bulkbench", "docs": warm_docs }),
    );
    assert_eq!(warm_resp["errors"], json!(false), "warm-up: {warm_resp}");

    let (texts, provenance) = corpus(documents);
    let bytes: usize = texts.iter().map(String::len).sum();
    let longest = texts.iter().map(String::len).max().unwrap_or(0);
    let batches: Vec<Vec<Value>> = texts
        .chunks(per_bulk)
        .enumerate()
        .map(|(b, chunk)| {
            chunk
                .iter()
                .enumerate()
                .map(|(i, text)| json!({ "_id": format!("d{b}-{i}"), "body": text }))
                .collect()
        })
        .collect();

    let started = Instant::now();
    let mut indexed = 0i64;
    for batch in &batches {
        let resp = frame(
            &mut stream,
            op::BULK,
            &json!({ "index": "bulkbench", "docs": batch }),
        );
        assert_eq!(resp["errors"], json!(false), "bulk: {resp}");
        indexed += resp["indexed"].as_i64().unwrap_or(0);
    }
    let elapsed = started.elapsed();

    assert_eq!(indexed, documents as i64, "every document must be indexed");
    println!(
        "mode={mode} corpus={provenance} documents={documents} per_bulk={per_bulk} \
         bulks={} mean_doc_bytes={} longest_doc_bytes={longest} elapsed_s={:.3} docs_per_s={:.2}",
        batches.len(),
        bytes / documents,
        elapsed.as_secs_f64(),
        documents as f64 / elapsed.as_secs_f64(),
    );
}
