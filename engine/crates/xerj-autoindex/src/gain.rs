//! `xerj gain` — what XERJ actually did for you, from the audit log.
//!
//! Every number here is counted from the node's hash-chained audit log —
//! real requests that really happened. Deliberately absent: "tokens saved",
//! "time saved", or any other counterfactual. Estimated savings dashboards
//! read well until someone A/B-tests the estimate; counted requests do not
//! have that failure mode.

use crate::esclient::Es;
use serde_json::{json, Value};

const USAGE: &str = "\
xerj gain — what this node has done for you, counted (not estimated)

USAGE:
    xerj gain [OPTIONS]

Reads the node's hash-chained audit log and reports real usage: searches
served, hit rate, latency, the busiest indices, and a recent-search strip.

OPTIONS:
    --url <U>       node endpoint (default $XERJ_URL or http://localhost:9200)
    --api-key <K>   Authorization (or $XERJ_API_KEY; --insecure nodes need none)
    --json          machine-readable stats instead of the dashboard
    -h, --help      this help
";

/// One parsed search event from an audit entry.
#[derive(Debug, Clone)]
pub(crate) struct SearchEvent {
    pub index: String,
    pub took_ms: u64,
    pub hits: u64,
}

/// The search events a human caused, plus the machine traffic the same
/// audit window holds (#1105). `xerj gain` is "what did XERJ do FOR YOU" —
/// autoindex's own verification round-trips are not that, and folding them
/// in reported a 3% hit rate immediately after a flawless onboarding where
/// every real search found something.
#[derive(Debug, Default)]
pub(crate) struct SearchWindow {
    pub user: Vec<SearchEvent>,
    /// `size:0` searches — counting probes that asked for no hits.
    pub counting_probes: u64,
    /// Searches against `autoindex-catalog`, the index autoindex itself
    /// maintains (incremental reconcile sweeps it with full-page searches).
    pub catalog_sweeps: u64,
}

impl SearchWindow {
    /// Machine entries excluded from `user`, for the honesty line.
    fn excluded(&self) -> u64 {
        self.counting_probes + self.catalog_sweeps
    }
}

/// Parse the audit `note` written by the search handler:
/// `took=12ms hits=3 size=10`. Tolerant: missing pieces become 0 (or
/// `None` for `size`, which entries written before that field existed —
/// or by any other client — simply do not carry).
pub(crate) fn parse_note(note: &str) -> (u64, u64, Option<u64>) {
    let mut took = 0;
    let mut hits = 0;
    let mut size = None;
    for tok in note.split_whitespace() {
        if let Some(v) = tok.strip_prefix("took=") {
            took = v.trim_end_matches("ms").parse().unwrap_or(0);
        } else if let Some(v) = tok.strip_prefix("hits=") {
            hits = v.parse().unwrap_or(0);
        } else if let Some(v) = tok.strip_prefix("size=") {
            size = v.parse().ok();
        }
    }
    (took, hits, size)
}

/// Pull search events out of the audit snapshot (`{entries: [...]}`),
/// separating the user's searches from autoindex's machine traffic.
///
/// Two sources of truth, in order (#1109): an entry carrying the server-set
/// `actor` field is classified by it — the server derived the class from the
/// request itself at append time, which a client cannot influence; entries
/// written by a binary older than #1109 carry no `actor`, and for those the
/// note/resource heuristics below remain the (only) derivation. The two agree
/// on everything the heuristics could see; the field's value is that it is
/// authoritative rather than inferred.
pub(crate) fn search_events(audit: &Value) -> SearchWindow {
    let mut out = SearchWindow::default();
    let Some(entries) = audit.get("entries").and_then(Value::as_array) else {
        return out;
    };
    for e in entries {
        if e.get("op").and_then(Value::as_str) != Some("search") {
            continue;
        }
        let resource = e
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let note = e.get("note").and_then(Value::as_str).unwrap_or("");
        let (took_ms, hits, size) = parse_note(note);
        match e.get("actor").and_then(Value::as_str) {
            // Written by a post-#1109 server: trust the server-side class.
            // Within "machine", keep the probe/sweep split for the honesty
            // line: the two machine classes are exactly size:0 probes and
            // catalog sweeps, so the size alone disambiguates them.
            Some("machine") => {
                if size == Some(0) {
                    out.counting_probes += 1;
                } else {
                    out.catalog_sweeps += 1;
                }
                continue;
            }
            Some(_) => {}
            // No actor field: a log written before #1109. The note/resource
            // heuristics that shipped with #1105 stay as the legacy
            // derivation — an old log read by a new client must still
            // separate machine traffic, or every upgraded install would
            // report a hit-rate dip that is really a version skew.
            None => {
                if size == Some(0) {
                    out.counting_probes += 1;
                    continue;
                }
                if resource == crate::catalog::CATALOG_INDEX {
                    out.catalog_sweeps += 1;
                    continue;
                }
            }
        }
        out.user.push(SearchEvent {
            index: resource,
            took_ms,
            hits,
        });
    }
    out
}

/// Computed stats over the events, ready to render or serialize.
pub(crate) fn stats(events: &[SearchEvent]) -> Value {
    let total = events.len() as u64;
    let with_hits = events.iter().filter(|e| e.hits > 0).count() as u64;
    let zero = total - with_hits;
    let mut tooks: Vec<u64> = events.iter().map(|e| e.took_ms).collect();
    tooks.sort_unstable();
    let pct = |p: f64| -> u64 {
        if tooks.is_empty() {
            0
        } else {
            tooks[((tooks.len() - 1) as f64 * p) as usize]
        }
    };
    // busiest indices
    let mut by_index: std::collections::BTreeMap<&str, (u64, u64)> = Default::default();
    for e in events {
        let slot = by_index.entry(e.index.as_str()).or_default();
        slot.0 += 1;
        if e.hits > 0 {
            slot.1 += 1;
        }
    }
    let mut top: Vec<(&str, u64, u64)> = by_index.iter().map(|(k, v)| (*k, v.0, v.1)).collect();
    top.sort_by_key(|x| std::cmp::Reverse(x.1));
    top.truncate(5);
    serde_json::json!({
        "searches": total,
        "with_hits": with_hits,
        "zero_hit": zero,
        "hit_rate": if total > 0 { (with_hits as f64 / total as f64 * 100.0).round() } else { 0.0 },
        "took_ms": { "p50": pct(0.50), "p95": pct(0.95) },
        "top_indices": top.iter().map(|(name, n, h)| serde_json::json!({
            "index": name, "searches": n, "with_hits": h
        })).collect::<Vec<_>>(),
    })
}

/// The recent-search strip: one glyph per search, newest last.
/// `█` a search that found something, `·` a zero-hit search.
pub(crate) fn strip(events: &[SearchEvent], width: usize) -> String {
    events
        .iter()
        .rev()
        .take(width)
        .rev()
        .map(|e| if e.hits > 0 { '█' } else { '·' })
        .collect()
}

/// Entry point for the `gain` subcommand. Returns a process exit code.
pub fn run_gain_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:9200".to_string());
    let mut api_key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
    let mut as_json = false;
    let mut it = xerj_common::argv::args_after_subcommand().into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return 0;
            }
            "--url" => url = it.next().unwrap_or(url),
            "--api-key" => api_key = it.next(),
            "--json" => as_json = true,
            other => {
                eprintln!("xerj gain: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        }
    }
    let es = match Es::new(&url, api_key) {
        Ok(es) => es,
        Err(e) => {
            eprintln!("xerj gain: could not build client for {url}: {e}");
            return 2;
        }
    };
    // The audit API lives on the node's NATIVE listener, whose default port
    // is 8080 (the startup banner prints it as 'Native REST'). Try the given
    // URL first — it costs one request and keeps working if the routes ever
    // merge — then the default native port on the same host, then the
    // ES-port+1 convention for nodes that co-locate them that way (#1104:
    // probing only port+1 missed the default entirely and `gain` was
    // unusable without hand-guessing the port).
    let audit = match es.get_json("/_audit/_search") {
        Ok(v) => v,
        Err(_) => {
            let key = std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty());
            let mut retry = None;
            let mut tried = vec![url.clone()];
            for candidate in native_candidates(&url) {
                tried.push(candidate.clone());
                if let Ok(nes) = Es::new(&candidate, key.clone()) {
                    if let Ok(v) = nes.get_json("/_audit/_search") {
                        retry = Some(v);
                        break;
                    }
                }
            }
            match retry {
                Some(v) => v,
                None => {
                    eprintln!(
                        "xerj gain: could not read the audit log at {}.\n\
                         (the audit API is served by the node's native listener — the port the\n\
                         startup banner prints as 'native REST'; a default node uses 8080.\n\
                         A node started with --insecure serves it without a key; otherwise\n\
                         the key needs read_audit.)",
                        tried
                            .iter()
                            .map(|u| format!("{u}/_audit/_search"))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    );
                    return 1;
                }
            }
        }
    };
    let window = search_events(&audit);
    let excluded = window.excluded();
    let (probes, sweeps) = (window.counting_probes, window.catalog_sweeps);
    let events = window.user;
    let s = stats(&events);
    if as_json {
        let mut s = s;
        s["excluded_machine_searches"] = json!({
            "counting_probes": probes,
            "catalog_sweeps": sweeps,
        });
        println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
        return 0;
    }
    if events.is_empty() && excluded == 0 {
        println!("no searches in the audit window yet.");
        println!("(index something: xerj autoindex <folder> — then: xerj search \"<words>\")");
        return 0;
    }
    println!("xerj gain — counted from the node's audit log, no estimates\n");
    if events.is_empty() {
        println!("  no user searches in the audit window (only machine traffic).");
        println!("  try one: xerj search \"<words>\"");
    } else {
        println!(
            "  searches served   {}   ({}% found something; {} zero-hit)",
            s["searches"], s["hit_rate"], s["zero_hit"]
        );
        println!(
            "  latency           p50 {}ms · p95 {}ms",
            s["took_ms"]["p50"], s["took_ms"]["p95"]
        );
        println!("  recent            {}", strip(&events, 50));
        println!("\n  busiest indices");
        for t in s["top_indices"].as_array().into_iter().flatten() {
            println!(
                "    {:<40} {:>5} searches  ({} with hits)",
                t["index"].as_str().unwrap_or("?"),
                t["searches"],
                t["with_hits"]
            );
        }
    }
    if excluded > 0 {
        println!(
            "\n  (not counted as yours: {} machine search(es) this window — {} counting\n\
             \x20  probe(s) autoindex used to verify its own writes, {} catalog\n\
             \x20  maintenance sweep(s). Nothing was thrown away; they are in the\n\
             \x20  audit log with everything else.)",
            excluded, probes, sweeps
        );
    }
    println!(
        "\n(the audit log keeps the last N entries; this reports that window, \
         nothing more. █ = search with hits, · = zero-hit)"
    );
    0
}

