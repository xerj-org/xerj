//! Unit tests (no network) plus a Pinecone run against a mock HTTP server.
//! Live-container tests for Qdrant, Chroma and pgvector are `#[ignore]`d and
//! read their endpoints from env (`XERJ_TEST_QDRANT`, `XERJ_TEST_CHROMA`,
//! `XERJ_TEST_PG`, plus `XERJ_TEST_URL` for the XERJ node).

use super::cli::{self, SourceSpec};
use super::*;

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

#[test]
fn routing_claims_only_vector_sources() {
    assert!(cli::looks_like_vector_source(&args("qdrant://h:6333/c")));
    assert!(cli::looks_like_vector_source(&args(
        "--url http://x chroma://h/c"
    )));
    assert!(cli::looks_like_vector_source(&args("pinecone://idx")));
    assert!(cli::looks_like_vector_source(&args(
        "postgres://u@h/db --pgvector-table items"
    )));
    assert!(!cli::looks_like_vector_source(&args("postgres://u@h/db")));
    assert!(!cli::looks_like_vector_source(&args("./docs")));
    assert!(!cli::looks_like_vector_source(&args("s3://bucket/p")));
}

#[test]
fn source_urls_parse() {
    assert_eq!(
        cli::parse_source("qdrant://localhost/docs", None).unwrap(),
        SourceSpec::Qdrant {
            base: "http://localhost:6333".into(),
            collection: "docs".into()
        }
    );
    assert_eq!(
        cli::parse_source("qdrants://q.example:443/docs", None).unwrap(),
        SourceSpec::Qdrant {
            base: "https://q.example:443".into(),
            collection: "docs".into()
        }
    );
    assert_eq!(
        cli::parse_source("chroma://h:8001/notes?tenant=t&database=d", None).unwrap(),
        SourceSpec::Chroma {
            base: "http://h:8001".into(),
            collection: "notes".into(),
            tenant: "t".into(),
            database: "d".into()
        }
    );
    assert_eq!(
        cli::parse_source("pinecone://prod?namespace=en", None).unwrap(),
        SourceSpec::Pinecone {
            index: "prod".into(),
            namespace: Some("en".into())
        }
    );
    assert!(cli::parse_source("qdrant://h/", None).is_err());
    assert!(cli::parse_source("qdrant://h/c?x=1", None).is_err());
    assert!(cli::parse_source("postgres://h/db", None).is_err());
    assert!(cli::parse_source("chroma://h/c", Some("t".into())).is_err());
}

