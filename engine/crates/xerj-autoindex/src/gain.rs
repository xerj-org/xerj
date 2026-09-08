//! `xerj gain` — what XERJ actually did for you, from the audit log.
//!
//! Every number here is counted from the node's hash-chained audit log —
//! real requests that really happened. Deliberately absent: "tokens saved",
//! "time saved", or any other counterfactual. Estimated savings dashboards
//! read well until someone A/B-tests the estimate; counted requests do not
//! have that failure mode.

use crate::esclient::Es;
use serde_json::Value;

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

/// Parse the audit `note` written by the search handler: `took=12ms hits=3`.
/// Tolerant: missing pieces become 0 rather than dropping the event.
pub(crate) fn parse_note(note: &str) -> (u64, u64) {
    let mut took = 0;
    let mut hits = 0;
    for tok in note.split_whitespace() {
        if let Some(v) = tok.strip_prefix("took=") {
            took = v.trim_end_matches("ms").parse().unwrap_or(0);
        } else if let Some(v) = tok.strip_prefix("hits=") {
            hits = v.parse().unwrap_or(0);
        }
    }
    (took, hits)
}

/// Pull search events out of the audit snapshot (`{entries: [...]}`).
pub(crate) fn search_events(audit: &Value) -> Vec<SearchEvent> {
    let mut out = Vec::new();
    let Some(entries) = audit.get("entries").and_then(Value::as_array) else {
        return out;
    };
    for e in entries {
        if e.get("op").and_then(Value::as_str) != Some("search") {
            continue;
        }
        let note = e.get("note").and_then(Value::as_str).unwrap_or("");
        let (took_ms, hits) = parse_note(note);
        out.push(SearchEvent {
            index: e
                .get("resource")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
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
    let mut it = std::env::args().skip(2);
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
    // The audit API lives on the NATIVE listener, which the server starts on
    // ES-port + 1 (the startup banner prints both). Try the given URL first —
    // it costs one request and keeps working if the routes ever merge — then
    // fall back to the +1 convention.
    let audit = match es.get_json("/_audit/_search") {
        Ok(v) => v,
        Err(_) => {
            let native = bump_port(&url);
            let retry = native
                .as_deref()
                .and_then(|nu| Es::new(nu, std::env::var("XERJ_API_KEY").ok()).ok())
                .and_then(|nes| nes.get_json("/_audit/_search").ok());
            match retry {
                Some(v) => v,
                None => {
                    eprintln!(
                        "xerj gain: could not read the audit log at {url}/_audit/_search{}.\n\
                         (the audit API is served by the node's native listener — the port the\n\
                         startup banner prints as 'native REST'. A node started with --insecure\n\
                         serves it without a key; otherwise the key needs read_audit.)",
                        native
                            .map(|n| format!(" or {n}/_audit/_search"))
                            .unwrap_or_default()
                    );
                    return 1;
                }
            }
        }
    };
    let events = search_events(&audit);
    let s = stats(&events);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
        return 0;
    }
    if events.is_empty() {
        println!("no searches in the audit window yet.");
        println!("(index something: xerj autoindex <folder> — then: xerj search \"<words>\")");
        return 0;
    }
    println!("xerj gain — counted from the node's audit log, no estimates\n");
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

    fn audit(entries: Vec<Value>) -> Value {
        json!({ "next_seq": entries.len(), "entries": entries })
    }

    fn search_entry(index: &str, took: u64, hits: u64) -> Value {
        json!({ "op": "search", "subject": "anonymous", "resource": index,
                "outcome": "ok", "note": format!("took={took}ms hits={hits}") })
    }

    #[test]
    fn parses_note_and_filters_non_search_ops() {
        let a = audit(vec![
            search_entry("ax-docs", 12, 3),
            json!({ "op": "index.create", "resource": "ax-docs", "outcome": "ok", "note": "status=200" }),
            search_entry("ref-sled-docs", 4, 0),
        ]);
        let ev = search_events(&a);
        assert_eq!(ev.len(), 2);
        assert_eq!((ev[0].took_ms, ev[0].hits), (12, 3));
        assert_eq!((ev[1].took_ms, ev[1].hits), (4, 0));
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
