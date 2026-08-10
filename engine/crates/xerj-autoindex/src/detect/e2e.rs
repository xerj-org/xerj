//! End-to-end detector tests over the contract's §8 fixture: a real
//! `run_index` against a fake ES endpoint, asserting the EXACT edge set the
//! five notes must produce — including what must NOT be produced (dangling
//! links become counters, replaced files' prior edges become soft-invalidated
//! documents, and a re-run converges byte-identically modulo `created_at`).

use crate::cli::IndexCfg;
use crate::detect;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

/// index → (_id → _source). BTreeMaps keep every assertion order-stable.
type Docs = BTreeMap<String, BTreeMap<String, Value>>;

struct MockEs {
    url: String,
    docs: Arc<Mutex<Docs>>,
    stop: Arc<Mutex<bool>>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockEs {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let docs: Arc<Mutex<Docs>> = Arc::default();
        let stop = Arc::new(Mutex::new(false));
        let (sd, ss) = (Arc::clone(&docs), Arc::clone(&stop));
        let join = thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // BSD/macOS: accepted sockets inherit the listener's
                    // O_NONBLOCK; the handler does blocking reads.
                    stream.set_nonblocking(false).unwrap();
                    handle(stream, &sd)
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if *ss.lock().unwrap() {
                        break;
                    }
                    thread::yield_now();
                }
                Err(e) => panic!("mock accept: {e}"),
            }
        });
        Self {
            url,
            docs,
            stop,
            join: Some(join),
        }
    }

    fn index(&self, name: &str) -> BTreeMap<String, Value> {
        self.docs
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// rel path → node `_id` for one locator, straight from the published
    /// node docs so expectations use exactly the ids the pipeline derived.
    /// `"file"` = the per-file card (anchor of every file-level edge);
    /// `"s0"` = the first text section.
    fn nodes_at(&self, locator: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (index, docs) in self.docs.lock().unwrap().iter() {
            if index.starts_with('.') {
                continue; // catalog + edges indices are not node datasets
            }
            for (id, source) in docs {
                if source.get("ax_locator").and_then(Value::as_str) == Some(locator) {
                    if let Some(rel) = source.get("ax_path").and_then(Value::as_str) {
                        out.insert(rel.to_string(), id.clone());
                    }
                }
            }
        }
        out
    }

    fn anchors(&self) -> HashMap<String, String> {
        self.nodes_at(detect::FILE_CARD_LOCATOR)
    }
}

impl Drop for MockEs {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        self.join.take().unwrap().join().unwrap();
    }
}

fn respond(mut stream: TcpStream, status: u16, body: &Value) {
    let bytes = body.to_string();
    let reason = if status == 200 { "OK" } else { "Not Found" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        bytes.len(),
        bytes
    )
    .unwrap();
}

