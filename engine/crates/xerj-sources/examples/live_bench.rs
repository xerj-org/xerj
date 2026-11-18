//! Measurements for the live-source PR. Not a test: run by hand against a
//! throwaway Postgres and a throwaway XERJ node on private ports.
//!
//! ```text
//! live_bench snapshot <node> <pg_url> <pw> <rows>      1M-row snapshot rows/s
//! live_bench latency  <node> <pg_url> <pw> <rate> <secs>  commit -> searchable p50/p95
//! live_bench write    <pg_url> <pw> <table> <from> <to> <rate>  paced inserts
//! live_bench verify   <node> <pg_url> <pw> <table> <index>      no loss, no duplicates
//! ```

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use xerj_sources::pg::wire::PgConn;
use xerj_sources::spec::parse_url;

fn pg(url: &str, pw: &str) -> PgConn {
    PgConn::connect(&parse_url(url).unwrap(), Some(pw), false).unwrap()
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

fn status(node: &str, name: &str) -> Value {
    http()
        .get(format!("{node}/_xerj/sources/{name}"))
        .send()
        .and_then(|r| r.json())
        .unwrap_or(Value::Null)
}

fn connect(node: &str, name: &str, url: &str, tables: &[&str]) {
    let r = http()
        .put(format!("{node}/_xerj/sources/{name}"))
        .json(&json!({"url": url, "password_env": "PG_SRC_PW", "tables": tables}))
        .send()
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().unwrap_or_default());
}

