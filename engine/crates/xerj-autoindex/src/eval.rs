//! `xerj eval` — score retrieval against relevance judgments (#1798).
//!
//! Runs every judged query through the same query path as `xerj search`
//! (prefix resolution, mapping-aware query body, code-corpus fetch window and
//! re-rank) against a live node, and reports per-query nDCG@k, recall@k and
//! MRR@k plus their means. `--compare <prev.json>` adds a paired, seeded
//! randomization test against an earlier `--json` run.
//!
//! Metric definitions (standard; no reference retrieval was needed):
//! - gain is the judged relevance grade (linear gain, as in trec_eval's
//!   `ndcg_cut` and the BEIR/pytrec_eval numbers), discount `log2(rank + 1)`;
//!   the ideal ranking is every judged grade sorted descending, cut at k.
//! - a document is relevant when its grade is > 0.
//! - recall@k = relevant documents in the top k / all relevant documents.
//! - MRR@k = 1 / rank of the first relevant document in the top k, else 0.
//! - queries with no relevant judgment are skipped and counted, because
//!   nDCG and recall are undefined for them.
//! - repeated document ids in one ranking keep their first position only
//!   (a code corpus can return several passages of one file).
//!
//! Not in this command yet (follow-ups on #1798): a swappable scorer
//! (BM25 k1/b, per-field weights), per-field score explanations, and an
//! embeddable library mode.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use serde_json::{json, Value};

use crate::esclient::Es;
use crate::search::{
    mapped_property_names, numeric_property_names, rank_plain_search_hits, resolve_prefix,
    search_fetch_size, search_query_body_for_mapping,
};

const USAGE: &str = "\
xerj eval — score retrieval on a running node against relevance judgments

USAGE:
    xerj eval --qrels <FILE> --queries <FILE> [OPTIONS]

INPUTS:
    --qrels <FILE>     TREC qrels (`qid 0 docid grade`) or BEIR qrels TSV
                       (`query-id<TAB>corpus-id<TAB>score`, header allowed)
    --queries <FILE>   BEIR queries.jsonl (`{\"_id\":…,\"text\":…}`) or TSV
                       (`qid<TAB>query text`)

OPTIONS:
    --prefix <P>       index prefix to search, as for `xerj search` (default \"ax\")
    -k, --k <N>        cutoff for every metric (default 10)
    --id-field <F>     _source field holding the judged doc id (dotted path);
                       default: the hit's _id
    --compare <FILE>   an earlier `xerj eval --json` output: adds a paired
                       randomization test per metric
    --iterations <N>   randomization rounds (default 10000)
    --seed <N>         randomization seed (default 42), so p-values repeat
    --url <U>          node endpoint (default $XERJ_URL or the local node)
    --api-key <K>      Authorization (or $XERJ_API_KEY)
    --json             print the full report as JSON (save it for --compare)

Each query runs through the `xerj search` query path. Metrics: nDCG@k with
linear gain (trec_eval ndcg_cut), recall@k, MRR@k; a grade > 0 is relevant.
Queries without a relevant judgment are skipped and counted.

EXAMPLE:
    xerj eval --prefix scifact --qrels qrels/test.tsv --queries queries.jsonl --json > run-a.json
    xerj eval --prefix scifact --qrels qrels/test.tsv --queries queries.jsonl --compare run-a.json
";

/// Judged grades: qid → (doc id → grade).
pub type Qrels = BTreeMap<String, HashMap<String, i64>>;

/// Per-query metric values at one cutoff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryMetrics {
    pub ndcg: f64,
    pub recall: f64,
    pub mrr: f64,
}

/// Reads one metric out of [`QueryMetrics`].
pub type MetricFn = fn(&QueryMetrics) -> f64;

/// Metric names, in report order, with their accessor.
pub const METRICS: [(&str, MetricFn); 3] = [
    ("ndcg", |m| m.ndcg),
    ("recall", |m| m.recall),
    ("mrr", |m| m.mrr),
];