fn handle(stream: TcpStream, docs: &Arc<Mutex<Docs>>) {
    let clone = stream.try_clone().unwrap();
    let mut reader = BufReader::new(clone);
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
        if let Some(v) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
        {
            content_length = v.parse().unwrap();
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("").to_string();

    if method == "POST" && path == "/_bulk" {
        return respond(stream, 200, &bulk(&body, docs));
    }
    if let Some((index, id)) = doc_route(&path) {
        let locked = docs.lock().unwrap();
        return match locked.get(&index).and_then(|d| d.get(&id)) {
            Some(source) => respond(
                stream,
                200,
                &json!({"found": true, "_id": id, "_source": source}),
            ),
            None => respond(stream, 404, &json!({"found": false})),
        };
    }
    if method == "POST" && path.contains("/_search") {
        let index = path.trim_start_matches('/').split('/').next().unwrap_or("");
        let query: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        return respond(stream, 200, &search(index, &query, docs));
    }
    if method == "POST" && path.contains("/_delete_by_query") {
        let index = path.trim_start_matches('/').split('/').next().unwrap_or("");
        let query: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let key = query
            .pointer("/query/term/ax_file")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut deleted = 0u64;
        if let (Some(key), Some(store)) = (key, docs.lock().unwrap().get_mut(index)) {
            let before = store.len();
            store.retain(|_, d| d.get("ax_file").and_then(Value::as_str) != Some(key.as_str()));
            deleted = (before - store.len()) as u64;
        }
        return respond(stream, 200, &json!({"deleted": deleted, "failures": []}));
    }
    respond(stream, 200, &json!({"acknowledged": true}));
}

fn doc_route(path: &str) -> Option<(String, String)> {
    let mut segs = path.trim_start_matches('/').split('/');
    let index = segs.next()?;
    if segs.next()? != "_doc" {
        return None;
    }
    let id = segs.next()?;
    segs.next()
        .is_none()
        .then(|| (index.to_string(), id.to_string()))
}

fn bulk(body: &[u8], docs: &Arc<Mutex<Docs>>) -> Value {
    let lines: Vec<&[u8]> = body
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    let mut locked = docs.lock().unwrap();
    let mut items = Vec::new();
    let mut errors = false;
    for pair in lines.chunks_exact(2) {
        let action: Value = serde_json::from_slice(pair[0]).unwrap();
        let doc: Value = serde_json::from_slice(pair[1]).unwrap();
        let (op, meta) = if let Some(m) = action.get("index") {
            ("index", m)
        } else if let Some(m) = action.get("create") {
            ("create", m)
        } else {
            continue;
        };
        let index = meta["_index"].as_str().unwrap().to_string();
        let id = meta["_id"].as_str().unwrap().to_string();
        let store = locked.entry(index).or_default();
        if op == "create" && store.contains_key(&id) {
            errors = true;
            items.push(json!({"create": {"status": 409,
                "error": {"type": "version_conflict_engine_exception", "reason": "exists"}}}));
            continue;
        }
        store.insert(id, doc);
        items.push(json!({op: {"status": 200}}));
    }
    json!({"errors": errors, "items": items})
}

fn search(index: &str, body: &Value, docs: &Arc<Mutex<Docs>>) -> Value {
    let locked = docs.lock().unwrap();
    let store = locked.get(index).cloned().unwrap_or_default();
    // The §6.6.3 invalidation query: live edges taught by one src_file.
    if let Some(rel) = body
        .pointer("/query/bool/filter/0/term/src_file")
        .and_then(Value::as_str)
    {
        let size = body.get("size").and_then(Value::as_u64).unwrap_or(10) as usize;
        let hits: Vec<Value> = store
            .iter()
            .filter(|(_, s)| {
                s.get("src_file").and_then(Value::as_str) == Some(rel)
                    && s.get("invalid_at").is_none()
            })
            .take(size)
            .map(|(id, s)| json!({"_id": id, "_source": s}))
            .collect();
        return json!({"hits": {"total": {"value": hits.len()}, "hits": hits}});
    }
    // size:0 totals (dataset counts, map summary): total = matching docs.
    let live_only = body.pointer("/query/bool/must_not").is_some();
    let total = store
        .values()
        .filter(|s| !live_only || s.get("invalid_at").is_none())
        .count();
    json!({"hits": {"total": {"value": total}, "hits": []}, "aggregations": {}})
}

// ─── scenarios ───────────────────────────────────────────────────────────

const EDGES_INDEX: &str = ".xerj-memory-notes-edges";

fn cfg(root: &Path, state_dir: &Path, url: &str) -> IndexCfg {
    IndexCfg {
        root: root.to_owned(),
        url: url.to_owned(),
        api_key: None,
        workers: 1,
        scan_workers: 1,
        pdf_workers: 1,
        resource_notes: Vec::new(),
        pdf_timeout_secs: 30,
        bulk_mb: 8,
        bulk_timeout_secs: 300,
        snapshot_max_bytes: 64 << 30,
        prefix: "ax".into(),
        state_dir: Some(state_dir.to_owned()),
        fresh: false,
        follow_symlinks: false,
        stub_globs: Vec::new(),
        ignore: crate::ignore_rules::IgnoreOptions::default(),
        max_file_gb: 1,
        sample: 50,
        no_semantic: true,
        brain: Some("notes".into()),
        no_graph: false,
        dry_run: false,
        json: false,
        quiet: true,
        progress: crate::progress::ProgressMode::None,
        progress_interval: None,
    }
}

/// §8.1 — all five bodies verbatim; the wikilink byte offsets in §8.2 are
/// offsets into these exact strings.
fn write_fixture(dir: &Path) {
    fs::write(
        dir.join("alpha.md"),
        "Alpha is the hub note. It links to [[beta]] and [[gamma]].",
    )
    .unwrap();
    fs::write(
        dir.join("beta.md"),
        "Beta continues the thread and references [[gamma]].",
    )
    .unwrap();
    fs::write(
        dir.join("gamma.md"),
        "Gamma is the sink note with no outgoing links.",
    )
    .unwrap();
    fs::write(dir.join("delta.md"), "Delta cites [[alpha]] as its source.").unwrap();
    fs::write(dir.join("epsilon.md"), "Epsilon stands alone.").unwrap();
}

fn journal_graph_summary(state_dir: &Path) -> Value {
    let journal = fs::read_to_string(state_dir.join("journal.ndjson")).unwrap();
    journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .rfind(|v| v.get("kind").and_then(Value::as_str) == Some("finish"))
        .and_then(|v| v.pointer("/summary/graph").cloned())
        .expect("finish event with graph summary")
}

/// Comparable edge shape: everything the contract §8.3 table pins for
/// autoindex output (node-id derivation included via the live anchors).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    src: String,
    dst: String,
    edge_type: String,
    detector: String,
    weight: String,
    confidence: String,
    src_file: String,
    quote: String,
    offset: u64,
    src_format: String,
    dst_format: String,
}