#[test]
fn flags_that_do_not_apply_are_refused() {
    assert!(cli::parse(args("qdrant://h/c --id-column id")).is_err());
    assert!(cli::parse(args("chroma://h/c --vector-name v")).is_err());
    assert!(cli::parse(args("qdrant://h/c --prefix ax")).is_err());
    assert!(cli::parse(args("qdrant://h/c --dry-run --fresh")).is_err());
    assert!(cli::parse(args("qdrant://h/c --batch 0")).is_err());
    assert!(cli::parse(args("qdrant://h/c --similarity manhattan")).is_err());
    assert!(cli::parse(args("qdrant://h/c --vector-field document")).is_err());
    let c = cli::parse(args("qdrant://h/c --batch=50 --limit 7 --similarity dot"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (c.batch, c.limit, c.similarity),
        (50, Some(7), Some(Similarity::MaxInnerProduct))
    );
}

#[test]
fn credentials_are_redacted_and_index_names_sanitized() {
    assert_eq!(
        cli::redact_url("postgres://u:secret@h:5432/db"),
        "postgres://***@h:5432/db"
    );
    assert_eq!(cli::default_index_name("My Docs.v2"), "vec-my-docs-v2");
}

#[test]
fn distances_map_to_xerj_similarities() {
    assert_eq!(qdrant::distance("Cosine").unwrap(), Similarity::Cosine);
    assert_eq!(
        qdrant::distance("Dot").unwrap(),
        Similarity::MaxInnerProduct
    );
    assert_eq!(qdrant::distance("Euclid").unwrap(), Similarity::L2);
    assert!(qdrant::distance("Manhattan").is_err());
    assert_eq!(chroma::space("ip").unwrap(), Similarity::MaxInnerProduct);
    assert_eq!(pinecone::metric("euclidean").unwrap(), Similarity::L2);
    assert_eq!(
        pgvector::opclass_similarity("vector_ip_ops"),
        Some(Similarity::MaxInnerProduct)
    );
    assert_eq!(pgvector::opclass_similarity("btree_ops"), None);
}

#[test]
fn pgvector_text_and_identifiers() {
    assert_eq!(
        pgvector::parse_vector_text("[1,2.5,-3]").unwrap(),
        vec![1.0, 2.5, -3.0]
    );
    assert!(pgvector::parse_vector_text("[1,x]").is_err());
    assert_eq!(
        pgvector::split_table("items").unwrap(),
        ("public".into(), "items".into())
    );
    assert!(pgvector::split_table("items; drop table x").is_err());
    assert!(pgvector::split_table("a\"b").is_err());
}

#[test]
fn documents_keep_payload_and_never_overwrite_the_vector() {
    let mut payload = Map::new();
    payload.insert("title".into(), json!("x"));
    payload.insert("vector".into(), json!("clash"));
    let p = Point {
        id: "7".into(),
        vector: vec![0.5, 1.0],
        payload,
        document: Some("text".into()),
    };
    let d = point_doc(&p, "vector");
    assert_eq!(d["title"], json!("x"));
    assert_eq!(d["payload_vector"], json!("clash"));
    assert_eq!(d["vector"], json!([0.5, 1.0]));
    assert_eq!(d["document"], json!("text"));
    let body = String::from_utf8(bulk_body("i", &[p], "vector")).unwrap();
    assert_eq!(body.lines().count(), 2);
    assert!(
        body.starts_with(r#"{"index":{"_id":"7","_index":"i"}}"#) || body.contains(r#""_id":"7""#)
    );
}

#[test]
fn perturbation_is_small_and_deterministic() {
    let v = vec![1.0f32; 64];
    let a = perturb(&v, 42);
    assert_eq!(a, perturb(&v, 42));
    assert_ne!(a, v);
    assert!(a.iter().all(|x| (x - 1.0).abs() <= 0.05 + 1e-6));
}

#[test]
fn reservoir_holds_at_most_its_cap() {
    let mut r = Reservoir::new(10);
    for i in 0..1000 {
        r.offer(&Point {
            id: i.to_string(),
            vector: vec![i as f32],
            payload: Map::new(),
            document: None,
        });
    }
    assert_eq!(r.items.len(), 10);
    assert_eq!(r.seen, 1000);
    assert!(
        r.items
            .iter()
            .any(|(id, _)| id.parse::<u32>().unwrap() >= 10),
        "later points must be sampled"
    );
}

#[test]
fn journal_round_trips_and_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = cli::parse(args("qdrant://h/c")).unwrap().unwrap();
    cfg.state_dir = Some(dir.path().to_path_buf());
    let p = journal::path_for(&cfg, "vec-c");
    assert!(journal::load(&p).unwrap().is_none());
    let s = journal::State {
        source: "s".into(),
        index: "vec-c".into(),
        cursor: Some("\"abc\"".into()),
        imported: 9,
    };
    journal::save(&p, &s).unwrap();
    assert_eq!(journal::load(&p).unwrap(), Some(s));
    journal::finish(&p).unwrap();
    assert!(journal::load(&p).unwrap().is_none());
}

// ---- Pinecone against a mock HTTP server --------------------------------

mod pinecone_mock {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A tiny single-threaded HTTP/1.1 server answering the four Pinecone
    /// calls the importer makes, over 250 vectors of 4 dims.
    pub fn serve() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        let host = addr.clone();
        std::thread::spawn(move || {
            let vec_of =
                |i: usize| -> Vec<f32> { vec![i as f32, (i % 7) as f32, 1.0, -(i as f32) / 10.0] };
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut len = 0usize;
                let mut key_ok = false;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h == "\r\n" || h.is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("api-key: test-key") {
                        key_ok = true;
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let params: Vec<(String, String)> = query
                    .split('&')
                    .filter(|x| !x.is_empty())
                    .map(|kv| {
                        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                        (k.to_string(), v.to_string())
                    })
                    .collect();
                let resp = if !key_ok {
                    (401, json!({"error": "no key"}))
                } else if path == "/indexes/prod" {
                    (
                        200,
                        json!({"dimension": 4, "metric": "euclidean", "host": host}),
                    )
                } else if path == "/describe_index_stats" {
                    (
                        200,
                        json!({"namespaces": {"en": {"vectorCount": 250}}, "totalVectorCount": 250}),
                    )
                } else if path == "/vectors/list" {
                    let lim: usize = params
                        .iter()
                        .find(|(k, _)| k == "limit")
                        .map(|(_, v)| v.parse().unwrap())
                        .unwrap_or(100);
                    let start: usize = params
                        .iter()
                        .find(|(k, _)| k == "paginationToken")
                        .map(|(_, v)| v.parse().unwrap())
                        .unwrap_or(0);
                    let end = (start + lim).min(250);
                    let ids: Vec<Value> = (start..end)
                        .map(|i| json!({"id": format!("v{i:03}")}))
                        .collect();
                    let mut v = json!({"vectors": ids, "namespace": "en"});
                    if end < 250 {
                        v["pagination"] = json!({"next": end.to_string()});
                    }
                    (200, v)
                } else if path == "/vectors/fetch" {
                    let mut m = serde_json::Map::new();
                    for (k, id) in &params {
                        if k != "ids" {
                            continue;
                        }
                        let i: usize = id[1..].parse().unwrap();
                        m.insert(id.clone(), json!({"id": id, "values": vec_of(i), "metadata": {"n": i, "even": i % 2 == 0}}));
                    }
                    (200, json!({"vectors": m}))
                } else if path == "/query" {
                    let b: Value = serde_json::from_slice(&body).unwrap();
                    let q = f32s(&b["vector"]).unwrap();
                    let best = (0..250)
                        .min_by(|&a, &c| {
                            let d = |i: usize| {
                                vec_of(i)
                                    .iter()
                                    .zip(&q)
                                    .map(|(x, y)| (x - y) * (x - y))
                                    .sum::<f32>()
                            };
                            d(a).partial_cmp(&d(c)).unwrap()
                        })
                        .unwrap();
                    (
                        200,
                        json!({"matches": [{"id": format!("v{best:03}"), "score": 0.0}]}),
                    )
                } else {
                    (404, json!({"error": path}))
                };
                let text = resp.1.to_string();
                let _ = write!(s, "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", resp.0, text.len(), text);
                let _ = s.flush();
            }
        });
        addr
    }

    #[test]
    fn pinecone_source_lists_fetches_and_queries() {
        let addr = serve();
        let mut src = pinecone::Pinecone::new(
            "prod",
            Some("en".into()),
            Some("test-key".into()),
            None,
            Some(addr),
        )
        .unwrap();
        let info = src.describe().unwrap();
        assert_eq!(
            (info.dims, info.similarity, info.count),
            (4, Similarity::L2, Some(250))
        );
        let mut cursor: Option<String> = None;
        let mut all = Vec::new();
        loop {
            let p = src.page(cursor.as_deref(), 120).unwrap();
            all.extend(p.points);
            match p.next {
                Some(n) => cursor = Some(n),
                None => break,
            }
        }
        assert_eq!(all.len(), 250);
        assert_eq!(all[0].id, "v000");
        assert_eq!(all[249].payload["n"], json!(249));
        assert_eq!(
            src.top1(&[10.0, 3.0, 1.0, -1.0]).unwrap().as_deref(),
            Some("v010")
        );
    }

    #[test]
    fn pinecone_wrong_key_is_an_error_not_an_empty_index() {
        let addr = serve();
        let mut src =
            pinecone::Pinecone::new("prod", None, Some("bad".into()), None, Some(addr)).unwrap();
        let e = src.describe().unwrap_err().to_string();
        assert!(e.contains("401"), "{e}");
    }
}

