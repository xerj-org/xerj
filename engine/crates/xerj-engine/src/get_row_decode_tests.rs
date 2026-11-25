//! #3498: GET / `_mget` on a segment whose stored section is not resident
//! decode only the requested rows, and answer exactly what the full decode
//! (the resident-slices path) answers.

use super::*;
use serde_json::json;

/// Varied `_source` shapes: nested objects, arrays (of scalars, of objects,
/// mixed), explicit nulls, empty containers, floats, large integers,
/// unicode and escapes, and fields present on only some docs.
fn doc_body(i: usize) -> Value {
    let mut doc = json!({
        "title": format!("title {i} \u{e9}t\u{e9} \"quoted\" \\ back"),
        "n": i as i64 * 7_919,
        "f": i as f64 / 4.0,
        "tags": (0..i % 4).map(|k| format!("t{k}")).collect::<Vec<_>>(),
        "meta": {"a": i % 3, "deep": {"b": format!("x{i}"), "list": [1, "two", null]}},
    });
    if i.is_multiple_of(5) {
        doc["maybe"] = Value::Null;
    }
    if i.is_multiple_of(7) {
        doc["objs"] = json!([{"k": 1}, {"k": 2, "v": "w"}]);
        doc["empty_obj"] = json!({});
        doc["empty_arr"] = json!([]);
    }
    if i.is_multiple_of(11) {
        doc["big"] = json!(9_007_199_254_740_993u64);
        doc["neg"] = json!(-12.5e-3);
    }
    doc
}

async fn built(name: &str) -> (tempfile::TempDir, crate::Engine, Arc<Index>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    let engine = crate::Engine::new(config).unwrap();
    engine.create_index(name, Schema::empty()).unwrap();
    let idx = engine.get_index(name).unwrap();
    // Hand-driven: the segment layout below is what the asserts rely on, so
    // no background merge tick may reshape it. At load 150-300 the build
    // runs long enough for a tick (5 s) to land between a flush and its
    // `force_merge`: 30 of 60 runs ended on ONE segment (the `>= 2` assert)
    // and 6 kept a 124-doc legacy-LZ4 shard segment the tick left behind
    // (the `ZBS` assert), once `force_merge(round + 1)` saw few enough
    // segments to do nothing.
    idx.abort_background_tasks();
    // A flush splits into one small segment per memtable shard, and a stored
    // section under 128 rows stays legacy LZ4 (the full-decode path), so each
    // round is merged into one columnar segment before the next is written.
    let mut n = 0usize;
    for round in 0..3 {
        for _ in 0..2_000 {
            idx.index_document(Some(format!("g{n:04}")), doc_body(n))
                .await
                .unwrap();
            n += 1;
        }
        idx.flush().await.unwrap();
        idx.force_merge(round + 1).await.unwrap();
    }
    (dir, engine, idx, n)
}