fn edge_keys(edges: &BTreeMap<String, Value>) -> Vec<EdgeKey> {
    let mut out: Vec<EdgeKey> = edges
        .iter()
        .filter(|(id, _)| id.as_str() != detect::BRAIN_META_ID)
        .map(|(id, s)| {
            // Identity discipline: _id == edge_id field == the §2.3 hash.
            assert_eq!(s["edge_id"].as_str().unwrap(), id);
            assert_eq!(
                detect::edge_id(
                    s["src"].as_str().unwrap(),
                    s["type"].as_str().unwrap(),
                    s["dst"].as_str().unwrap(),
                    s["valid_at"].as_i64().unwrap(),
                ),
                id.as_str()
            );
            assert_eq!(s["schema_version"], json!(1));
            EdgeKey {
                src: s["src"].as_str().unwrap().into(),
                dst: s["dst"].as_str().unwrap().into(),
                edge_type: s["type"].as_str().unwrap().into(),
                detector: s["detector"].as_str().unwrap().into(),
                weight: s["weight"].to_string(),
                confidence: s["confidence"].to_string(),
                src_file: s["src_file"].as_str().unwrap().into(),
                quote: s["evidence"]["quote"].as_str().unwrap().into(),
                offset: s["evidence"]["offset"].as_u64().unwrap(),
                src_format: s["src_format"].as_str().unwrap_or("").into(),
                dst_format: s["dst_format"].as_str().unwrap_or("").into(),
            }
        })
        .collect();
    out.sort();
    out
}

/// `src_map`/`dst_map` pick the endpoint node per edge semantics: authored
/// text edges (wikilink) START at the section holding the evidence and LAND on
/// the target's file card; structural samedir edges connect card to card.
#[allow(clippy::too_many_arguments)]
fn expect(
    src_map: &HashMap<String, String>,
    dst_map: &HashMap<String, String>,
    src_rel: &str,
    dst_rel: &str,
    edge_type: &str,
    detector: &str,
    weight: &str,
    confidence: &str,
    quote: &str,
    offset: u64,
) -> EdgeKey {
    EdgeKey {
        src: src_map[src_rel].clone(),
        dst: dst_map[dst_rel].clone(),
        edge_type: edge_type.into(),
        detector: detector.into(),
        weight: weight.into(),
        confidence: confidence.into(),
        src_file: src_rel.into(),
        quote: quote.into(),
        offset,
        src_format: "md".into(),
        dst_format: "md".into(),
    }
}