// ---- live containers (run with --ignored) -------------------------------

fn live_cfg(src: &str, extra: &str) -> Option<VecImportCfg> {
    let url = std::env::var("XERJ_TEST_URL").ok()?;
    let mut c = cli::parse(args(&format!(
        "{src} --url {url} --fresh --batch 200 --progress-interval 1 {extra}"
    )))
    .unwrap()
    .unwrap();
    c.state_dir = Some(std::env::temp_dir().join("xerj-vecimport-tests"));
    Some(c)
}

#[test]
#[ignore]
fn live_qdrant_import_and_verify() {
    let Some(q) = std::env::var("XERJ_TEST_QDRANT").ok() else {
        return;
    };
    let cfg = live_cfg(&format!("qdrant://{q}"), "--index vec-live-qdrant").unwrap();
    let r = run(&cfg).unwrap();
    assert!(r.imported > 0);
    assert_eq!(
        r.count_mismatch, None,
        "paging must cover the whole collection"
    );
    assert_eq!(r.refused, 0);
    assert_eq!(r.self_hits, r.verified);
    assert!(
        r.agree * 100 >= r.verified * 95,
        "agree {}/{}",
        r.agree,
        r.verified
    );
}

#[test]
#[ignore]
fn live_chroma_import_and_verify() {
    let Some(c) = std::env::var("XERJ_TEST_CHROMA").ok() else {
        return;
    };
    let cfg = live_cfg(&format!("chroma://{c}"), "--index vec-live-chroma").unwrap();
    let r = run(&cfg).unwrap();
    assert!(r.imported > 0);
    assert_eq!(
        r.count_mismatch, None,
        "paging must cover the whole collection"
    );
    assert_eq!(r.refused, 0);
    assert_eq!(r.self_hits, r.verified);
    assert!(
        r.agree * 100 >= r.verified * 95,
        "agree {}/{}",
        r.agree,
        r.verified
    );
}