/// Drop every segment cache once no background stored warm (scheduled by a
/// cold GET) is still running, so the next GET finds its segment cold.
async fn make_cold(idx: &Index) {
    for _ in 0..1_000 {
        if !idx.cold_stored_warm_running.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    idx.release_memory();
}

fn row_decodes() -> u64 {
    GET_ROW_DECODES.with(|n| n.get())
}

fn segment_of(idx: &Index, id: &str) -> String {
    idx.store
        .version_map
        .get(id)
        .unwrap()
        .segment_id
        .to_string()
}

#[tokio::test]
async fn a_cold_get_reads_one_row_and_matches_the_full_decode() {
    let (_dir, _engine, idx, n) = built("getrow1").await;
    let snap = idx.store.snapshot();
    assert!(snap.segments.len() >= 2, "{} segments", snap.segments.len());
    // Every segment is columnar (ZBS2/3/4; ZBS3 carries the explicit nulls).
    for meta in snap.segments.iter() {
        let reader = idx.store.open_segment_arc(&meta.id).unwrap();
        let bytes = reader.section(SectionType::Stored).unwrap().unwrap();
        assert!(
            bytes.starts_with(b"ZBS"),
            "{}: {} docs, magic {:?}",
            meta.id,
            meta.doc_count,
            &bytes[..4]
        );
    }
    for i in (0..n).step_by(97).chain([0, n - 1]) {
        let id = format!("g{i:04}");
        let seg = segment_of(&idx, &id);
        make_cold(&idx).await;
        assert!(!idx.stored_section_resident(&seg), "precondition: cold");
        let before = row_decodes();
        let cold = idx.get_document(&id).await.unwrap().expect("found cold");
        assert_eq!(
            row_decodes() - before,
            1,
            "{id}: a cold GET reads its row, not the whole section"
        );
        // What the slices path returns: the row's `_source` from the full
        // decode (read directly; whether `stored_slices_for` keeps the
        // section depends on the process-wide hydration budget).
        let reader = idx.store.open_segment_arc(&seg).unwrap();
        let raw = reader.section(SectionType::Stored).unwrap().unwrap();
        let full = xerj_storage::stored_codec::decode_stored(raw).unwrap();
        let rows: Vec<Value> = serde_json::from_slice(&full).unwrap();
        let full_source = rows
            .into_iter()
            .find(|row| row["_id"] == json!(id))
            .and_then(|row| row.get("_source").cloned())
            .expect("row in the full decode");
        assert_eq!(cold, full_source, "{id}");
        assert_eq!(
            serde_json::to_string(&cold).unwrap(),
            serde_json::to_string(&full_source).unwrap(),
            "{id}: same key order"
        );
        assert_eq!(cold, doc_body(i), "{id}: the indexed source");
    }
}

#[tokio::test]
async fn get_documents_answers_like_get_document_for_every_id() {
    let (_dir, _engine, idx, n) = built("getrow2").await;
    idx.delete_document("g0005").await.unwrap();
    idx.index_document(Some("g0010".into()), json!({"over": "written"}))
        .await
        .unwrap();
    idx.index_document(Some("mem1".into()), json!({"in": "memtable"}))
        .await
        .unwrap();
    let mut ids: Vec<String> = (0..n).step_by(37).map(|i| format!("g{i:04}")).collect();
    ids.extend(["g0005", "g0010", "mem1", "nope", "g0001", "g0001", ""].map(String::from));
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();

    make_cold(&idx).await;
    let gets_before = idx.metric_get_count.load(Ordering::Relaxed);
    let decodes_before = row_decodes();
    let batch = idx.get_documents(&refs).await;
    let segments = idx.store.snapshot().segments.len() as u64;
    let decodes = row_decodes() - decodes_before;
    assert!(
        (1..=segments).contains(&decodes),
        "one row decode per cold segment, not per id: {decodes} for {segments} segments"
    );
    assert_eq!(
        idx.metric_get_count.load(Ordering::Relaxed) - gets_before,
        refs.len() as u64,
        "each id counts as one GET"
    );
    let mut single = Vec::new();
    for id in &refs {
        single.push(idx.get_document(id).await.unwrap());
    }
    let batch: Vec<Option<Value>> = batch.into_iter().map(Result::unwrap).collect();
    assert_eq!(batch, single);
    assert_eq!(
        batch[refs.iter().position(|i| *i == "g0005").unwrap()],
        None
    );
    assert_eq!(
        batch[refs.iter().position(|i| *i == "g0010").unwrap()],
        Some(json!({"over": "written"}))
    );
    assert_eq!(
        batch[refs.iter().position(|i| *i == "mem1").unwrap()],
        Some(json!({"in": "memtable"}))
    );
    assert_eq!(batch[refs.iter().position(|i| *i == "nope").unwrap()], None);
    assert_eq!(batch[0], Some(doc_body(0)));
}

/// Before/after numbers for #3498, interleaved in one process (the box is
/// shared, so separate runs differ by up to 2x). 100k docs in large merged
/// segments; "off" is the pre-#3498 route (whole-section slices), "on" the
/// row route. Run with `--ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn get_row_route_ab_bench() {
    use xerj_storage::stored_codec::{
        decode_stored, decode_stored_v2_rows_projected_parallel, StoredV2RowHydrationResult,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_string_lossy().into_owned();
    config.merge.min_segments = 1_000_000;
    config.merge.min_merge_count = 1_000_000;
    let engine = crate::Engine::new(config).unwrap();
    engine.create_index("getbench", Schema::empty()).unwrap();
    let idx = engine.get_index("getbench").unwrap();
    let mut n = 0usize;
    for round in 0..5 {
        let mut batch = Vec::new();
        for _ in 0..20_000 {
            batch.push((
                Some(format!("b{n:07}")),
                json!({
                    "title": format!("document number {n} about topic {}", n % 97),
                    "body": format!("{} lorem ipsum dolor sit amet {}", "word ".repeat(40 + n % 20), n),
                    "n": n as i64, "f": n as f64 * 0.5,
                    "tags": [format!("t{}", n % 13), format!("u{}", n % 7)],
                    "meta": {"a": n % 3, "b": format!("m{}", n % 1000)},
                }),
            ));
            n += 1;
        }
        for r in idx.index_documents_batched(batch).await {
            r.unwrap();
        }
        idx.flush().await.unwrap();
        idx.force_merge(round + 1).await.unwrap();
    }
    let snap = idx.store.snapshot();
    let k = snap.segments.len();
    let (mut raw_b, mut full_b, mut row_b) = (0usize, 0usize, 0usize);
    for meta in snap.segments.iter() {
        let reader = idx.store.open_segment_arc(&meta.id).unwrap();
        let raw = reader.section(SectionType::Stored).unwrap().unwrap();
        raw_b += raw.len();
        full_b += decode_stored(raw).unwrap().len();
        let mid = meta.doc_count as usize / 2;
        if let StoredV2RowHydrationResult::Hydrated { stats, .. } =
            decode_stored_v2_rows_projected_parallel(raw, &[mid], None).unwrap()
        {
            row_b += stats.decompressed_buffer_bytes;
        }
    }
    eprintln!(
        "BENCH {k} segments, {n} docs; per segment: stored section {} B on disk, \
         slices route decodes {} B of row text and indexes every row, row route \
         decompresses {} B of column data and builds 1 row",
        raw_b / k,
        full_b / k,
        row_b / k
    );
    let mut rng = 0x1234_5678_9abc_def1u64;
    let mut pick = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        format!("b{:07}", rng % n as u64)
    };
    let pct = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let at = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
        format!("p50={:.1}ms p99={:.1}ms", at(0.5), at(0.99))
    };
    let load = || std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    eprintln!("BENCH load before {}", load().trim());
    for rep in 0..3 {
        for off in [true, false] {
            GET_ROW_DECODE_OFF.store(off, Ordering::Relaxed);
            let route = if off { "off" } else { "on " };
            // Cold single GET: every cache dropped before each GET.
            let mut ms = Vec::new();
            for _ in 0..20 {
                let id = pick();
                make_cold(&idx).await;
                let t = std::time::Instant::now();
                assert!(idx.get_document(&id).await.unwrap().is_some());
                ms.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let cold = pct(ms);
            // Cold _mget of 50 random ids.
            let mut ms = Vec::new();
            for _ in 0..5 {
                let ids: Vec<String> = (0..50).map(|_| pick()).collect();
                let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                make_cold(&idx).await;
                let t = std::time::Instant::now();
                for r in idx.get_documents(&refs).await {
                    assert!(r.unwrap().is_some());
                }
                ms.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let mget = pct(ms);
            // Burst from a cold start: 16 clients, 1024 random GETs.
            let ids: Arc<Vec<String>> = Arc::new((0..1024).map(|_| pick()).collect());
            let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            make_cold(&idx).await;
            let t0 = std::time::Instant::now();
            let mut tasks = Vec::new();
            for _ in 0..16 {
                let (idx, ids, next) = (Arc::clone(&idx), Arc::clone(&ids), Arc::clone(&next));
                tasks.push(tokio::spawn(async move {
                    let mut ms = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(id) = ids.get(i) else { break };
                        let t = std::time::Instant::now();
                        assert!(idx.get_document(id).await.unwrap().is_some());
                        ms.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    ms
                }));
            }
            let mut all = Vec::new();
            for t in tasks {
                all.extend(t.await.unwrap());
            }
            let wall = t0.elapsed().as_secs_f64() * 1e3;
            eprintln!(
                "BENCH rep{rep} route {route} cold GET {cold} | cold mget50 {mget} | burst16 {} wall={wall:.0}ms | load {}",
                pct(all),
                load().split_whitespace().next().unwrap_or("?")
            );
        }
    }
    GET_ROW_DECODE_OFF.store(false, Ordering::Relaxed);
}