/// The @2 opening edge of every text file's reading chain: card → section 0.
fn expect_opener(
    anchors: &HashMap<String, String>,
    sections: &HashMap<String, String>,
    rel: &str,
) -> EdgeKey {
    EdgeKey {
        src: anchors[rel].clone(),
        dst: sections[rel].clone(),
        edge_type: "sequence".into(),
        detector: "sequence@2".into(),
        weight: "0.8".into(),
        confidence: "0.99".into(),
        src_file: rel.into(),
        quote: format!("section 0 opens {rel}"),
        offset: 0,
        src_format: "md".into(),
        dst_format: "md".into(),
    }
}

#[test]
fn fixture_folder_end_to_end_matches_the_contract_edge_set() {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write_fixture(corpus.path());
    let es = MockEs::start();

    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        0
    );

    let anchors = es.anchors();
    assert_eq!(anchors.len(), 5, "five file-card anchor nodes");
    let sections = es.nodes_at("s0");
    assert_eq!(sections.len(), 5, "five s0 section nodes");
    for rel in anchors.keys() {
        assert_ne!(
            anchors[rel], sections[rel],
            "card and first section are distinct nodes"
        );
    }
    let edges = es.index(EDGES_INDEX);
    assert!(
        edges.contains_key(detect::BRAIN_META_ID),
        "brain meta doc must exist"
    );
    let meta = &edges[detect::BRAIN_META_ID];
    assert_eq!(meta["brain"], json!("notes"));
    assert_eq!(meta["meta_version"], json!(1));
    assert!(meta["nodes_index"].as_str().unwrap().starts_with("ax-"));

    let alpha_line = "Alpha is the hub note. It links to [[beta]] and [[gamma]].";
    let mut expected = vec![
        // §8.2/§8.3 wikilink edges — exact quotes and byte offsets.
        expect(
            &sections,
            &anchors,
            "alpha.md",
            "beta.md",
            "wikilink",
            "wikilink@2",
            "1.0",
            "0.95",
            alpha_line,
            35,
        ),
        expect(
            &sections,
            &anchors,
            "alpha.md",
            "gamma.md",
            "wikilink",
            "wikilink@2",
            "1.0",
            "0.95",
            alpha_line,
            48,
        ),
        expect(
            &sections,
            &anchors,
            "beta.md",
            "gamma.md",
            "wikilink",
            "wikilink@2",
            "1.0",
            "0.95",
            "Beta continues the thread and references [[gamma]].",
            41,
        ),
        expect(
            &sections,
            &anchors,
            "delta.md",
            "alpha.md",
            "wikilink",
            "wikilink@2",
            "1.0",
            "0.95",
            "Delta cites [[alpha]] as its source.",
            12,
        ),
        // §8.3 samedir chain over rel-sorted files — 4 edges, not a clique.
        expect(
            &anchors,
            &anchors,
            "alpha.md",
            "beta.md",
            "same_dir",
            "samedir@2",
            "0.3",
            "0.4",
            "alpha.md and beta.md share directory .",
            0,
        ),
        expect(
            &anchors,
            &anchors,
            "beta.md",
            "delta.md",
            "same_dir",
            "samedir@2",
            "0.3",
            "0.4",
            "beta.md and delta.md share directory .",
            0,
        ),
        expect(
            &anchors,
            &anchors,
            "delta.md",
            "epsilon.md",
            "same_dir",
            "samedir@2",
            "0.3",
            "0.4",
            "delta.md and epsilon.md share directory .",
            0,
        ),
        expect(
            &anchors,
            &anchors,
            "epsilon.md",
            "gamma.md",
            "same_dir",
            "samedir@2",
            "0.3",
            "0.4",
            "epsilon.md and gamma.md share directory .",
            0,
        ),
        // sequence@2 opening edges: every file's card starts its reading
        // chain (single-section notes have exactly the opener).
        expect_opener(&anchors, &sections, "alpha.md"),
        expect_opener(&anchors, &sections, "beta.md"),
        expect_opener(&anchors, &sections, "delta.md"),
        expect_opener(&anchors, &sections, "epsilon.md"),
        expect_opener(&anchors, &sections, "gamma.md"),
    ];
    expected.sort();
    assert_eq!(edge_keys(&edges), expected, "the exact §8.3 edge set");

    let g = journal_graph_summary(state.path());
    assert_eq!(g["edges_written"], json!(13));
    assert_eq!(g["by_detector"]["wikilink@2"], json!(4));
    assert_eq!(g["by_detector"]["samedir@2"], json!(4));
    assert_eq!(g["by_detector"]["sequence@2"], json!(5));
    assert_eq!(g["edges_unresolved"], json!(0));
    assert_eq!(g["edges_ambiguous"], json!(0));
    assert_eq!(g["edges_self_dropped"], json!(0));
    assert_eq!(g["edges_invalidated"], json!(0));

    // Determinism (§6.4): a fresh re-run over the unchanged corpus overwrites
    // every edge in place — byte-identical modulo created_at.
    let strip_created = |m: &BTreeMap<String, Value>| -> BTreeMap<String, Value> {
        m.iter()
            .map(|(id, s)| {
                let mut s = s.clone();
                if let Some(o) = s.as_object_mut() {
                    o.remove("created_at");
                }
                (id.clone(), s)
            })
            .collect()
    };
    let first = strip_created(&edges);
    let state2 = tempfile::tempdir().unwrap();
    assert_eq!(
        crate::run_index(cfg(corpus.path(), state2.path(), &es.url)).unwrap(),
        0
    );
    assert_eq!(
        strip_created(&es.index(EDGES_INDEX)),
        first,
        "re-run must converge on the identical edge set"
    );
}