#[test]
#[ignore]
fn live_pgvector_import_and_verify() {
    let Some(pg) = std::env::var("XERJ_TEST_PG").ok() else {
        return;
    };
    let cfg = live_cfg(&pg, "--pgvector-table items --index vec-live-pg").unwrap();
    let r = run(&cfg).unwrap();
    assert!(r.imported > 0);
    assert_eq!(
        r.count_mismatch, None,
        "paging must cover the whole collection"
    );
    assert_eq!(r.refused, 0);
    assert_eq!(r.self_hits, r.verified);
    assert!(
        r.agree * 100 >= r.verified * 95,
        "agree {}/{}",
        r.agree,
        r.verified
    );
}

#[test]
#[ignore]
fn live_pgvector_pages_cover_the_table() {
    let Some(pg) = std::env::var("XERJ_TEST_PG").ok() else {
        return;
    };
    let mut s = pgvector::PgVector::new(&pg, "items", None, None).unwrap();
    let info = s.describe().unwrap();
    let mut cursor: Option<String> = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let p = s.page(cursor.as_deref(), 200).unwrap();
        eprintln!(
            "page from {:?}: {} points, next {:?}",
            cursor,
            p.points.len(),
            p.next
        );
        for x in &p.points {
            assert!(seen.insert(x.id.clone()), "dup {}", x.id);
        }
        match p.next {
            Some(n) => cursor = Some(n),
            None => break,
        }
    }
    assert_eq!(seen.len() as u64, info.count.unwrap());
}
