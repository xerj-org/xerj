//! `xerj autoindex s3://bucket/prefix --watch`: the CLI route.
//!
//! Everything policy-shaped lives above this file ([`super::poll_once`],
//! [`super::cost`]); this is resolution and plumbing — which endpoint, which
//! credentials, which state directory, where the change feed goes, and how a
//! `^C` stops a watch that may be five minutes into a sleep.
//!
//! Two contracts an operator or an agent can rely on:
//!
//! * **stdout is the change feed and nothing else.** One JSON object per
//!   changed object, so `xerj autoindex s3://… --watch | jq` works. The
//!   per-cycle report goes to stderr. The single exception is the cost
//!   refusal, which prints the same decision-request document the indexing
//!   gate prints and exits [`crate::gate::EXIT_NEEDS_DECISION`] — and it
//!   happens before any event, so the two never interleave.
//! * **credentials come from the environment only.** There is no flag for
//!   them, so they cannot reach a shell history, a CI log or a `ps` listing.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use super::journal::{WatchJournal, WatchLock};
use super::s3::{parse_s3_url, Credentials, S3Source};
use super::ObjectSource;
use super::{ChangeSink, CountingSink, CycleReport, JsonlSink, PollCostRefused, WatchOptions};
use crate::cli::WatchCfg;

/// Set by the signal handler, read by the poll loop. A watch is a long-lived
/// foreground process, so `^C` has to stop it between objects rather than in the
/// middle of a journal write — the loop checks this flag, finishes what it is
/// holding, saves, and returns.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    // Async-signal-safe: one relaxed atomic store, nothing else.
    STOP.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    // SAFETY: `on_signal` does one atomic store and returns, which is
    // async-signal-safe. Installing a handler for SIGINT/SIGTERM is the whole
    // effect.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

/// `r2://bucket/prefix` is a spelling of `s3://bucket/prefix`: Cloudflare R2
/// speaks the S3 API, and what differs is the endpoint, not the protocol.
/// Normalising here rather than teaching the parser two schemes keeps one code
/// path, and keeps `r2://` from being a flag that parses and then does nothing.
pub fn normalize_object_url(url: &str) -> String {
    match url.strip_prefix("r2://") {
        Some(rest) => format!("s3://{rest}"),
        None => url.to_string(),
    }
}

/// Where the bucket is served from. Explicit flag, then `AWS_ENDPOINT_URL`
/// (which is what R2's own documentation tells people to set), then AWS's
/// regional endpoint. A watch against R2 or MinIO with neither set is refused
/// with the thing to do about it, because the alternative is signing a request
/// to Amazon with an R2 key and reporting a 403.
pub fn resolve_endpoint(flag: Option<&str>, env: Option<&str>, region: &str) -> Result<String> {
    if let Some(e) = flag.filter(|e| !e.is_empty()) {
        return Ok(e.to_string());
    }
    if let Some(e) = env.filter(|e| !e.is_empty()) {
        return Ok(e.to_string());
    }
    if region == "auto" {
        bail!(
            "no S3 endpoint and no region: pass --endpoint-url, or set AWS_ENDPOINT_URL. \
             Cloudflare R2 is https://<account-id>.r2.cloudflarestorage.com, MinIO is \
             http://<host>:9000, and AWS S3 needs --region <region> instead"
        );
    }
    Ok(format!("https://s3.{region}.amazonaws.com"))
}