/// The corpus-wide pass, end to end: documents that never cite each other and
/// do not even share a directory are linked by the words only they use — and
/// the edge lands in the edges index carrying those words as its evidence.
/// `detect_corpus` runs after Phase B, so this is also the proof that the
/// third detection phase is wired into the pipeline at all.
#[test]
fn shared_vocabulary_links_documents_that_cite_nothing() {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // Two recipes in different folders: no link, no shared directory, one
    // shared distinctive vocabulary.
    fs::create_dir(corpus.path().join("bread")).unwrap();
    fs::create_dir(corpus.path().join("pickles")).unwrap();
    fs::create_dir(corpus.path().join("admin")).unwrap();
    fs::write(
        corpus.path().join("bread/sourdough.md"),
        "The starter ferments overnight. Lactobacillus and wild yeast do the \
         work; the note on hydration is filed.",
    )
    .unwrap();
    fs::write(
        corpus.path().join("pickles/kimchi.md"),
        "Cabbage ferments in brine. Lactobacillus again, and the note says \
         three days.",
    )
    .unwrap();
    // Ten unrelated documents: enough corpus for "distinctive" to mean
    // something (the 10% rule), and everyday words in all of them.
    for i in 0..10 {
        fs::write(
            corpus.path().join(format!("admin/memo{i:02}.md")),
            format!("The note was filed. Item unique{i:02} is recorded."),
        )
        .unwrap();
    }
    let es = MockEs::start();
    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        0
    );

    let anchors = es.anchors();
    let edges = es.index(EDGES_INDEX);
    let shared: Vec<&Value> = edges
        .values()
        .filter(|s| s["type"] == json!("shared_term"))
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "exactly one vocabulary link, between the two recipes: {shared:#?}"
    );
    let e = shared[0];
    assert_eq!(e["detector"], json!("sharedterm@1"));
    assert_eq!(e["weight"], json!(0.45));
    assert_eq!(e["confidence"], json!(0.5));
    assert_eq!(e["src"], json!(anchors["bread/sourdough.md"]));
    assert_eq!(e["dst"], json!(anchors["pickles/kimchi.md"]));
    assert_eq!(e["src_file"], json!("bread/sourdough.md"));
    let quote = e["evidence"]["quote"].as_str().unwrap();
    assert!(
        quote.contains("lactobacillus") && quote.contains("ferments"),
        "the shared terms must be inspectable on the edge: {quote}"
    );
    assert!(
        !quote.contains("note") && !quote.contains("filed"),
        "words the whole corpus uses are not evidence of anything: {quote}"
    );

    let g = journal_graph_summary(state.path());
    assert_eq!(g["by_detector"]["sharedterm@1"], json!(1));
    assert_eq!(g["edges_capped"], json!(0), "nothing hit the density cap");

    // Same corpus, fresh run: the vocabulary edge is byte-identical, so a
    // re-run converges instead of accumulating beliefs.
    let state2 = tempfile::tempdir().unwrap();
    assert_eq!(
        crate::run_index(cfg(corpus.path(), state2.path(), &es.url)).unwrap(),
        0
    );
    let after: Vec<Value> = es
        .index(EDGES_INDEX)
        .values()
        .filter(|s| s["type"] == json!("shared_term"))
        .cloned()
        .collect();
    assert_eq!(after.len(), 1, "the re-run overwrote in place");
    assert_eq!(after[0]["evidence"]["quote"], json!(quote));
}