/// Parse TREC (`qid iter docid grade`) or BEIR TSV (`qid docid grade`) qrels.
/// A first line whose grade is not a number is a header and is skipped.
pub fn parse_qrels(text: &str) -> Result<Qrels, String> {
    let mut out = Qrels::new();
    let mut seen_data = false;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (qid, doc, grade) = match cols.as_slice() {
            [q, _, d, g] => (*q, *d, *g),
            [q, d, g] => (*q, *d, *g),
            _ => {
                return Err(format!(
                    "qrels line {}: expected `qid 0 docid grade` or `qid<TAB>docid<TAB>grade`, got {line:?}",
                    n + 1
                ))
            }
        };
        let grade = match grade.parse::<f64>() {
            Ok(g) if g.is_finite() => g.round() as i64,
            _ if !seen_data => continue,
            _ => {
                return Err(format!(
                    "qrels line {}: grade {grade:?} is not a number",
                    n + 1
                ))
            }
        };
        seen_data = true;
        out.entry(qid.to_string())
            .or_default()
            .insert(doc.to_string(), grade);
    }
    if out.is_empty() {
        return Err("qrels file holds no judgments".into());
    }
    Ok(out)
}

/// Parse BEIR `queries.jsonl` or a `qid<TAB>text` file, in file order.
pub fn parse_queries(text: &str) -> Result<Vec<(String, String)>, String> {
    let jsonl = text.trim_start().starts_with('{');
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (!jsonl && line.starts_with('#')) {
            continue;
        }
        if jsonl {
            let v: Value = serde_json::from_str(line)
                .map_err(|e| format!("queries line {}: not JSON ({e})", n + 1))?;
            let id = ["_id", "id", "query_id", "qid"]
                .iter()
                .find_map(|k| v.get(*k))
                .and_then(id_string)
                .ok_or_else(|| format!("queries line {}: no `_id`", n + 1))?;
            let q = ["text", "query"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str))
                .ok_or_else(|| format!("queries line {}: no `text`", n + 1))?;
            out.push((id, q.to_string()));
        } else {
            let (id, q) = line
                .split_once('\t')
                .or_else(|| line.split_once(char::is_whitespace))
                .ok_or_else(|| format!("queries line {}: expected `qid<TAB>text`", n + 1))?;
            out.push((id.trim().to_string(), q.trim().to_string()));
        }
    }
    if out.is_empty() {
        return Err("queries file holds no queries".into());
    }
    Ok(out)
}

fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Score one ranking (doc ids, best first) against one query's judgments.
/// `None` when the query has no relevant (grade > 0) judgment.
pub fn score_ranking(
    ranked: &[String],
    judged: &HashMap<String, i64>,
    k: usize,
) -> Option<QueryMetrics> {
    let n_rel = judged.values().filter(|g| **g > 0).count();
    if n_rel == 0 || k == 0 {
        return None;
    }
    let mut seen = HashSet::new();
    let top: Vec<&String> = ranked
        .iter()
        .filter(|d| seen.insert(d.as_str()))
        .take(k)
        .collect();
    let discount = |i: usize| ((i + 2) as f64).log2();
    let mut dcg = 0.0;
    let mut hits = 0usize;
    let mut mrr = 0.0;
    for (i, d) in top.iter().enumerate() {
        let g = judged.get(d.as_str()).copied().unwrap_or(0);
        if g > 0 {
            dcg += g as f64 / discount(i);
            hits += 1;
            if mrr == 0.0 {
                mrr = 1.0 / (i + 1) as f64;
            }
        }
    }
    let mut ideal: Vec<i64> = judged.values().copied().filter(|g| *g > 0).collect();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let idcg: f64 = ideal
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, g)| *g as f64 / discount(i))
        .sum();
    Some(QueryMetrics {
        ndcg: dcg / idcg,
        recall: hits as f64 / n_rel as f64,
        mrr,
    })
}

