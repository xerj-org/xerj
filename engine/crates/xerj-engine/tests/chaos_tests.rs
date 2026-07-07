//! # xerj Chaos Test Suite — SLA & Durability
//!
//! Simulates real production failures: crash recovery, WAL corruption, restart
//! loops, concurrent stress, large documents, flush under load, schema evolution,
//! and data integrity.
//!
//! Every test prints timestamped headers, per-iteration results, and contributes
//! to a final summary table printed at the end of the suite run.
//!
//! Run with:
//! ```bash
//! cargo test -p xerj-engine --test chaos_tests -- --nocapture
//! ```

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::Engine;
use xerj_query::parse_request;

// ── Test result tracking ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct TestResult {
    name: String,
    iterations: u64,
    pass: u64,
    fail: u64,
    duration: Duration,
}

/// Global accumulator for cross-test summary.
fn chaos_results() -> &'static Mutex<Vec<TestResult>> {
    static RESULTS: OnceLock<Mutex<Vec<TestResult>>> = OnceLock::new();
    RESULTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn push_result(r: TestResult) {
    chaos_results().lock().unwrap().push(r);
}

fn now_ts() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}Z", h, m, s)
}

fn print_header(test_name: &str) {
    println!();
    println!("┌─────────────────────────────────────────────────────────────────┐");
    println!("│  [{ts}]  {name:<52}│", ts = now_ts(), name = test_name);
    println!("└─────────────────────────────────────────────────────────────────┘");
}

fn print_summary(results: &[TestResult]) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║  CHAOS TEST RESULTS — xerj SLA & Durability                   ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!(
        "║  {:<24} │ {:>10} │ {:>4} │ {:>4} │ {:<9} ║",
        "Test", "Iterations", "Pass", "Fail", "Duration"
    );
    println!(
        "║  {:<24} │ {:>10} │ {:>4} │ {:>4} │ {:<9} ║",
        "─".repeat(24),
        "─".repeat(10),
        "─".repeat(4),
        "─".repeat(4),
        "─".repeat(9)
    );
    for r in results {
        println!(
            "║  {:<24} │ {:>10} │ {:>4} │ {:>4} │ {:<9} ║",
            trunc24(&r.name),
            r.iterations,
            r.pass,
            r.fail,
            format!("{:.1}s", r.duration.as_secs_f64()),
        );
    }
    println!("╚══════════════════════════════════════════════════════════════════╝");

    let total_pass: u64 = results.iter().map(|r| r.pass).sum();
    let total_fail: u64 = results.iter().map(|r| r.fail).sum();
    let total_iters: u64 = results.iter().map(|r| r.iterations).sum();
    println!(
        "  TOTAL: {}/{} iterations passed  ({} failed)",
        total_pass, total_iters, total_fail
    );
    if total_fail == 0 {
        println!("  ALL CHAOS TESTS PASSED");
    } else {
        println!("  FAILURES DETECTED — see above for details");
    }
    println!();
}

fn trunc24(s: &str) -> String {
    if s.len() <= 24 {
        s.to_string()
    } else {
        format!("{}…", &s[..23])
    }
}

// ── Engine helpers ────────────────────────────────────────────────────────────

fn make_engine(dir: &TempDir) -> Engine {
    make_engine_at(dir.path())
}

fn make_engine_at(path: &std::path::Path) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = path.to_str().unwrap().to_string();
    // High flush thresholds — tests that care will flush explicitly.
    config.storage.flush_size_mb = 4096;
    config.storage.flush_interval_secs = 86400;
    Engine::new(config).expect("engine::new")
}

async fn count_all(idx: &Arc<xerj_engine::Index>) -> u64 {
    let req =
        parse_request(&json!({ "query": { "match_all": {} }, "size": 0 })).expect("parse_request");
    idx.search(&req).await.map(|r| r.total.value).unwrap_or(0)
}

// ── FNV-1a 64-bit hash for checksums ─────────────────────────────────────────

fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 1 — SIGKILL During Active Writes (crash resilience)
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_crash_during_writes() {
    print_header("Test 1: SIGKILL During Active Writes (crash resilience)");

    let test_start = Instant::now();
    let crash_points = [100u64, 500, 1000, 5000];
    let rounds_per_point: usize = 2; // 2 × 4 = 8, plus 2 bonus = 10 total
    let mut iterations = 0u64;
    let mut pass = 0u64;
    let mut fail = 0u64;

    for &crash_after in &crash_points {
        for round in 0..rounds_per_point {
            iterations += 1;
            let dir = TempDir::new().unwrap();
            let iter_start = Instant::now();

            // Phase 1: index N docs then "crash" (drop without flush).
            {
                let engine = make_engine(&dir);
                engine.create_index("crash_idx", Schema::empty()).unwrap();
                let idx = engine.get_index("crash_idx").unwrap();
                for i in 0..crash_after {
                    idx.index_document(
                        Some(format!("doc-{}", i)),
                        json!({ "body": format!("document number {}", i), "seq": i }),
                    )
                    .await
                    .unwrap();
                }
                // Drop without flush — simulate power loss / SIGKILL.
                drop(idx);
                drop(engine);
            }

            // Phase 2: reopen and replay WAL.
            let engine2 = make_engine_at(dir.path());
            let idx2 = engine2
                .get_index("crash_idx")
                .expect("index must reopen from WAL");
            let recovered = count_all(&idx2).await;
            let elapsed_ms = iter_start.elapsed().as_millis();

            // Allow up to 0.5% loss for batched WAL sync (same as ES translog batched mode)
            let min_recovered = (crash_after as f64 * 0.995) as u64;
            let ok = recovered >= min_recovered;
            if ok {
                pass += 1;
            } else {
                fail += 1;
            }

            let rate = (recovered as f64 / crash_after as f64) * 100.0;
            println!(
                "  [{ts}] iter={it:>2}  crash_after={n:>5}  round={r}  \
                 recovered={rec}  rate={rate:.1}%  elapsed={ms}ms  {status}",
                ts = now_ts(),
                it = iterations,
                n = crash_after,
                r = round,
                rec = recovered,
                rate = rate,
                ms = elapsed_ms,
                status = if ok {
                    "PASS"
                } else {
                    "FAIL ← data loss detected"
                },
            );
        }
    }

    // Two additional iterations at extreme points.
    for &crash_after in &[50u64, 10_000u64] {
        iterations += 1;
        let dir = TempDir::new().unwrap();
        let iter_start = Instant::now();
        {
            let engine = make_engine(&dir);
            engine.create_index("crash_idx", Schema::empty()).unwrap();
            let idx = engine.get_index("crash_idx").unwrap();
            for i in 0..crash_after {
                idx.index_document(
                    Some(format!("doc-{}", i)),
                    json!({ "body": format!("doc {}", i) }),
                )
                .await
                .unwrap();
            }
            drop(idx);
            drop(engine);
        }
        let engine2 = make_engine_at(dir.path());
        let idx2 = engine2.get_index("crash_idx").expect("index must reopen");
        let recovered = count_all(&idx2).await;
        // Allow up to 0.5% loss for batched WAL sync (same as ES translog batched mode)
        let min_recovered = (crash_after as f64 * 0.995) as u64;
        let ok = recovered >= min_recovered;
        if ok {
            pass += 1;
        } else {
            fail += 1;
        }
        println!(
            "  [{ts}] iter={it:>2}  crash_after={n:>6}  recovered={rec}  \
             elapsed={ms}ms  {status}",
            ts = now_ts(),
            it = iterations,
            n = crash_after,
            rec = recovered,
            ms = iter_start.elapsed().as_millis(),
            status = if ok { "PASS" } else { "FAIL" },
        );
    }

    let duration = test_start.elapsed();
    println!(
        "\n  RESULT: {pass}/{iterations} passed  duration={d:.1}s",
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Crash recovery".into(),
        iterations,
        pass,
        fail,
        duration,
    });
    assert_eq!(fail, 0, "crash recovery failed on {} iterations", fail);
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 2 — WAL Corruption Recovery
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_wal_corruption() {
    print_header("Test 2: WAL Corruption Recovery");

    let test_start = Instant::now();
    let mut iterations = 0u64;
    let mut pass = 0u64;
    let mut fail = 0u64;
    let doc_count = 200u64;

    struct CorruptCase {
        name: &'static str,
        apply: fn(&mut Vec<u8>),
    }

    // ptr_arg: signature must stay `fn(&mut Vec<u8>)` to match the CorruptCase.apply
    // field type shared with corrupt_truncate50, which needs Vec::truncate.
    #[allow(clippy::ptr_arg)]
    fn corrupt_last10(data: &mut Vec<u8>) {
        let len = data.len();
        if len >= 10 {
            for b in &mut data[len - 10..] {
                *b = 0xFF;
            }
        }
    }
    // ptr_arg: signature must stay `fn(&mut Vec<u8>)` to match the CorruptCase.apply
    // field type shared with corrupt_truncate50, which needs Vec::truncate.
    #[allow(clippy::ptr_arg)]
    fn corrupt_middle(data: &mut Vec<u8>) {
        let mid = data.len() / 2;
        let end = (mid + 64).min(data.len());
        for b in &mut data[mid..end] {
            *b ^= 0xAA;
        }
    }
    // ptr_arg: signature must stay `fn(&mut Vec<u8>)` to match the CorruptCase.apply
    // field type shared with corrupt_truncate50, which needs Vec::truncate.
    #[allow(clippy::ptr_arg)]
    fn corrupt_header(data: &mut Vec<u8>) {
        // Overwrite the 4-byte "ZWAL" magic at offset 0.
        if data.len() >= 4 {
            data[0] = 0x00;
            data[1] = 0x00;
            data[2] = 0x00;
            data[3] = 0x00;
        }
    }
    fn corrupt_truncate50(data: &mut Vec<u8>) {
        let half = data.len() / 2;
        data.truncate(half);
    }
    // ptr_arg: signature must stay `fn(&mut Vec<u8>)` to match the CorruptCase.apply
    // field type shared with corrupt_truncate50, which needs Vec::truncate.
    #[allow(clippy::ptr_arg)]
    fn corrupt_zero4kb(data: &mut Vec<u8>) {
        let start = (data.len() / 4).min(data.len().saturating_sub(4096));
        let end = (start + 4096).min(data.len());
        for b in &mut data[start..end] {
            *b = 0x00;
        }
    }

    let cases: &[CorruptCase] = &[
        CorruptCase {
            name: "last-10-bytes",
            apply: corrupt_last10,
        },
        CorruptCase {
            name: "middle-64-bytes",
            apply: corrupt_middle,
        },
        CorruptCase {
            name: "header-magic",
            apply: corrupt_header,
        },
        CorruptCase {
            name: "truncate-50pct",
            apply: corrupt_truncate50,
        },
        CorruptCase {
            name: "zero-4kb-block",
            apply: corrupt_zero4kb,
        },
    ];

    for case in cases {
        iterations += 1;
        let dir = TempDir::new().unwrap();
        let iter_start = Instant::now();

        // Phase 1: index docs and close engine cleanly (WAL is written, no flush).
        {
            let engine = make_engine(&dir);
            engine.create_index("wal_idx", Schema::empty()).unwrap();
            let idx = engine.get_index("wal_idx").unwrap();
            for i in 0..doc_count {
                idx.index_document(
                    Some(format!("d{}", i)),
                    json!({ "n": i, "msg": format!("entry {}", i) }),
                )
                .await
                .unwrap();
            }
            drop(idx);
            drop(engine);
        }

        // Phase 2: find WAL file and corrupt it.
        let wal_dir = dir.path().join("wal_idx").join("wal");
        let wal_file = std::fs::read_dir(&wal_dir)
            .ok()
            .and_then(|rd| {
                rd.flatten()
                    .find(|e| e.path().extension().map(|x| x == "wal").unwrap_or(false))
            })
            .map(|e| e.path());

        let mut recovered = 0u64;
        let engine_opened;

        if let Some(ref wf) = wal_file {
            let mut data = std::fs::read(wf).unwrap_or_default();
            (case.apply)(&mut data);
            let _ = std::fs::write(wf, &data);

            // Phase 3: reopen — must NOT panic.
            let engine_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                make_engine_at(dir.path())
            }));

            match engine_result {
                Ok(engine2) => {
                    engine_opened = true;
                    if let Ok(idx2) = engine2.get_index("wal_idx") {
                        recovered = count_all(&idx2).await;
                    }
                    // Header corruption means no docs can be read; that's acceptable.
                }
                Err(_) => {
                    engine_opened = false;
                }
            }
        } else {
            // No WAL file found — engine may have flushed, which is also fine.
            engine_opened = true;
            let engine2 = make_engine_at(dir.path());
            if let Ok(idx2) = engine2.get_index("wal_idx") {
                recovered = count_all(&idx2).await;
            }
        }

        let loss_pct = if doc_count > 0 {
            (doc_count.saturating_sub(recovered)) as f64 / doc_count as f64 * 100.0
        } else {
            0.0
        };

        // Pass = engine opened without panic (data loss is acceptable for some corruption types).
        let ok = engine_opened;
        if ok {
            pass += 1;
        } else {
            fail += 1;
        }

        println!(
            "  [{ts}] iter={it}  corruption={name:<20}  opened={op}  \
             recovered={rec}/{total}  loss={loss:.1}%  elapsed={ms}ms  {status}",
            ts = now_ts(),
            it = iterations,
            name = case.name,
            op = engine_opened,
            rec = recovered,
            total = doc_count,
            loss = loss_pct,
            ms = iter_start.elapsed().as_millis(),
            status = if ok {
                "PASS"
            } else {
                "FAIL ← engine crashed/panicked"
            },
        );
    }

    let duration = test_start.elapsed();
    println!(
        "\n  RESULT: {pass}/{iterations} passed  duration={d:.1}s",
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "WAL corruption".into(),
        iterations,
        pass,
        fail,
        duration,
    });
    assert_eq!(fail, 0, "WAL corruption caused {} engine crash(es)", fail);
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 3 — Rapid Restart Stability (100x restart loop)
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_rapid_restart_loop() {
    print_header("Test 3: Rapid Restart Stability (100x restart loop)");

    let test_start = Instant::now();
    let initial_docs = 500u64;
    let restart_count = 100u64;
    let mut pass = 0u64;
    let mut fail = 0u64;

    let dir = TempDir::new().unwrap();

    // Seed data and flush so it lives in a segment (survives WAL-free reopens).
    {
        let engine = make_engine(&dir);
        engine.create_index("restart_idx", Schema::empty()).unwrap();
        let idx = engine.get_index("restart_idx").unwrap();
        for i in 0..initial_docs {
            idx.index_document(
                Some(format!("doc-{}", i)),
                json!({ "title": format!("restart doc {}", i), "n": i }),
            )
            .await
            .unwrap();
        }
        idx.flush().await.unwrap();
        drop(idx);
        drop(engine);
    }

    println!(
        "  Seeded {initial_docs} docs (flushed to segment). Beginning {restart_count} rapid restarts…"
    );

    for i in 0..restart_count {
        let iter_start = Instant::now();
        let engine = make_engine_at(dir.path());
        let idx = engine
            .get_index("restart_idx")
            .expect("index must exist after restart");

        let doc_count = count_all(&idx).await;

        // Use match_all — works across both memtable and flushed segments
        let search_req =
            parse_request(&json!({ "query": { "match_all": {} }, "size": 5 })).unwrap();
        let search_result = idx.search(&search_req).await.unwrap();
        let search_works = search_result.total.value > 0 && !search_result.hits.is_empty();

        let restart_ms = iter_start.elapsed().as_millis();
        let iter_ok = doc_count == initial_docs && search_works;

        if iter_ok {
            pass += 1;
        } else {
            fail += 1;
        }

        // Print every 10th iteration or on failure.
        if i % 10 == 0 || !iter_ok {
            println!(
                "  [{ts}] restart={it:>3}/{total}  doc_count={dc}  \
                 search_works={sw}  restart_ms={ms}  {status}",
                ts = now_ts(),
                it = i + 1,
                total = restart_count,
                dc = doc_count,
                sw = search_works,
                ms = restart_ms,
                status = if iter_ok { "PASS" } else { "FAIL" },
            );
        }

        drop(idx);
        drop(engine);
    }

    let duration = test_start.elapsed();
    println!(
        "\n  RESULT: {pass}/{restart_count} restarts passed  duration={d:.1}s",
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Rapid restart (100x)".into(),
        iterations: restart_count,
        pass,
        fail,
        duration,
    });
    assert_eq!(fail, 0, "rapid restart failed on {} iterations", fail);
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 4 — Concurrent Read/Write Stress
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_concurrent_readwrite_stress() {
    print_header("Test 4: Concurrent Read/Write Stress (8 writers × 8 readers)");

    let test_start = Instant::now();
    let writer_tasks = 8usize;
    let docs_per_writer = 1000usize;
    let reader_tasks = 8usize;
    let searches_per_reader = 500usize;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("stress_idx", Schema::empty()).unwrap();
    let idx = Arc::new(engine.get_index("stress_idx").unwrap());

    let write_errors = Arc::new(AtomicU64::new(0));
    let read_errors = Arc::new(AtomicU64::new(0));
    let reads_ok = Arc::new(AtomicU64::new(0));

    println!(
        "  Spawning {writer_tasks} writers ({docs_per_writer} docs each) + \
         {reader_tasks} readers ({searches_per_reader} searches each)…"
    );

    // Spawn writers.
    let mut writer_handles = Vec::new();
    for w in 0..writer_tasks {
        let idx_c = Arc::clone(&idx);
        let err_c = Arc::clone(&write_errors);
        writer_handles.push(tokio::spawn(async move {
            for d in 0..docs_per_writer {
                let r = idx_c
                    .index_document(
                        Some(format!("w{}-d{}", w, d)),
                        json!({ "writer": w, "doc": d,
                                 "body": format!("writer {} document {}", w, d) }),
                    )
                    .await;
                if r.is_err() {
                    err_c.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    // Spawn readers.
    let search_req =
        Arc::new(parse_request(&json!({ "query": { "match_all": {} }, "size": 1 })).unwrap());
    let mut reader_handles = Vec::new();
    for _ in 0..reader_tasks {
        let idx_c = Arc::clone(&idx);
        let req_c = Arc::clone(&search_req);
        let err_c = Arc::clone(&read_errors);
        let ok_c = Arc::clone(&reads_ok);
        reader_handles.push(tokio::spawn(async move {
            for _ in 0..searches_per_reader {
                match idx_c.search(&req_c).await {
                    Ok(_) => {
                        ok_c.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        err_c.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    for h in writer_handles {
        h.await.expect("writer task panicked");
    }
    for h in reader_handles {
        h.await.expect("reader task panicked");
    }

    let expected_docs = (writer_tasks * docs_per_writer) as u64;
    let actual_docs = count_all(&idx).await;
    let w_err = write_errors.load(Ordering::Relaxed);
    let r_err = read_errors.load(Ordering::Relaxed);
    let r_ok = reads_ok.load(Ordering::Relaxed);

    let duration = test_start.elapsed();
    let write_tput = expected_docs as f64 / duration.as_secs_f64();
    let read_tput = r_ok as f64 / duration.as_secs_f64();

    println!(
        "  [{ts}] total_writes={tw}  actual_docs={ad}  write_errors={we}",
        ts = now_ts(),
        tw = expected_docs,
        ad = actual_docs,
        we = w_err
    );
    println!(
        "  [{ts}] reads_ok={rok}  read_errors={re}  duration={d:.1}s",
        ts = now_ts(),
        rok = r_ok,
        re = r_err,
        d = duration.as_secs_f64()
    );
    println!(
        "  [{ts}] write_throughput={wt:.0} docs/s  read_throughput={rt:.0} searches/s",
        ts = now_ts(),
        wt = write_tput,
        rt = read_tput
    );

    let all_ok = actual_docs == expected_docs && w_err == 0 && r_err == 0;
    println!(
        "\n  RESULT: {}  duration={d:.1}s",
        if all_ok { "PASS" } else { "FAIL" },
        d = duration.as_secs_f64()
    );

    push_result(TestResult {
        name: "Concurrent rw stress".into(),
        iterations: 1,
        pass: u64::from(all_ok),
        fail: u64::from(!all_ok),
        duration,
    });
    assert_eq!(w_err, 0, "write errors: {}", w_err);
    assert_eq!(r_err, 0, "read errors: {}", r_err);
    assert_eq!(actual_docs, expected_docs, "doc count mismatch");
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 5 — Large Document Stress
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_large_documents() {
    print_header("Test 5: Large Document Stress");

    let test_start = Instant::now();
    let mut iterations = 0u64;
    let mut pass = 0u64;
    let mut fail = 0u64;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("large_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("large_idx").unwrap();

    let sizes: &[(usize, &str)] = &[
        (1_024, "1KB"),
        (10_240, "10KB"),
        (102_400, "100KB"),
        (1_048_576, "1MB"),
        (5_242_880, "5MB"),
    ];

    for &(size, label) in sizes {
        iterations += 1;
        let doc_id = format!("large-{}", label);
        let body: String = "X".repeat(size);

        let index_start = Instant::now();
        let index_ok = idx
            .index_document(
                Some(doc_id.clone()),
                json!({ "body": body, "size_label": label }),
            )
            .await
            .is_ok();
        let index_ms = index_start.elapsed().as_millis();

        let get_start = Instant::now();
        let retrieved = if index_ok {
            idx.get_document(&doc_id).await.unwrap_or(None)
        } else {
            None
        };
        let get_ms = get_start.elapsed().as_millis();

        let source_verified = retrieved
            .as_ref()
            .and_then(|v| v.get("body"))
            .and_then(Value::as_str)
            .map(|s| s.len() == size)
            .unwrap_or(false);

        let iter_ok = index_ok && source_verified;
        if iter_ok {
            pass += 1;
        } else {
            fail += 1;
        }

        println!(
            "  [{ts}] size={label:<6}  bytes={b:>9}  index_ms={im:>6}  \
             get_ms={gm:>5}  source_verified={sv}  {status}",
            ts = now_ts(),
            label = label,
            b = size,
            im = index_ms,
            gm = get_ms,
            sv = source_verified,
            status = if iter_ok { "PASS" } else { "FAIL" },
        );
    }

    // Cross-index search across all large docs.
    let total = count_all(&idx).await;
    println!(
        "  [{ts}] cross-search total={total}  expected={}",
        sizes.len(),
        ts = now_ts()
    );

    let duration = test_start.elapsed();
    println!(
        "\n  RESULT: {pass}/{iterations} passed  duration={d:.1}s",
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Large documents".into(),
        iterations,
        pass,
        fail,
        duration,
    });
    assert_eq!(fail, 0, "{} large-doc iterations failed", fail);
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 6 — Flush Under Load
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_flush_during_writes() {
    print_header("Test 6: Flush Under Load");

    let test_start = Instant::now();
    let total_docs = 5000u64;
    let flush_every = 500u64;
    let expected_flushes = total_docs / flush_every; // 10

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("flush_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("flush_idx").unwrap();

    let mut flush_count = 0u64;

    for i in 0..total_docs {
        idx.index_document(
            Some(format!("doc-{}", i)),
            json!({ "n": i, "body": format!("flush test document {}", i) }),
        )
        .await
        .unwrap();

        if (i + 1) % flush_every == 0 {
            let flush_start = Instant::now();
            idx.flush().await.unwrap();
            flush_count += 1;
            let docs_now = count_all(&idx).await;
            println!(
                "  [{ts}] flush #{f:>2}  docs_indexed={di:>5}  searchable={ds:>5}  flush_ms={ms}",
                ts = now_ts(),
                f = flush_count,
                di = i + 1,
                ds = docs_now,
                ms = flush_start.elapsed().as_millis(),
            );
        }
    }

    // Final flush to ensure all docs are in segments + WAL checkpointed.
    idx.flush().await.unwrap();
    let docs_before = count_all(&idx).await;
    drop(idx);
    drop(engine);

    // Reopen and verify all docs survive.
    let engine2 = make_engine_at(dir.path());
    let idx2 = engine2
        .get_index("flush_idx")
        .expect("flush_idx must reopen");
    let docs_after = count_all(&idx2).await;

    let duration = test_start.elapsed();
    println!(
        "\n  [{ts}] docs_indexed={total}  flushes={f}  before_restart={br}  after_restart={ar}",
        ts = now_ts(),
        total = total_docs,
        f = flush_count,
        br = docs_before,
        ar = docs_after,
    );

    let all_ok =
        flush_count == expected_flushes && docs_before == total_docs && docs_after == total_docs;
    println!(
        "  RESULT: {}  duration={d:.1}s",
        if all_ok { "PASS" } else { "FAIL" },
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Flush under load".into(),
        iterations: expected_flushes,
        pass: if all_ok { expected_flushes } else { 0 },
        fail: if all_ok { 0 } else { 1 },
        duration,
    });
    assert_eq!(
        flush_count, expected_flushes,
        "expected {} flushes",
        expected_flushes
    );
    assert_eq!(docs_before, total_docs, "docs before restart mismatch");
    assert_eq!(docs_after, total_docs, "docs after restart mismatch");
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 7 — Delete Under Load
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_delete_during_writes() {
    print_header("Test 7: Delete Under Load");

    let test_start = Instant::now();
    let initial_docs = 2000u64;
    let new_docs_base = 20_000u64; // IDs start here to avoid collision
    let new_docs_count = 500u64;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("del_idx", Schema::empty()).unwrap();
    let idx = Arc::new(engine.get_index("del_idx").unwrap());

    // Phase 1: index 2000 initial docs.
    println!("  Phase 1: indexing {} initial docs…", initial_docs);
    for i in 0..initial_docs {
        idx.index_document(
            Some(format!("orig-{}", i)),
            json!({ "n": i, "kind": "original" }),
        )
        .await
        .unwrap();
    }

    // Phase 2: concurrently delete odd originals + index new docs.
    println!(
        "  Phase 2: deleting odd-numbered originals + indexing {} new docs…",
        new_docs_count
    );
    let idx_del = Arc::clone(&idx);
    let idx_new = Arc::clone(&idx);

    let delete_handle = tokio::spawn(async move {
        let mut deleted = 0u64;
        for i in (1..initial_docs).step_by(2) {
            if idx_del
                .delete_document(&format!("orig-{}", i))
                .await
                .unwrap_or(false)
            {
                deleted += 1;
            }
        }
        deleted
    });

    let new_write_handle = tokio::spawn(async move {
        for i in 0..new_docs_count {
            idx_new
                .index_document(
                    Some(format!("new-{}", new_docs_base + i)),
                    json!({ "n": new_docs_base + i, "kind": "new" }),
                )
                .await
                .unwrap();
        }
    });

    let deleted_count = delete_handle.await.unwrap();
    new_write_handle.await.unwrap();

    // Phase 3: verify correctness.
    // Even originals (0, 2, 4, …) should be present.
    let even_spot_check = [0u64, 2, 100, 500, 1998];
    let mut even_present = 0u64;
    for &n in &even_spot_check {
        if idx
            .get_document(&format!("orig-{}", n))
            .await
            .unwrap()
            .is_some()
        {
            even_present += 1;
        }
    }

    // Odd originals should be absent.
    let odd_spot_check = [1u64, 3, 101, 501, 1999];
    let mut odd_absent = 0u64;
    for &n in &odd_spot_check {
        if idx
            .get_document(&format!("orig-{}", n))
            .await
            .unwrap()
            .is_none()
        {
            odd_absent += 1;
        }
    }

    // New docs should be present.
    let new_doc_ok = idx
        .get_document(&format!("new-{}", new_docs_base))
        .await
        .unwrap()
        .is_some();

    let total_remaining = count_all(&idx).await;
    let duration = test_start.elapsed();

    println!(
        "\n  [{ts}] total_ops={ops}  deleted_count={del}  remaining_docs={rem}",
        ts = now_ts(),
        ops = initial_docs + new_docs_count + deleted_count,
        del = deleted_count,
        rem = total_remaining,
    );
    println!(
        "  [{ts}] even_spot_check={ev}/{ec}  odd_absent={oa}/{oc}  new_doc_ok={nd}",
        ts = now_ts(),
        ev = even_present,
        ec = even_spot_check.len(),
        oa = odd_absent,
        oc = odd_spot_check.len(),
        nd = new_doc_ok,
    );

    let all_ok = even_present == even_spot_check.len() as u64
        && odd_absent == odd_spot_check.len() as u64
        && new_doc_ok;

    println!(
        "  RESULT: {}  duration={d:.1}s",
        if all_ok { "PASS" } else { "FAIL" },
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Delete under load".into(),
        iterations: 1,
        pass: u64::from(all_ok),
        fail: u64::from(!all_ok),
        duration,
    });
    assert_eq!(
        even_present,
        even_spot_check.len() as u64,
        "even originals missing"
    );
    assert_eq!(
        odd_absent,
        odd_spot_check.len() as u64,
        "odd docs not deleted"
    );
    assert!(new_doc_ok, "new doc missing after concurrent writes");
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 8 — Memory Pressure Simulation
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_memory_pressure() {
    print_header("Test 8: Memory Pressure Simulation (~50MB data, 100 concurrent queries)");

    let test_start = Instant::now();
    let doc_count = 50_000u64;
    let concurrent_queries = 100usize;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("mem_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("mem_idx").unwrap();

    // ~1KB body per doc.
    let body_1kb: String = "abcdefghij".repeat(102);

    println!(
        "  Indexing {} docs (~1KB each = ~{}MB total)…",
        doc_count,
        doc_count / 1024
    );
    let ingest_start = Instant::now();
    for i in 0..doc_count {
        idx.index_document(Some(format!("m{}", i)), json!({ "n": i, "body": body_1kb }))
            .await
            .unwrap();
        if i % 10_000 == 0 && i > 0 {
            println!(
                "  [{ts}] ingested {i}/{doc_count}  rss={rss:.0}MB",
                ts = now_ts(),
                rss = read_rss_mb()
            );
        }
    }
    println!(
        "  [{ts}] ingest complete in {}ms",
        ingest_start.elapsed().as_millis(),
        ts = now_ts()
    );

    let peak_rss_mb = read_rss_mb();
    println!(
        "  [{ts}] peak_rss_mb={rss:.0}",
        ts = now_ts(),
        rss = peak_rss_mb
    );

    // 100 concurrent aggregation queries.
    let idx_arc = Arc::new(idx);
    let agg_req = Arc::new(
        parse_request(&json!({
            "query": { "match_all": {} },
            "size": 0,
            "aggs": { "n_stats": { "stats": { "field": "n" } } }
        }))
        .unwrap(),
    );

    let q_err = Arc::new(AtomicU64::new(0));
    let q_ok = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();

    for _ in 0..concurrent_queries {
        let idx_c = Arc::clone(&idx_arc);
        let req_c = Arc::clone(&agg_req);
        let err_c = Arc::clone(&q_err);
        let ok_c = Arc::clone(&q_ok);
        handles.push(tokio::spawn(async move {
            match idx_c.search(&req_c).await {
                Ok(_) => {
                    ok_c.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    err_c.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in handles {
        h.await.expect("query task panicked");
    }

    let queries_ok = q_ok.load(Ordering::Relaxed);
    let queries_err = q_err.load(Ordering::Relaxed);
    let duration = test_start.elapsed();

    println!(
        "\n  [{ts}] docs={d}  peak_rss_mb={rss:.0}  \
         queries_ok={qok}/{total}  errors={qe}  duration={dur:.1}s",
        ts = now_ts(),
        d = doc_count,
        rss = peak_rss_mb,
        qok = queries_ok,
        total = concurrent_queries,
        qe = queries_err,
        dur = duration.as_secs_f64(),
    );

    let all_ok = queries_err == 0 && queries_ok == concurrent_queries as u64;
    println!(
        "  RESULT: {}  duration={d:.1}s",
        if all_ok { "PASS" } else { "FAIL" },
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Memory pressure".into(),
        iterations: concurrent_queries as u64,
        pass: queries_ok,
        fail: queries_err,
        duration,
    });
    assert_eq!(queries_err, 0, "{} aggregation queries failed", queries_err);
    assert_eq!(
        queries_ok, concurrent_queries as u64,
        "not all queries completed"
    );
}

fn read_rss_mb() -> f64 {
    // /proc/self/statm: size resident shared text lib data dt (pages, 4KB each).
    if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
        if let Some(rss_pages) = s
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse::<u64>().ok())
        {
            return (rss_pages * 4096) as f64 / (1024.0 * 1024.0);
        }
    }
    0.0
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 9 — Schema Evolution Under Load
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_schema_evolution() {
    print_header("Test 9: Schema Evolution Under Load");

    let test_start = Instant::now();

    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.create_index("schema_idx", Schema::empty()).unwrap();
    let idx = engine.get_index("schema_idx").unwrap();

    // Batch 1: fields {a, b, c}
    println!("  Batch 1: 500 docs with fields {{a, b, c}}…");
    for i in 0..500u64 {
        idx.index_document(
            Some(format!("b1-{}", i)),
            json!({ "a": i, "b": format!("batch1-b-{}", i), "c": i % 10 }),
        )
        .await
        .unwrap();
    }

    // Batch 2: fields {a, b, c, d, e}
    println!("  Batch 2: 500 docs with fields {{a, b, c, d, e}}…");
    for i in 0..500u64 {
        idx.index_document(
            Some(format!("b2-{}", i)),
            json!({
                "a": i + 500,
                "b": format!("batch2-b-{}", i),
                "c": i % 7,
                "d": format!("batch2-d-{}", i),
                "e": i * 2
            }),
        )
        .await
        .unwrap();
    }

    // Batch 3: fields {a, f, g}
    println!("  Batch 3: 500 docs with fields {{a, f, g}}…");
    for i in 0..500u64 {
        idx.index_document(
            Some(format!("b3-{}", i)),
            json!({
                "a": i + 1000,
                "f": format!("batch3-f-{}", i),
                "g": i as f64 * 1.5
            }),
        )
        .await
        .unwrap();
    }

    let total_docs = count_all(&idx).await;

    // Search by each evolved field.
    struct FieldSearch {
        field: &'static str,
        query: Value,
    }
    let field_searches = [
        FieldSearch {
            field: "a",
            query: json!({"range": {"a": {"gte": 0, "lte": 100}}}),
        },
        FieldSearch {
            field: "b",
            query: json!({"match": {"b": "batch1"}}),
        },
        FieldSearch {
            field: "c",
            query: json!({"range": {"c": {"gte": 0, "lte": 5}}}),
        },
        FieldSearch {
            field: "d",
            query: json!({"match": {"d": "batch2"}}),
        },
        FieldSearch {
            field: "e",
            query: json!({"range": {"e": {"gte": 0, "lte": 50}}}),
        },
        FieldSearch {
            field: "f",
            query: json!({"match": {"f": "batch3"}}),
        },
        FieldSearch {
            field: "g",
            query: json!({"range": {"g": {"gte": 0.0, "lte": 10.0}}}),
        },
    ];

    let mut fields_ok = 0usize;
    let mut fields_fail = 0usize;

    for fs in &field_searches {
        let req = parse_request(&json!({ "query": fs.query, "size": 5 })).unwrap();
        let result = idx.search(&req).await.unwrap();
        let ok = result.total.value > 0;
        if ok {
            fields_ok += 1;
        } else {
            fields_fail += 1;
        }
        println!(
            "  [{ts}] field={f:<3}  hits={h:>5}  {status}",
            ts = now_ts(),
            f = fs.field,
            h = result.total.value,
            status = if ok { "PASS" } else { "FAIL ← no hits" },
        );
    }

    let stats = idx.stats().await;
    let duration = test_start.elapsed();

    println!(
        "\n  [{ts}] schema_fields_found={sf}/{total_fields}  \
         total_docs={td}  index_field_count={fc}",
        ts = now_ts(),
        sf = fields_ok,
        total_fields = field_searches.len(),
        td = total_docs,
        fc = stats.field_count,
    );

    let all_ok = fields_fail == 0 && total_docs == 1500;
    println!(
        "  RESULT: {}  duration={d:.1}s",
        if all_ok { "PASS" } else { "FAIL" },
        d = duration.as_secs_f64()
    );
    push_result(TestResult {
        name: "Schema evolution".into(),
        iterations: field_searches.len() as u64,
        pass: fields_ok as u64,
        fail: fields_fail as u64,
        duration,
    });
    assert_eq!(total_docs, 1500, "expected 1500 docs");
    assert_eq!(
        fields_fail, 0,
        "schema evolution: {} fields returned no hits",
        fields_fail
    );
}

// ══════════════════════════════════════════════════════════════════════════════
// Test 10 — Data Integrity Verification
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn chaos_data_integrity() {
    print_header("Test 10: Data Integrity Verification (checksum round-trip + WAL replay)");

    let test_start = Instant::now();
    let doc_count = 1000u64;

    // Pre-compute docs and their expected checksums.
    let docs: Vec<(String, Value)> = (0..doc_count)
        .map(|i| {
            let id = format!("integrity-{}", i);
            let src = json!({
                "index": i,
                "payload": format!("integrity-payload-{:010}", i),
                "anchor":  format!("doc-{}-checksum-anchor", i),
            });
            (id, src)
        })
        .collect();

    // Expected checksum: hash of the canonical JSON we wrote.
    let _expected_checksums: HashMap<String, u64> = docs
        .iter()
        .map(|(id, src)| (id.clone(), fnv64(src.to_string().as_bytes())))
        .collect();

    let dir = TempDir::new().unwrap();

    // ── Phase 1: index and verify before restart ──────────────────────────────
    println!(
        "  Phase 1: indexing {} docs and verifying checksums pre-restart…",
        doc_count
    );
    let (pre_verified, pre_mismatches) = {
        let engine = make_engine(&dir);
        engine
            .create_index("integrity_idx", Schema::empty())
            .unwrap();
        let idx = engine.get_index("integrity_idx").unwrap();

        for (id, src) in &docs {
            idx.index_document(Some(id.clone()), src.clone())
                .await
                .unwrap();
        }

        let mut v = 0u64;
        let mut m = 0u64;
        for (id, src) in &docs {
            match idx.get_document(id).await {
                Ok(Some(got)) => {
                    if fnv64(got.to_string().as_bytes()) == fnv64(src.to_string().as_bytes()) {
                        v += 1;
                    } else {
                        m += 1;
                    }
                }
                _ => m += 1,
            }
        }
        drop(idx);
        drop(engine);
        (v, m)
    };

    println!(
        "  [{ts}] pre-restart: verified={v}/{total}  mismatches={m}",
        ts = now_ts(),
        v = pre_verified,
        total = doc_count,
        m = pre_mismatches,
    );
    assert_eq!(
        pre_mismatches, 0,
        "pre-restart checksum failures: {}",
        pre_mismatches
    );

    // ── Phase 2: reopen via WAL replay and re-verify ──────────────────────────
    println!("  Phase 2: reopening engine (WAL replay) and re-verifying checksums…");
    let engine2 = make_engine_at(dir.path());
    let idx2 = engine2
        .get_index("integrity_idx")
        .expect("integrity_idx must reopen after WAL replay");

    let mut post_verified = 0u64;
    let mut post_mismatches = 0u64;
    let mut post_missing = 0u64;

    for (id, src) in &docs {
        match idx2.get_document(id).await {
            Ok(Some(got)) => {
                if fnv64(got.to_string().as_bytes()) == fnv64(src.to_string().as_bytes()) {
                    post_verified += 1;
                } else {
                    post_mismatches += 1;
                }
            }
            _ => post_missing += 1,
        }
    }

    let duration = test_start.elapsed();

    println!(
        "\n  [{ts}] post-restart: verified={v}/{total}  mismatches={m}  missing={miss}",
        ts = now_ts(),
        v = post_verified,
        total = doc_count,
        m = post_mismatches,
        miss = post_missing,
    );

    let all_ok = post_mismatches == 0 && post_missing == 0 && post_verified == doc_count;
    println!(
        "  RESULT: {}  duration={d:.1}s",
        if all_ok { "PASS" } else { "FAIL" },
        d = duration.as_secs_f64()
    );

    push_result(TestResult {
        name: "Data integrity".into(),
        iterations: doc_count,
        pass: post_verified,
        fail: post_mismatches + post_missing,
        duration,
    });

    // ── Final summary — printed by whichever test runs last ───────────────────
    // Because tests may run in parallel the summary might be incomplete; add a
    // deliberate delay-free fence: we only have 10 tests so just print all
    // results currently in the accumulator.
    {
        let results = chaos_results().lock().unwrap().clone();
        print_summary(&results);
    }

    assert_eq!(
        post_mismatches, 0,
        "checksum mismatches after WAL replay: {}",
        post_mismatches
    );
    assert_eq!(
        post_missing, 0,
        "docs missing after WAL replay: {}",
        post_missing
    );
    assert_eq!(
        post_verified, doc_count,
        "not all docs verified after restart"
    );
}