#[test]
fn dangling_wikilink_is_counted_not_silently_dropped() {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("zeta.md"),
        "Zeta links [[nowhere]] and [[eta]].",
    )
    .unwrap();
    fs::write(corpus.path().join("eta.md"), "Eta stands alone.").unwrap();
    let es = MockEs::start();

    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        0
    );

    let edges = es.index(EDGES_INDEX);
    let keys = edge_keys(&edges);
    let mut by_type: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for k in &keys {
        *by_type
            .entry((k.edge_type.as_str(), k.detector.as_str()))
            .or_default() += 1;
    }
    // One resolved wikilink + one samedir chain edge + two sequence openers
    // (card → s0, one per file); [[nowhere]] must not fabricate an edge…
    assert_eq!(
        by_type,
        BTreeMap::from([
            (("same_dir", "samedir@2"), 1),
            (("sequence", "sequence@2"), 2),
            (("wikilink", "wikilink@2"), 1),
        ])
    );
    // …but it must not vanish either: it is a dangling-link fact of record.
    let g = journal_graph_summary(state.path());
    assert_eq!(g["edges_unresolved"], json!(1));
    assert_eq!(g["edges_written"], json!(4));
}

#[test]
fn replaced_file_soft_invalidates_its_prior_edges() {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        corpus.path().join("alpha.md"),
        "Alpha cites [[beta]] today.",
    )
    .unwrap();
    fs::write(corpus.path().join("beta.md"), "Beta stands alone.").unwrap();
    let es = MockEs::start();
    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        0
    );
    let before = es.index(EDGES_INDEX);
    let old_wikilink: Vec<String> = before
        .iter()
        .filter(|(_, s)| s["type"] == json!("wikilink"))
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(old_wikilink.len(), 1);

    // Rewrite alpha at a strictly later mtime-ms: same fact, new valid_at →
    // a NEW edge_id; the old edge must survive as an invalidated document.
    std::thread::sleep(std::time::Duration::from_millis(15));
    fs::write(
        corpus.path().join("alpha.md"),
        "Alpha still cites [[beta]], reworded.",
    )
    .unwrap();
    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        0
    );

    let after = es.index(EDGES_INDEX);
    let old = &after[&old_wikilink[0]];
    assert!(
        old.get("invalid_at").is_some() && old.get("expired_at").is_some(),
        "prior edge is soft-invalidated, not deleted: {old}"
    );
    let live_wikilinks: Vec<&Value> = after
        .values()
        .filter(|s| s["type"] == json!("wikilink") && s.get("invalid_at").is_none())
        .collect();
    assert_eq!(live_wikilinks.len(), 1, "exactly one live replacement edge");
    assert_eq!(
        live_wikilinks[0]["evidence"]["quote"],
        json!("Alpha still cites [[beta]], reworded.")
    );
    let g = journal_graph_summary(state.path());
    assert!(
        g["edges_invalidated"].as_u64().unwrap() >= 1,
        "invalidation surfaced in the run summary: {g}"
    );
}