/// SplitMix64: a small, seeded, dependency-free generator, so a p-value
/// repeats bit for bit across runs and platforms.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Two-sided paired randomization (sign-flip) test on per-query
/// differences `a - b`: the share of `iterations` random sign assignments
/// whose |mean| reaches the observed |mean|, with the +1 correction so the
/// p-value is never 0. Returns 1.0 for no pairs or all-zero differences.
pub fn paired_randomization_p(diffs: &[f64], iterations: usize, seed: u64) -> f64 {
    if diffs.is_empty() || iterations == 0 {
        return 1.0;
    }
    let n = diffs.len() as f64;
    let observed = (diffs.iter().sum::<f64>() / n).abs();
    if observed == 0.0 {
        return 1.0;
    }
    let mut rng = SplitMix64(seed);
    let mut at_least = 0usize;
    for _ in 0..iterations {
        let mut sum = 0.0;
        let mut bits = 0u64;
        for (i, d) in diffs.iter().enumerate() {
            if i % 64 == 0 {
                bits = rng.next();
            }
            sum += if bits & 1 == 1 { *d } else { -*d };
            bits >>= 1;
        }
        // Tolerance so a permutation equal to the observed mean (up to
        // summation-order rounding) counts as "at least as extreme".
        if (sum / n).abs() >= observed - 1e-12 {
            at_least += 1;
        }
    }
    (at_least + 1) as f64 / (iterations + 1) as f64
}