fn wait_streaming(node: &str, name: &str, limit: Duration) -> Value {
    let t = Instant::now();
    loop {
        let s = status(node, name);
        match s.pointer("/status/state").and_then(Value::as_str) {
            Some("streaming") => return s,
            Some("error") => panic!("source error: {}", s["status"]["error"]),
            _ => {}
        }
        assert!(t.elapsed() < limit, "not streaming after {limit:?}: {s}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a[0].as_str() {
        "snapshot" => {
            let (node, url, pw, rows) = (&a[1], &a[2], &a[3], a[4].parse::<u64>().unwrap());
            let mut c = pg(url, pw);
            c.simple_query("DROP TABLE IF EXISTS bench; CREATE TABLE bench (id bigint PRIMARY KEY, title text, body text, qty int, price numeric(10,2), created_at timestamptz)").unwrap();
            let t = Instant::now();
            c.simple_query(&format!(
                "INSERT INTO bench SELECT g, 'Product ' || g, repeat(md5(g::text), 4), g % 1000, (g % 9973) / 7.0, now() - (g || ' seconds')::interval FROM generate_series(1, {rows}) g; ANALYZE bench"
            ))
            .unwrap();
            eprintln!(
                "loaded {rows} rows into Postgres in {:.1} s",
                t.elapsed().as_secs_f64()
            );
            let size = c
                .simple_query("SELECT pg_size_pretty(pg_total_relation_size('bench'))")
                .unwrap();
            let t = Instant::now();
            connect(node, "bench", url, &["public.bench"]);
            let s = wait_streaming(node, "bench", Duration::from_secs(3600));
            let secs = t.elapsed().as_secs_f64();
            let n = s["status"]["tables"][0]["rows_indexed"].as_u64().unwrap();
            let count: Value = http()
                .get(format!("{node}/src-bench-public-bench/_count"))
                .send()
                .unwrap()
                .json()
                .unwrap();
            println!(
                "{}",
                json!({
                    "rows": rows,
                    "postgres_table_size": size.rows[0][0],
                    "rows_indexed": n,
                    "index_count": count["count"],
                    "connect_to_streaming_secs": secs,
                    "rows_per_sec_end_to_end": n as f64 / secs,
                    "rows_per_sec_reported": s["status"]["snapshot"]["rows_per_sec"],
                })
            );
        }
        "latency" => {
            let (node, url, pw) = (&a[1], &a[2], &a[3]);
            let rate: u64 = a[4].parse().unwrap();
            let secs: u64 = a[5].parse().unwrap();
            let mut c = pg(url, pw);
            c.simple_query(
                "DROP TABLE IF EXISTS lat; CREATE TABLE lat (id bigint PRIMARY KEY, note text)",
            )
            .unwrap();
            connect(node, "lat", url, &["public.lat"]);
            wait_streaming(node, "lat", Duration::from_secs(120));
            let idx = "src-lat-public-lat";
            let (tx, rx) = std::sync::mpsc::channel::<(u64, Instant)>();
            let node2 = node.clone();
            let reader = std::thread::spawn(move || {
                let h = http();
                let mut pending: BTreeMap<u64, Instant> = BTreeMap::new();
                let mut lats = Vec::new();
                let mut done = false;
                loop {
                    loop {
                        match rx.try_recv() {
                            Ok((id, t)) => {
                                pending.insert(id, t);
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(_) => {
                                done = true;
                                break;
                            }
                        }
                    }
                    if !pending.is_empty() {
                        let ids: Vec<String> = pending.keys().map(|k| k.to_string()).collect();
                        let r: Value = h
                            .post(format!("{node2}/{idx}/_mget"))
                            .json(&json!({"ids": ids}))
                            .send()
                            .and_then(|r| r.json())
                            .unwrap_or(Value::Null);
                        let now = Instant::now();
                        for d in r["docs"].as_array().cloned().unwrap_or_default() {
                            if d["found"] == true {
                                let id: u64 = d["_id"].as_str().unwrap().parse().unwrap();
                                if let Some(t) = pending.remove(&id) {
                                    lats.push(now.duration_since(t).as_secs_f64() * 1000.0);
                                }
                            }
                        }
                    }
                    if done && pending.is_empty() {
                        return lats;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
            // Four writer connections: one connection's commit round trip
            // (fsync included) caps a single writer near 800 commits/s.
            let writers = 4u64;
            let total = rate * secs;
            let start = Instant::now();
            let mut hs = Vec::new();
            for w in 0..writers {
                let (url, pw, tx) = (url.clone(), pw.clone(), tx.clone());
                hs.push(std::thread::spawn(move || {
                    let mut c = pg(&url, &pw);
                    let interval = Duration::from_secs_f64(writers as f64 / rate as f64);
                    let mut n = 0u32;
                    let mut i = w;
                    while i < total {
                        let due = start + interval * n;
                        if let Some(d) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(d);
                        }
                        c.simple_query(&format!("INSERT INTO lat VALUES ({i}, 'row {i}')"))
                            .unwrap();
                        if i % 10 == 0 {
                            tx.send((i, Instant::now())).unwrap();
                        }
                        n += 1;
                        i += writers;
                    }
                }));
            }
            for h in hs {
                h.join().unwrap();
            }
            let achieved = total as f64 / start.elapsed().as_secs_f64();
            drop(tx);
            let mut lats = reader.join().unwrap();
            let st = status(node, "lat");
            println!(
                "{}",
                json!({
                    "writes": total,
                    "target_rate": rate,
                    "achieved_rate": achieved,
                    "samples": lats.len(),
                    "commit_to_searchable_ms_p50": pct(&mut lats, 0.5),
                    "commit_to_searchable_ms_p95": pct(&mut lats, 0.95),
                    "commit_to_searchable_ms_p99": pct(&mut lats, 0.99),
                    "commit_to_searchable_ms_max": pct(&mut lats, 1.0),
                    "node_reported_apply_ms_p50": st["status"]["stream"]["apply_latency_ms_p50"],
                    "node_reported_apply_ms_p95": st["status"]["stream"]["apply_latency_ms_p95"],
                })
            );
        }
        "write" => {
            let (url, pw, table) = (&a[1], &a[2], &a[3]);
            let (from, to, rate): (u64, u64, u64) = (
                a[4].parse().unwrap(),
                a[5].parse().unwrap(),
                a[6].parse().unwrap(),
            );
            let mut c = pg(url, pw);
            let interval = Duration::from_secs_f64(1.0 / rate as f64);
            let start = Instant::now();
            for (n, i) in (from..to).enumerate() {
                let due = start + interval * n as u32;
                if let Some(d) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
                // Every 7th id is updated once and every 13th deleted later,
                // so recovery is checked on all three change kinds.
                c.simple_query(&format!("INSERT INTO {table} VALUES ({i}, 'v1')"))
                    .unwrap();
                if i % 7 == 0 {
                    c.simple_query(&format!("UPDATE {table} SET note = 'v2' WHERE id = {i}"))
                        .unwrap();
                }
                if i % 13 == 0 && i >= from + 50 {
                    c.simple_query(&format!("DELETE FROM {table} WHERE id = {}", i - 50))
                        .unwrap();
                }
            }
            eprintln!("wrote ids {from}..{to}");
        }
        "verify" => {
            let (node, url, pw, table, index) = (&a[1], &a[2], &a[3], &a[4], &a[5]);
            let mut c = pg(url, pw);
            let lsn = c
                .simple_query("SELECT pg_current_wal_lsn()::text")
                .unwrap()
                .rows[0][0]
                .clone()
                .unwrap();
            let target = xerj_sources::status::parse_lsn(&lsn).unwrap();
            let t = Instant::now();
            loop {
                let s = status(
                    node,
                    &a[6..].first().cloned().unwrap_or_else(|| "rec".into()),
                );
                let conf = s
                    .pointer("/status/stream/confirmed_lsn")
                    .and_then(Value::as_str)
                    .and_then(xerj_sources::status::parse_lsn)
                    .unwrap_or(0);
                if conf >= target {
                    break;
                }
                assert!(t.elapsed() < Duration::from_secs(300), "not caught up: {s}");
                std::thread::sleep(Duration::from_millis(200));
            }
            let caught_up = t.elapsed().as_secs_f64();
            let rows = c
                .simple_query(&format!("SELECT id::text, note FROM {table} ORDER BY id"))
                .unwrap()
                .rows;
            let pgmap: BTreeMap<String, String> = rows
                .into_iter()
                .map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap()))
                .collect();
            let h = http();
            let mut idx: BTreeMap<String, String> = BTreeMap::new();
            let mut sa: Option<Value> = None;
            loop {
                let mut body = json!({"size": 5000, "sort": [{"id": "asc"}], "_source": ["note"]});
                if let Some(s) = &sa {
                    body["search_after"] = s.clone();
                }
                let r: Value = h
                    .post(format!("{node}/{index}/_search"))
                    .json(&body)
                    .send()
                    .unwrap()
                    .json()
                    .unwrap();
                let hits = r["hits"]["hits"].as_array().cloned().unwrap_or_default();
                if hits.is_empty() {
                    break;
                }
                for hit in &hits {
                    let id = hit["_id"].as_str().unwrap().to_string();
                    let note = hit["_source"]["note"].as_str().unwrap_or("").to_string();
                    assert!(
                        idx.insert(id.clone(), note).is_none(),
                        "duplicate document {id}"
                    );
                }
                sa = hits.last().unwrap().get("sort").cloned();
            }
            let missing: Vec<&String> = pgmap.keys().filter(|k| !idx.contains_key(*k)).collect();
            let extra: Vec<&String> = idx.keys().filter(|k| !pgmap.contains_key(*k)).collect();
            let stale: Vec<&String> = pgmap
                .iter()
                .filter(|(k, v)| idx.get(*k).is_some_and(|n| n != *v))
                .map(|(k, _)| k)
                .collect();
            println!(
                "{}",
                json!({
                    "postgres_rows": pgmap.len(),
                    "index_docs": idx.len(),
                    "missing": missing.len(),
                    "extra_after_delete": extra.len(),
                    "stale_values": stale.len(),
                    "caught_up_secs": caught_up,
                })
            );
        }
        other => panic!("unknown mode {other}"),
    }
}