/// Run the watch. Returns the process exit code.
pub fn run(cfg: WatchCfg) -> Result<i32> {
    let normalized = normalize_object_url(&cfg.url);
    let loc = parse_s3_url(&normalized)?;
    let endpoint = resolve_endpoint(
        cfg.endpoint.as_deref(),
        std::env::var("AWS_ENDPOINT_URL").ok().as_deref(),
        &cfg.region,
    )?;
    let creds = Credentials::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "no object-storage credentials: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY \
             (and AWS_SESSION_TOKEN if your provider issues one). There is deliberately no flag \
             for them — a flag would put a secret in your shell history and in `ps`"
        )
    })?;

    let src = S3Source::new(
        &endpoint,
        &cfg.region,
        creds,
        loc.clone(),
        std::time::Duration::from_secs(60),
    )?;

    let state_dir = cfg.state_dir.clone().unwrap_or_else(|| {
        // Keyed by the same helper folder watches use, so two watches on
        // different buckets never share a journal.
        crate::state::default_state_dir(&normalized, &endpoint, &loc.prefix)
    });
    // Taken before the journal is read: two watchers that both read an empty
    // journal would both index the whole bucket.
    let _lock = WatchLock::acquire(&state_dir)?;
    let mut journal = WatchJournal::open(
        &state_dir,
        &endpoint,
        &loc.bucket,
        &loc.prefix,
        cfg.append_only,
        cfg.fresh,
    )?;

    let opts = WatchOptions {
        poll_interval: cfg.poll_interval,
        max_cycles: cfg.max_cycles,
        fetch: cfg.fetch,
        max_object_bytes: cfg.max_object_bytes,
        append_only: cfg.append_only,
        max_monthly_class_a: cfg.max_monthly_ops,
        max_monthly_class_b: cfg.max_monthly_gets,
        allow_cost: cfg.allow_cost,
        page_size: cfg.page_size,
        status_path: Some(
            cfg.status_file
                .clone()
                .unwrap_or_else(|| state_dir.join("objwatch-status.json")),
        ),
        dry_run: cfg.dry_run,
    };

    if !cfg.quiet {
        eprintln!(
            "xerj-watch: {} every {}s, state {} ({} object(s) remembered){}",
            src.describe(),
            cfg.poll_interval.as_secs(),
            state_dir.display(),
            journal.len(),
            if cfg.dry_run { ", dry run" } else { "" }
        );
        if cfg.dry_run {
            eprintln!(
                "xerj-watch: dry run — one cycle, no GETs, nothing emitted, no journal. The \
                 LISTING is real: it costs Class A operations and is charged to the spend \
                 ledger like any other cycle."
            );
        }
    }

    install_signal_handlers();
    let stop = || STOP.load(Ordering::SeqCst);

    // stdout is the feed unless --events-out redirects it. --dry-run emits
    // nothing at all: it exists to price a poll, and writing a bucket-sized
    // first change set to a terminal would defeat that. (`poll_once` never
    // calls the sink in a dry run; the counting sink is belt and braces.)
    let mut counting = CountingSink::default();
    let mut file_sink;
    let mut stdout_sink;
    let sink: &mut dyn ChangeSink = if cfg.dry_run {
        &mut counting
    } else if let Some(path) = &cfg.events_out {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("create events dir {}", dir.display()))?;
            }
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open events file {}", path.display()))?;
        file_sink = JsonlSink::new(std::io::BufWriter::new(f));
        &mut file_sink
    } else {
        stdout_sink = JsonlSink::new(std::io::stdout());
        &mut stdout_sink
    };

    let quiet = cfg.quiet;
    let json = cfg.json;
    let mut on_cycle = move |r: &CycleReport| {
        // Never stdout: that is the change feed.
        if json {
            let _ = writeln!(std::io::stderr(), "{}", r.to_json());
        } else if !quiet {
            let _ = writeln!(std::io::stderr(), "{}", r.line());
            for w in &r.warnings {
                let _ = writeln!(std::io::stderr(), "xerj-watch: WARNING {w}");
            }
            for e in &r.errors {
                let _ = writeln!(std::io::stderr(), "xerj-watch: error {e}");
            }
        }
    };

    let outcome = match super::run_watch(&src, &mut journal, &opts, sink, &stop, &mut on_cycle) {
        Ok(o) => o,
        Err(e) => {
            // The cost refusal is a decision request, not a failure: same
            // document and same exit code as the indexing gate, so one
            // agent-side branch handles both.
            //
            // A breaker can trip after events were written (a bucket that grew
            // mid-watch). The decision request is then the LAST line of the
            // feed, told apart by `"xerj":"objwatch-decision-request"` where every
            // event says `"xerj":"objwatch-change"`.
            if let Some(refused) = e.downcast_ref::<PollCostRefused>() {
                println!("{}", refused.to_json());
                eprintln!("xerj-watch: {refused}");
                return Ok(crate::gate::EXIT_NEEDS_DECISION);
            }
            return Err(e);
        }
    };

    if !quiet {
        let t = &outcome.totals;
        eprintln!(
            "xerj-watch: stopped after {} cycle(s): {} Class A list call(s), {} Class B GET(s), \
             {} fetched, {} event(s), {} error(s). Status: {}",
            t.cycles,
            t.list_calls,
            t.gets,
            super::human_bytes(t.bytes_fetched),
            t.events,
            t.errors,
            opts.status_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".into())
        );
        if cfg.dry_run {
            if let Some(last) = &outcome.last {
                eprintln!("xerj-watch: {}", last.projection.line());
                eprintln!(
                    "xerj-watch: dry run saw {} added, {} changed, {} deleted; it emitted and \
                     recorded none of them, so the next real run starts from the same state. \
                     The price check itself cost {} Class A list call(s), on the ledger.",
                    last.added, last.changed, last.deleted, last.list_calls
                );
            }
        }
    }
    // An error inside a cycle (one object failed to fetch, the sink refused one
    // write) is not a failed watch: the next cycle retries it, and the journal
    // was left alone on purpose. Exit 3 says "it ran, and something in it did
    // not" — the same meaning `autoindex` gives 3 for completed-with-junk.
    if outcome.totals.errors > 0 {
        return Ok(3);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r2_urls_are_a_spelling_of_s3_urls() {
        assert_eq!(normalize_object_url("r2://b/p/"), "s3://b/p/");
        assert_eq!(normalize_object_url("s3://b/p/"), "s3://b/p/");
        // Not a URL at all: left alone so the parser gives the real error.
        assert_eq!(normalize_object_url("/tmp/x"), "/tmp/x");
        // And it must actually parse afterwards, which is the point.
        assert_eq!(
            parse_s3_url(&normalize_object_url("r2://logs/2026/"))
                .unwrap()
                .bucket,
            "logs"
        );
    }

    #[test]
    fn the_endpoint_comes_from_the_flag_then_the_environment_then_the_region() {
        assert_eq!(
            resolve_endpoint(Some("http://flag:1"), Some("http://env:2"), "auto").unwrap(),
            "http://flag:1"
        );
        assert_eq!(
            resolve_endpoint(None, Some("http://env:2"), "auto").unwrap(),
            "http://env:2"
        );
        assert_eq!(
            resolve_endpoint(None, None, "eu-west-1").unwrap(),
            "https://s3.eu-west-1.amazonaws.com"
        );
    }

    /// Region "auto" is what R2 wants, and it names no AWS endpoint — so
    /// guessing one would sign a request to Amazon with an R2 key and report a
    /// 403 that says nothing about the real mistake.
    #[test]
    fn region_auto_with_no_endpoint_is_refused_with_the_thing_to_do() {
        let err = resolve_endpoint(None, None, "auto")
            .unwrap_err()
            .to_string();
        assert!(err.contains("--endpoint-url"), "{err}");
        assert!(err.contains("r2.cloudflarestorage.com"), "{err}");
        assert!(err.contains("--region"), "{err}");
    }

    #[test]
    fn an_empty_endpoint_string_does_not_count_as_set() {
        assert_eq!(
            resolve_endpoint(Some(""), Some("http://env:2"), "auto").unwrap(),
            "http://env:2"
        );
        assert!(resolve_endpoint(Some(""), Some(""), "auto").is_err());
    }
}