/// Paired comparison of this run against an earlier `--json` report, on the
/// queries both scored.
pub fn compare_runs(current: &Value, previous: &Value, iterations: usize, seed: u64) -> Value {
    let per_query = |report: &Value| -> HashMap<String, QueryMetrics> {
        report
            .get("queries")
            .and_then(Value::as_array)
            .map(|qs| {
                qs.iter()
                    .filter_map(|q| {
                        Some((
                            q.get("qid").and_then(id_string)?,
                            QueryMetrics {
                                ndcg: q.get("ndcg")?.as_f64()?,
                                recall: q.get("recall")?.as_f64()?,
                                mrr: q.get("mrr")?.as_f64()?,
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let a = per_query(current);
    let b = per_query(previous);
    let mut qids: Vec<&String> = a.keys().filter(|q| b.contains_key(*q)).collect();
    qids.sort();
    let mut metrics = serde_json::Map::new();
    for (name, get) in METRICS {
        let diffs: Vec<f64> = qids.iter().map(|q| get(&a[*q]) - get(&b[*q])).collect();
        let n = diffs.len().max(1) as f64;
        let mean_a = qids.iter().map(|q| get(&a[*q])).sum::<f64>() / n;
        let mean_b = qids.iter().map(|q| get(&b[*q])).sum::<f64>() / n;
        let wins = diffs.iter().filter(|d| **d > 0.0).count();
        let losses = diffs.iter().filter(|d| **d < 0.0).count();
        metrics.insert(
            name.to_string(),
            json!({
                "current": mean_a,
                "previous": mean_b,
                "delta": mean_a - mean_b,
                "wins": wins,
                "losses": losses,
                "ties": diffs.len() - wins - losses,
                "p_value": paired_randomization_p(&diffs, iterations, seed),
            }),
        );
    }
    let k_mismatch = current.get("k") != previous.get("k");
    json!({
        "test": "paired randomization (sign flip), two-sided",
        "iterations": iterations,
        "seed": seed,
        "paired_queries": qids.len(),
        "only_current": a.len() - qids.len(),
        "only_previous": b.len() - qids.len(),
        "k_mismatch": k_mismatch,
        "metrics": Value::Object(metrics),
    })
}

/// The doc id a hit is judged under: `_source.<id_field>` (dotted path) or
/// the hit's `_id`.
pub fn hit_doc_id(hit: &Value, id_field: Option<&str>) -> Option<String> {
    match id_field {
        None => hit.get("_id").and_then(id_string),
        Some(f) => {
            let ptr = format!("/_source/{}", f.replace('.', "/"));
            hit.pointer(&ptr).and_then(id_string)
        }
    }
}

#[derive(Debug, PartialEq)]
struct EvalArgs {
    qrels: String,
    queries: String,
    prefix: String,
    k: usize,
    id_field: Option<String>,
    compare: Option<String>,
    iterations: usize,
    seed: u64,
    url: String,
    api_key: Option<String>,
    as_json: bool,
}

fn parse_args(args: &[String]) -> Result<Option<EvalArgs>, String> {
    let mut a = EvalArgs {
        qrels: String::new(),
        queries: String::new(),
        prefix: crate::search::DEFAULT_SEARCH_PREFIX.to_string(),
        k: 10,
        id_field: None,
        compare: None,
        iterations: 10_000,
        seed: 42,
        url: std::env::var("XERJ_URL").unwrap_or_default(),
        api_key: std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty()),
        as_json: false,
    };
    let mut it = args.iter().cloned();
    let value = |flag: &str, v: Option<String>| -> Result<String, String> {
        v.filter(|s| !s.starts_with("--"))
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--qrels" => a.qrels = value("--qrels", it.next())?,
            "--queries" => a.queries = value("--queries", it.next())?,
            "--prefix" => a.prefix = value("--prefix", it.next())?,
            "-k" | "--k" => {
                a.k = crate::search::parse_count_arg("-k", it.next())?;
                if a.k == 0 {
                    return Err("-k must be at least 1".into());
                }
            }
            "--id-field" => a.id_field = Some(value("--id-field", it.next())?),
            "--compare" => a.compare = Some(value("--compare", it.next())?),
            "--iterations" => {
                a.iterations = crate::search::parse_count_arg("--iterations", it.next())?
            }
            "--seed" => {
                let v = value("--seed", it.next())?;
                a.seed = v
                    .parse()
                    .map_err(|_| format!("--seed needs a non-negative integer, got {v:?}"))?;
            }
            "--url" => a.url = value("--url", it.next())?,
            "--api-key" => a.api_key = Some(value("--api-key", it.next())?),
            "--json" => a.as_json = true,
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    if a.qrels.is_empty() || a.queries.is_empty() {
        return Err("--qrels and --queries are both required".into());
    }
    Ok(Some(a))
}

/// Entry point for `xerj eval`. Returns the process exit code: 0 on a
/// complete run, 1 when any query failed (the report still prints, failed
/// queries score 0), 2 on a usage or input error.
pub fn run_cli(args: &[String]) -> i32 {
    let a = match parse_args(args) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("xerj eval: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let read =
        |path: &str| std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"));
    let inputs = read(&a.qrels)
        .and_then(|t| parse_qrels(&t))
        .and_then(|qrels| Ok((qrels, parse_queries(&read(&a.queries)?)?)));
    let (qrels, queries) = match inputs {
        Ok(v) => v,
        Err(e) => {
            eprintln!("xerj eval: {e}");
            return 2;
        }
    };
    let previous = match a.compare.as_deref().map(|p| {
        read(p).and_then(|t| {
            serde_json::from_str::<Value>(&t).map_err(|e| format!("{p} is not JSON: {e}"))
        })
    }) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            eprintln!("xerj eval: --compare {e}");
            return 2;
        }
    };
    let resolved = match resolve_prefix(&a.prefix) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("xerj eval: --prefix {e}");
            return 2;
        }
    };
    let url = if a.url.is_empty() {
        xerj_common::localnode::default_url()
    } else {
        a.url.clone()
    };
    let mut api_key = a.api_key.clone();
    if api_key.is_none() && xerj_common::localauth::url_is_loopback(&url) {
        api_key = xerj_common::localauth::discover_local_admin_key_for_url_with_origin(&url)
            .map(|found| found.key);
    }
    let es = match Es::new(&url, api_key) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj eval: could not build client for {url}: {e}");
            return 2;
        }
    };
    if let Err(e) = es.ping() {
        eprintln!("xerj eval: no XERJ node reachable at {url} ({e})");
        return 2;
    }

    // Same query construction as `xerj search` (search.rs run_search_cli):
    // one mapping read, mapping-aware body, the code-corpus fetch window,
    // then the same local re-rank and trim.
    let pattern = resolved.pattern.clone();
    let raw_mapping = es.get_json(&format!("/{pattern}/_mapping")).ok();
    let mapped = raw_mapping.as_ref().and_then(mapped_property_names);
    let numeric = mapped
        .as_ref()
        .and(raw_mapping.as_ref().map(numeric_property_names));
    let code_corpus = !xerj_common::code_query::code_symbol_fields_absent(mapped.as_ref());
    let cjk_bigrams = crate::search::cjk_bigram_indices(&es, &pattern);

    let judged: Vec<&(String, String)> = queries
        .iter()
        .filter(|(qid, _)| qrels.contains_key(qid))
        .collect();
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut skipped_no_relevant = 0usize;
    let mut errors = Vec::new();
    for (qid, text) in judged.iter().map(|p| (&p.0, &p.1)) {
        let q_started = Instant::now();
        let mut body = search_query_body_for_mapping(text, a.k, mapped.as_ref(), numeric.as_ref());
        if cjk_bigrams {
            xerj_common::code_query::drop_cjk_run_phrases(&mut body, text);
        }
        if code_corpus {
            body["size"] = json!(search_fetch_size(a.k));
        }
        let ranked: Vec<String> = match es.search_without_savings(&pattern, &body) {
            Ok(mut resp) => {
                if let Some(hits) = resp.pointer_mut("/hits/hits").and_then(Value::as_array_mut) {
                    rank_plain_search_hits(hits, code_corpus, a.k, text);
                }
                resp.pointer("/hits/hits")
                    .and_then(Value::as_array)
                    .map(|hs| {
                        hs.iter()
                            .filter_map(|h| hit_doc_id(h, a.id_field.as_deref()))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Err(e) => {
                errors.push(json!({"qid": qid, "error": format!("{e:#}")}));
                Vec::new()
            }
        };
        let latency_ms = q_started.elapsed().as_secs_f64() * 1000.0;
        let Some(m) = score_ranking(&ranked, &qrels[qid.as_str()], a.k) else {
            skipped_no_relevant += 1;
            continue;
        };
        let n_rel = qrels[qid.as_str()].values().filter(|g| **g > 0).count();
        let mut top: Vec<&String> = Vec::new();
        for d in &ranked {
            if top.len() == a.k {
                break;
            }
            if !top.contains(&d) {
                top.push(d);
            }
        }
        rows.push(json!({
            "qid": qid,
            "ndcg": m.ndcg,
            "recall": m.recall,
            "mrr": m.mrr,
            "relevant": n_rel,
            "retrieved": top,
            "latency_ms": latency_ms,
        }));
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let n = rows.len();
    let mean = |key: &str| {
        if n == 0 {
            0.0
        } else {
            rows.iter()
                .filter_map(|r| r.get(key).and_then(Value::as_f64))
                .sum::<f64>()
                / n as f64
        }
    };
    let mut report = json!({
        "k": a.k,
        "prefix": resolved.prefix,
        "pattern": pattern,
        "url": url,
        "id_field": a.id_field.clone().unwrap_or_else(|| "_id".into()),
        "code_corpus": code_corpus,
        "gain": "linear (trec_eval ndcg_cut)",
        "queries_in_file": queries.len(),
        "queries_judged": judged.len(),
        "queries_scored": n,
        "skipped_no_relevant": skipped_no_relevant,
        "errors": errors,
        "mean": {
            "ndcg": mean("ndcg"),
            "recall": mean("recall"),
            "mrr": mean("mrr"),
            "latency_ms": mean("latency_ms"),
        },
        "elapsed_ms": elapsed_ms,
        "queries": rows,
    });
    if let Some(prev) = &previous {
        report["compare"] = compare_runs(&report, prev, a.iterations, a.seed);
    }
    let failed = !report["errors"].as_array().is_none_or(Vec::is_empty);
    if a.as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print!("{}", render_text(&report));
    }
    if failed {
        eprintln!(
            "xerj eval: {} query(ies) failed and scored 0; see `errors`",
            report["errors"].as_array().map_or(0, Vec::len)
        );
        return 1;
    }
    0
}

/// Plain-text report: one line per query, then the means and the compare
/// table.
pub fn render_text(report: &Value) -> String {
    let k = report["k"].as_u64().unwrap_or(0);
    let mut out = format!(
        "{:<16} {:>9} {:>10} {:>8} {:>5}\n",
        "qid",
        format!("nDCG@{k}"),
        format!("recall@{k}"),
        format!("MRR@{k}"),
        "rel"
    );
    for q in report["queries"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "{:<16} {:>9.4} {:>10.4} {:>8.4} {:>5}\n",
            q["qid"].as_str().unwrap_or(""),
            q["ndcg"].as_f64().unwrap_or(0.0),
            q["recall"].as_f64().unwrap_or(0.0),
            q["mrr"].as_f64().unwrap_or(0.0),
            q["relevant"].as_u64().unwrap_or(0),
        ));
    }
    let m = &report["mean"];
    out.push_str(&format!(
        "{:<16} {:>9.4} {:>10.4} {:>8.4}\n\n{} scored of {} judged ({} in the queries file, {} without a relevant judgment) on {} in {:.0} ms\n",
        "MEAN",
        m["ndcg"].as_f64().unwrap_or(0.0),
        m["recall"].as_f64().unwrap_or(0.0),
        m["mrr"].as_f64().unwrap_or(0.0),
        report["queries_scored"],
        report["queries_judged"],
        report["queries_in_file"],
        report["skipped_no_relevant"],
        report["pattern"].as_str().unwrap_or(""),
        report["elapsed_ms"].as_f64().unwrap_or(0.0),
    ));
    if let Some(c) = report.get("compare") {
        out.push_str(&format!(
            "\ncompare: {} paired queries, {}, {} rounds, seed {}{}\n",
            c["paired_queries"],
            c["test"].as_str().unwrap_or(""),
            c["iterations"],
            c["seed"],
            if c["k_mismatch"] == json!(true) {
                " (WARNING: runs used different k)"
            } else {
                ""
            }
        ));
        for (name, _) in METRICS {
            let r = &c["metrics"][name];
            out.push_str(&format!(
                "  {name:<7} current {:.4}  previous {:.4}  delta {:+.4}  W/L/T {}/{}/{}  p={:.4}\n",
                r["current"].as_f64().unwrap_or(0.0),
                r["previous"].as_f64().unwrap_or(0.0),
                r["delta"].as_f64().unwrap_or(0.0),
                r["wins"],
                r["losses"],
                r["ties"],
                r["p_value"].as_f64().unwrap_or(1.0),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judged(pairs: &[(&str, i64)]) -> HashMap<String, i64> {
        pairs.iter().map(|(d, g)| (d.to_string(), *g)).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    /// Hand-computed: relevant d1 (grade 1) at rank 2, d3 (grade 2) at
    /// rank 3, d9 (grade 1) not retrieved.
    /// DCG@3  = 1/log2(3) + 2/log2(4) = 0.630930 + 1.0 = 1.630930
    /// IDCG@3 = 2/log2(2) + 1/log2(3) + 1/log2(4) = 2 + 0.630930 + 0.5
    /// nDCG@3 = 1.630930 / 3.130930 = 0.520909
    /// recall@3 = 2/3, MRR@3 = 1/2.
    #[test]
    fn graded_metrics_match_hand_computation() {
        let j = judged(&[("d1", 1), ("d3", 2), ("d9", 1), ("d0", 0)]);
        let m = score_ranking(&ids(&["d0", "d1", "d3", "d4"]), &j, 3).unwrap();
        let dcg = 1.0 / 3f64.log2() + 2.0 / 4f64.log2();
        let idcg = 2.0 + 1.0 / 3f64.log2() + 1.0 / 4f64.log2();
        close(m.ndcg, dcg / idcg);
        assert!((m.ndcg - 0.520_909).abs() < 1e-6, "{}", m.ndcg);
        close(m.recall, 2.0 / 3.0);
        close(m.mrr, 0.5);
    }

    #[test]
    fn perfect_empty_and_cutoff_cases() {
        let j = judged(&[("a", 1), ("b", 1)]);
        let m = score_ranking(&ids(&["a", "b", "c"]), &j, 10).unwrap();
        close(m.ndcg, 1.0);
        close(m.recall, 1.0);
        close(m.mrr, 1.0);
        // Nothing retrieved.
        let m = score_ranking(&[], &j, 10).unwrap();
        assert_eq!(
            m,
            QueryMetrics {
                ndcg: 0.0,
                recall: 0.0,
                mrr: 0.0
            }
        );
        // The relevant doc sits at rank 4, beyond k = 3.
        let m = score_ranking(&ids(&["x", "y", "z", "a"]), &j, 3).unwrap();
        assert_eq!(m.mrr, 0.0);
        // IDCG is cut at k too: one slot, two relevant docs, first is relevant.
        let m = score_ranking(&ids(&["b", "a"]), &j, 1).unwrap();
        close(m.ndcg, 1.0);
        close(m.recall, 0.5);
        // No relevant judgment → undefined, skipped.
        assert!(score_ranking(&ids(&["a"]), &judged(&[("a", 0)]), 10).is_none());
    }

    #[test]
    fn duplicate_ids_keep_their_first_rank() {
        // `a` twice must not take two slots or double-count gain.
        let j = judged(&[("a", 1), ("b", 1)]);
        let m = score_ranking(&ids(&["a", "a", "b"]), &j, 2).unwrap();
        close(m.ndcg, 1.0);
        close(m.recall, 1.0);
    }

    #[test]
    fn parses_trec_and_beir_qrels_and_queries() {
        let trec = "q1 0 d1 1\nq1 0 d2 0\nq2 Q0 d7 2\n";
        let q = parse_qrels(trec).unwrap();
        assert_eq!(q["q1"]["d1"], 1);
        assert_eq!(q["q1"]["d2"], 0);
        assert_eq!(q["q2"]["d7"], 2);
        let beir = "query-id\tcorpus-id\tscore\n1\t31715818\t1\n3\t14717500\t1\n";
        let q = parse_qrels(beir).unwrap();
        assert_eq!(q["1"]["31715818"], 1);
        assert_eq!(q.len(), 2);
        assert!(parse_qrels("q1 d1 x\nq2 d2 y\n").is_err());
        assert!(parse_qrels("only two\n").is_err());

        let jsonl = "{\"_id\": \"1\", \"text\": \"0-dimensional biomaterials\", \"metadata\": {}}\n{\"_id\": 3, \"text\": \"second\"}\n";
        let qs = parse_queries(jsonl).unwrap();
        assert_eq!(
            qs,
            vec![
                ("1".into(), "0-dimensional biomaterials".into()),
                ("3".into(), "second".into())
            ]
        );
        let tsv = "301\tinternational organized crime\n302 poliomyelitis and post-polio\n";
        let qs = parse_queries(tsv).unwrap();
        assert_eq!(
            qs[0],
            ("301".into(), "international organized crime".into())
        );
        assert_eq!(qs[1], ("302".into(), "poliomyelitis and post-polio".into()));
    }

    #[test]
    fn hit_ids_from_id_or_source_field() {
        let h = json!({"_id": "x1", "_source": {"meta": {"doc": 42}, "doc_id": "d9"}});
        assert_eq!(hit_doc_id(&h, None).as_deref(), Some("x1"));
        assert_eq!(hit_doc_id(&h, Some("doc_id")).as_deref(), Some("d9"));
        assert_eq!(hit_doc_id(&h, Some("meta.doc")).as_deref(), Some("42"));
        assert_eq!(hit_doc_id(&h, Some("missing")), None);
    }

    #[test]
    fn randomization_test_is_seeded_and_sane() {
        // All differences positive and equal: only the all-plus assignment
        // reaches the observed mean, so p ≈ 1/2^n (n = 12) plus the +1
        // correction; with 10k rounds p is tiny.
        let strong = vec![0.1; 12];
        let p = paired_randomization_p(&strong, 10_000, 7);
        assert!(p < 0.01, "{p}");
        assert_eq!(p, paired_randomization_p(&strong, 10_000, 7), "seeded");
        // Symmetric differences: mean 0 → p = 1.
        assert_eq!(paired_randomization_p(&[0.2, -0.2], 1000, 1), 1.0);
        assert_eq!(paired_randomization_p(&[], 1000, 1), 1.0);
        // One-query difference: every sign flip of a single value has the
        // same |mean|, so p = 1.
        assert_eq!(paired_randomization_p(&[0.3], 1000, 1), 1.0);
        // Mixed noise around zero is not significant.
        let noise = [0.1, -0.12, 0.05, -0.04, 0.02, -0.03, 0.0, 0.01];
        assert!(paired_randomization_p(&noise, 10_000, 3) > 0.3);
    }

    #[test]
    fn compare_pairs_by_qid_and_reports_deltas() {
        let run = |rows: Value| json!({"k": 10, "queries": rows});
        let cur = run(json!([
            {"qid": "1", "ndcg": 1.0, "recall": 1.0, "mrr": 1.0},
            {"qid": "2", "ndcg": 0.5, "recall": 0.5, "mrr": 0.5},
            {"qid": "9", "ndcg": 0.0, "recall": 0.0, "mrr": 0.0}
        ]));
        let prev = run(json!([
            {"qid": "1", "ndcg": 0.5, "recall": 1.0, "mrr": 0.5},
            {"qid": "2", "ndcg": 0.5, "recall": 0.0, "mrr": 1.0}
        ]));
        let c = compare_runs(&cur, &prev, 1000, 42);
        assert_eq!(c["paired_queries"], 2);
        assert_eq!(c["only_current"], 1);
        assert_eq!(c["k_mismatch"], false);
        close(c["metrics"]["ndcg"]["delta"].as_f64().unwrap(), 0.25);
        assert_eq!(c["metrics"]["ndcg"]["wins"], 1);
        assert_eq!(c["metrics"]["ndcg"]["ties"], 1);
        close(c["metrics"]["mrr"]["delta"].as_f64().unwrap(), 0.0);
        assert_eq!(c["metrics"]["mrr"]["p_value"], 1.0);
        let text = render_text(&json!({
            "k": 10, "queries": cur["queries"], "mean": {"ndcg": 0.5},
            "compare": c, "pattern": "p-*"
        }));
        assert!(text.contains("nDCG@10"), "{text}");
        assert!(text.contains("delta +0.2500"), "{text}");
    }

    #[test]
    fn args_need_both_files_and_a_positive_k() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(parse_args(&s(&["--qrels", "q"])).is_err());
        assert!(parse_args(&s(&["--qrels", "q", "--queries", "x", "-k", "0"])).is_err());
        assert!(parse_args(&s(&["--qrels", "--queries", "x"])).is_err());
        assert!(parse_args(&s(&["--help"])).unwrap().is_none());
        let a = parse_args(&s(&[
            "--qrels",
            "q",
            "--queries",
            "x",
            "--k",
            "5",
            "--prefix",
            "scifact",
            "--seed",
            "9",
            "--compare",
            "prev.json",
            "--json",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!((a.k, a.seed, a.as_json), (5, 9, true));
        assert_eq!(a.prefix, "scifact");
        assert_eq!(a.compare.as_deref(), Some("prev.json"));
    }
}
