//! `xerj index` must wait out the parent memory breaker instead of dropping
//! batches, and must exit non-zero whenever any document was not indexed.
//!
//! Before the fix the batch loop retried only `ResourceExhausted`. A
//! `CircuitBreaking` rejection logged `batch ingest error`, counted the batch
//! in `errors`, and dropped it, and the command still exited 0. Under
//! `max_process_memory_mb = 2048` that lost 144,158 of 284,158 documents.
//!
//! The breaker is injected (`XERJ_INDEX_TEST_BREAKER_REJECTIONS`) because the
//! real one trips on RSS, and the RSS a capped run reaches differs from host
//! to host; a low `max_process_memory_mb` either never trips or never admits.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

const WORDS: &[&str] = &[
    "river", "engine", "segment", "postings", "harbor", "lantern", "granite", "meadow", "quartz",
    "violet", "compass", "falcon", "thimble", "orchard", "saffron", "tundra", "ember", "glacier",
    "juniper", "kestrel", "marble", "nectar", "obsidian", "pebble", "quiver", "russet", "sierra",
    "timber", "umber", "willow", "zephyr", "anchor", "bramble", "cinder", "dahlia", "estuary",
];

/// `n` NDJSON lines of about 3 KB of prose each, deterministic.
fn write_corpus(path: &Path, n: usize) {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).expect("create corpus"));
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut body = String::with_capacity(3200);
    for i in 0..n {
        body.clear();
        while body.len() < 3000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let w = WORDS[(state >> 33) as usize % WORDS.len()];
            body.push_str(w);
            body.push_str(&((state >> 20) % 997).to_string());
            body.push(' ');
        }
        writeln!(
            out,
            "{{\"n\":{i},\"title\":\"doc {i}\",\"body\":\"{body}\"}}"
        )
        .expect("write line");
    }
    out.flush().expect("flush corpus");
}

fn run_index(
    file: &Path,
    data_dir: &Path,
    config: &Path,
    extra: &[&str],
    injected_rejections: u64,
) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["index", "--index", "docs", "--file"])
        .arg(file)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--config")
        .arg(config)
        .args(extra)
        .arg("--disable-feedback")
        .env("XERJ_DISABLE_FEEDBACK", "true")
        .env_remove("XERJ_MAX_PROCESS_MEMORY_MB")
        .env("RUST_LOG", "warn")
        .env(
            "XERJ_INDEX_TEST_BREAKER_REJECTIONS",
            injected_rejections.to_string(),
        )
        .output()
        .expect("run the xerj binary")
}

fn summary_value(stdout: &str, key: &str) -> Option<u64> {
    stdout
        .lines()
        .find(|l| l.trim_start().starts_with(key))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

fn live_docs(data_dir: &Path) -> u64 {
    let mut cfg = xerj_common::config::Config::default();
    cfg.server.data_dir = data_dir.to_str().expect("utf-8 data dir").to_string();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();
    let engine = xerj_engine::Engine::new(cfg).expect("reopen the data dir");
    let count = engine
        .get_index("docs")
        .expect("index exists")
        .live_doc_count();
    drop(engine);
    count
}

/// Every batch is first rejected by the parent breaker, several times over
/// (`XERJ_INDEX_TEST_BREAKER_REJECTIONS` injects the engine's
/// `CircuitBreaking` error ahead of the engine call, so the run does not
/// depend on this host's RSS). The loader must wait each one out and land
/// every document.
#[test]
fn breaker_rejections_are_retried_and_every_document_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = dir.path().join("docs.ndjson");
    let data_dir = dir.path().join("data");
    let config = dir.path().join("xerj.toml");
    const N: usize = 50_000;
    const BATCH: usize = 1_000;
    write_corpus(&corpus, N);
    std::fs::write(&config, "").expect("write config");

    let rejections = 3 * (N / BATCH) as u64;
    let out = run_index(
        &corpus,
        &data_dir,
        &config,
        &["--workers", "8", "--batch", "1000"],
        rejections,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let log = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(out.status.code(), Some(0), "{log}");
    assert_eq!(summary_value(&stdout, "errors"), Some(0), "{log}");
    assert_eq!(summary_value(&stdout, "docs sent"), Some(N as u64), "{log}");
    assert!(
        !log.contains("batch ingest error"),
        "a batch was dropped:\n{log}"
    );
    let warns = log
        .matches("memory breaker tripped, waiting for flush")
        .count();
    assert!(warns >= 1, "the breaker WARN never printed:\n{log}");
    assert!(
        (warns as u64) < rejections,
        "{warns} breaker WARNs for {rejections} rejections: the WARN is not rate-limited"
    );
    assert_eq!(live_docs(&data_dir), N as u64);
}

/// A batch the breaker never admits within `--retry-timeout-secs` counts as
/// failed and the command exits 1; it is never dropped silently.
#[test]
fn a_batch_that_outlasts_the_retry_budget_fails_the_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = dir.path().join("docs.ndjson");
    let data_dir = dir.path().join("data");
    let config = dir.path().join("xerj.toml");
    write_corpus(&corpus, 10);
    std::fs::write(&config, "").expect("write config");

    let out = run_index(
        &corpus,
        &data_dir,
        &config,
        &["--workers", "1", "--retry-timeout-secs", "1"],
        u64::MAX,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let log = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_eq!(out.status.code(), Some(1), "{log}");
    assert_eq!(summary_value(&stdout, "errors"), Some(10), "{log}");
    assert_eq!(summary_value(&stdout, "docs sent"), Some(0), "{log}");
    assert!(stderr.contains("10 documents were NOT indexed"), "{log}");
}

#[test]
fn a_document_that_cannot_be_indexed_makes_the_command_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = dir.path().join("docs.ndjson");
    let data_dir = dir.path().join("data");
    let config = dir.path().join("xerj.toml");
    std::fs::write(
        &corpus,
        "{\"n\":1,\"body\":\"one\"}\n{\"n\":2,\"body\":\n{\"n\":3,\"body\":\"three\"}\n",
    )
    .expect("write corpus");
    std::fs::write(&config, "").expect("write config");

    let out = run_index(&corpus, &data_dir, &config, &["--workers", "1"], 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a failed batch must not exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let errors = summary_value(&stdout, "errors").expect("summary prints errors");
    assert!(errors > 0, "{stdout}");
    assert!(
        stderr.contains(&format!("{errors} documents were NOT indexed")),
        "{stderr}"
    );
}