/// `http://host:9200` -> `http://host:9201` (the native-listener convention).
/// None when the URL has no explicit port to bump.
pub(crate) fn bump_port(url: &str) -> Option<String> {
    let idx = url.rfind(':')?;
    // Guard against the scheme colon (`http://host` with no port).
    let tail = &url[idx + 1..];
    let port: u16 = tail.parse().ok()?;
    Some(format!("{}:{}", &url[..idx], port.checked_add(1)?))
}

/// Candidate native-listener base URLs for the audit API, in probe order:
/// the default native port (8080) on the same host first, then the
/// ES-port+1 convention. Deduplicated, so a node whose ES listener already
/// IS on 8080 is probed once.
pub(crate) fn native_candidates(url: &str) -> Vec<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let mut out = Vec::new();
    if let Some(host) = after_scheme.split([':', '/']).next() {
        if !host.is_empty() {
            out.push(format!("http://{host}:8080"));
        }
    }
    if let Some(bumped) = bump_port(url) {
        if !out.contains(&bumped) {
            out.push(bumped);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bump_port_handles_shapes() {
        assert_eq!(
            bump_port("http://localhost:9200").as_deref(),
            Some("http://localhost:9201")
        );
        assert_eq!(
            bump_port("http://10.0.0.5:9280").as_deref(),
            Some("http://10.0.0.5:9281")
        );
        assert_eq!(bump_port("http://localhost"), None);
    }

    #[test]
    fn native_candidates_default_port_first_then_port_plus_one() {
        // #1104: the default node's native REST is 8080 — it must be probed,
        // and probed before the ES-port+1 guess.
        assert_eq!(
            native_candidates("http://localhost:9200"),
            vec![
                "http://localhost:8080".to_string(),
                "http://localhost:9201".to_string()
            ]
        );
        // A node whose ES listener is itself on 8080 still gets both the
        // default native port and its +1 guess — only an exact duplicate
        // (ES port+1 == 8080) is deduplicated.
        assert_eq!(
            native_candidates("http://localhost:8080"),
            vec![
                "http://localhost:8080".to_string(),
                "http://localhost:8081".to_string()
            ]
        );
        assert_eq!(
            native_candidates("http://10.0.0.5:8079"),
            vec!["http://10.0.0.5:8080".to_string()]
        );
        assert_eq!(native_candidates("http://10.0.0.5:9280").len(), 2);
        // No port to bump still yields the default native candidate.
        assert_eq!(
            native_candidates("http://localhost"),
            vec!["http://localhost:8080".to_string()]
        );
    }

    fn audit(entries: Vec<Value>) -> Value {
        json!({ "next_seq": entries.len(), "entries": entries })
    }

    fn search_entry(index: &str, took: u64, hits: u64) -> Value {
        json!({ "op": "search", "subject": "anonymous", "resource": index,
                "outcome": "ok", "note": format!("took={took}ms hits={hits} size=10") })
    }

    #[test]
    fn parses_note_and_filters_non_search_ops() {
        let a = audit(vec![
            search_entry("ax-docs", 12, 3),
            json!({ "op": "index.create", "resource": "ax-docs", "outcome": "ok", "note": "status=200" }),
            search_entry("ref-sled-docs", 4, 0),
        ]);
        let ev = search_events(&a);
        assert_eq!(ev.user.len(), 2);
        assert_eq!((ev.user[0].took_ms, ev.user[0].hits), (12, 3));
        assert_eq!((ev.user[1].took_ms, ev.user[1].hits), (4, 0));
    }

    #[test]
    fn parse_note_tolerates_missing_size() {
        // Entries written before `size=` existed (or by another client) must
        // still count as user searches, never as counting probes.
        assert_eq!(parse_note("took=4ms hits=0"), (4, 0, None));
        assert_eq!(parse_note("took=4ms hits=7 size=0"), (4, 7, Some(0)));
        assert_eq!(parse_note("took=4ms hits=7 size=25"), (4, 7, Some(25)));
    }

    #[test]
    fn counting_probes_and_catalog_sweeps_are_not_user_searches() {
        // #1105, verbatim from the rc.80 harness run: 179 size-0 verification
        // probes on the dataset index, catalog reconcile sweeps, three real
        // searches — gain must report the three. These entries carry no
        // `actor` field (pre-#1109 shape), so they also pin the legacy
        // note/resource derivation an upgraded client reads an old log with.
        let probe = |hits: u64| {
            json!({ "op": "search", "subject": "superuser", "resource": "ax-docs",
                    "outcome": "ok", "note": format!("took=0ms hits={hits} size=0") })
        };
        let mut entries: Vec<Value> = (0..179).map(|_| probe(0)).collect();
        for _ in 0..96 {
            entries.push(json!({ "op": "search", "subject": "superuser",
                    "resource": "autoindex-catalog", "outcome": "ok",
                    "note": "took=0ms hits=0 size=1000" }));
        }
        entries.push(search_entry("ax-*", 145, 5));
        entries.push(search_entry("ax-*", 82, 2));
        entries.push(search_entry("ax-*", 1028, 10));
        let w = search_events(&audit(entries));
        assert_eq!(w.user.len(), 3);
        assert_eq!(w.counting_probes, 179);
        assert_eq!(w.catalog_sweeps, 96);
        let s = stats(&w.user);
        assert_eq!(s["searches"], 3);
        assert_eq!(s["with_hits"], 3);
        assert_eq!(s["hit_rate"], 100.0);
    }

    /// #1109: entries written by a post-#1109 server carry the server-set
    /// `actor` field, and THAT classifies — not the note heuristics. This is
    /// the anti-spoof half of the feature living where its consumer reads it:
    /// the classification was derived from the request itself at append time,
    /// so a note that says `size=10` on machine traffic (or vice versa) is
    /// data, not a verdict.
    #[test]
    fn actor_field_is_authoritative_when_present() {
        let a = audit(vec![
            // Machine by the field, and the note would agree — normal case.
            json!({ "op": "search", "subject": "superuser", "resource": "ax-docs",
                    "outcome": "ok", "actor": "machine",
                    "note": "took=0ms hits=0 size=0" }),
            // Machine by the field with a note the legacy heuristic could NOT
            // parse a class from (no size, data index): still excluded, split
            // into catalog_sweeps as the non-probe machine class.
            json!({ "op": "search", "subject": "superuser", "resource": "ax-docs",
                    "outcome": "ok", "actor": "machine",
                    "note": "took=1ms hits=5" }),
            // User by the field even where the heuristic would have said
            // machine: a post-#1109 server never writes this combination, and
            // pinning that the field wins is what makes one classifier — the
            // server's — authoritative instead of two half-trusted ones.
            json!({ "op": "search", "subject": "superuser", "resource": "ax-docs",
                    "outcome": "ok", "actor": "user",
                    "note": "took=2ms hits=1 size=0" }),
        ]);
        let w = search_events(&a);
        assert_eq!(w.user.len(), 1);
        assert_eq!(w.user[0].hits, 1);
        assert_eq!(w.counting_probes, 1);
        assert_eq!(w.catalog_sweeps, 1);
    }

    /// A mixed-version audit window — an upgraded install whose ring still
    /// holds pre-#1109 entries — must classify both halves correctly in one
    /// pass: the fieldless tail by the legacy derivation, the new head by the
    /// field.
    #[test]
    fn mixed_version_log_classifies_both_halves() {
        let a = audit(vec![
            // Old-shape entry (no actor): heuristic must still exclude it.
            json!({ "op": "search", "subject": "superuser", "resource": "ax-docs",
                    "outcome": "ok", "note": "took=0ms hits=0 size=0" }),
            // New-shape machine entry.
            json!({ "op": "search", "subject": "superuser", "resource": "autoindex-catalog",
                    "outcome": "ok", "actor": "machine",
                    "note": "took=0ms hits=40 size=100" }),
            // New-shape user entry.
            json!({ "op": "search", "subject": "superuser", "resource": "ax-docs",
                    "outcome": "ok", "actor": "user",
                    "note": "took=8ms hits=4 size=10" }),
        ]);
        let w = search_events(&a);
        assert_eq!(w.user.len(), 1);
        assert_eq!(w.user[0].took_ms, 8);
        assert_eq!(w.counting_probes, 1, "the legacy entry");
        assert_eq!(w.catalog_sweeps, 1, "the fielded entry");
    }

    #[test]
    fn stats_count_hit_rate_and_percentiles() {
        let ev: Vec<SearchEvent> = (0..10)
            .map(|i| SearchEvent {
                index: "ax-docs".into(),
                took_ms: (i + 1) * 10,
                hits: if i < 7 { 1 } else { 0 },
            })
            .collect();
        let s = stats(&ev);
        assert_eq!(s["searches"], 10);
        assert_eq!(s["with_hits"], 7);
        assert_eq!(s["hit_rate"], 70.0);
        assert_eq!(s["took_ms"]["p50"], 50);
        assert_eq!(s["took_ms"]["p95"], 90);
    }

    #[test]
    fn strip_marks_hits_and_misses_newest_last() {
        let ev = vec![
            SearchEvent {
                index: "a".into(),
                took_ms: 1,
                hits: 1,
            },
            SearchEvent {
                index: "a".into(),
                took_ms: 1,
                hits: 0,
            },
            SearchEvent {
                index: "a".into(),
                took_ms: 1,
                hits: 2,
            },
        ];
        assert_eq!(strip(&ev, 50), "█·█");
        assert_eq!(strip(&ev, 2), "·█");
    }

    #[test]
    fn empty_audit_yields_zeroed_stats() {
        let s = stats(&[]);
        assert_eq!(s["searches"], 0);
        assert_eq!(s["hit_rate"], 0.0);
    }
}
