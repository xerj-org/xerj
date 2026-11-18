//! Labelled routing accuracy test (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md`,
//! step 5).
//!
//! `tests/routing.jsonl` is a hand-written set of questions, each labelled with
//! the corpus that should answer it and the project dependencies an agent
//! asking it would plausibly have. The questions are phrased from the registry's
//! one-line `domain`/`ask_for` descriptions of ~15 real corpora
//! (`tools/xerj-code/hub/backlog/backlog-100.json`); they are written by hand,
//! not generated from the sketches.
//!
//! This test builds a small [`Catalogue`] — one entry per referenced corpus,
//! each with a [`Sketch`](xerj_hub::Sketch) built over terms taken from that
//! corpus's description — and routes every question twice: once with the line's
//! dependencies present in the working directory (a synthetic `Cargo.lock`), and
//! once with none. It asserts top-1 accuracy (a `Confident` verdict for the
//! expected slug, or the expected slug first in an `Ambiguous` list) against the
//! plan's targets (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md` §5): at least 90% with
//! dependencies and 80% without.
//!
//! Only the crate's public API is used, so this exercises the router exactly as
//! the CLI will.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;
use xerj_hub::router::{route, Routing};
use xerj_hub::{Catalogue, CatalogueEntry, LicenceClass};

/// `(slug, description, dependency_names)` for every corpus the fixture labels.
/// The description is the registry's `domain`/`ask_for` sentence plus its topic
/// tags; the sketch for each corpus is built from it. The dependency names are
/// the real crate / module / package names each corpus answers for.
const CORPORA: &[(&str, &str, &[&str])] = &[
    (
        "lz4",
        "an agent needing fast block compression queries this for the block format and decoder loop. compression networking",
        &["lz4", "lz4-sys", "lz4_flex"],
    ),
    (
        "zstd",
        "an agent tuning compression queries this for frame block formats window and level internals. compression networking",
        &["zstd", "zstd-sys", "zstd-safe"],
    ),
    (
        "brotli",
        "an agent working on web content encoding queries this for the static dictionary and transform handling. compression networking",
        &["brotli", "brotli-decompressor"],
    ),
    (
        "rustls",
        "an agent implementing TLS in Rust queries this for the state machine cipher dispatch and certificate verification. tls networking",
        &["rustls", "tokio-rustls"],
    ),
    (
        "quinn",
        "an agent implementing QUIC queries this for congestion control stream state and 0-RTT handling in idiomatic Rust. quic networking",
        &["quinn", "quinn-proto"],
    ),
    (
        "blake3",
        "an agent implementing fast cryptographic hashing or content-addressed digests queries this for the BLAKE3 tree-hash construction SIMD lanes and keyed derive-key modes. hashing blake3 simd content-addressing kdf",
        &["blake3"],
    ),
    (
        "curve25519-dalek",
        "an agent working on elliptic-curve or signature crypto in Rust queries this for the dalek curve25519 ed25519 x25519 field-arithmetic and group-operation code. curve25519 ed25519 x25519 ristretto rust",
        &["curve25519-dalek", "ed25519-dalek", "x25519-dalek"],
    ),
    (
        "polars",
        "polars rust dataframe query engine columnar arrow olap lazy eager execution",
        &["polars", "polars-core", "polars-lazy"],
    ),
    (
        "datafusion",
        "datafusion rust sql query engine arrow olap execution planner",
        &["datafusion", "datafusion-expr"],
    ),
    (
        "rocksdb",
        "an agent working on LSM stores queries this for compaction WAL and recovery internals at production scale. storage data-engines",
        &["rocksdb", "librocksdb-sys"],
    ),
    (
        "aiohttp",
        "an agent working on an async HTTP client server stack queries this for streaming bodies websocket upgrades and connection reuse. http async python websocket",
        &["aiohttp"],
    ),
    (
        "miekg-dns",
        "miekg dns dnssec library resolver records zone go",
        &["github.com/miekg/dns"],
    ),
    (
        "candle",
        "a rust engine adding embeddings model inference queries this for a minimalist rust tensor ml framework and model implementations. tensors inference embeddings",
        &["candle-core", "candle-nn", "candle-transformers"],
    ),
    (
        "raft-rs",
        "an agent implementing raft queries this for the log ready progress state machine the tikv fleet runs on. consensus data-engines",
        &["raft"],
    ),
    (
        "arrow-rs",
        "an agent working on columnar memory queries this for the Rust arrow arrays kernels and IPC paths. columnar data-engines",
        &["arrow", "arrow-array", "arrow-flight"],
    ),
];

/// Lowercase words of 3+ characters in `s` (splitting on every non-alphanumeric
/// char), the terms a corpus's sketch is built from.
fn words_ge3(s: &str) -> Vec<String> {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect()
}

fn make_catalogue() -> Catalogue {
    let corpora = CORPORA
        .iter()
        .map(|(slug, description, deps)| {
            let mut terms = words_ge3(description);
            terms.extend(words_ge3(slug));
            CatalogueEntry {
                slug: (*slug).to_string(),
                domain: "TEST".to_string(),
                topics: Vec::new(),
                ask_for: (*description).to_string(),
                upstream: String::new(),
                pin_sha: "0".repeat(40),
                pin_date: "2026-01-01".to_string(),
                refresh_cadence_days: 7,
                licence_spdx: "Apache-2.0".to_string(),
                licence_class: LicenceClass::AdaptWithAttribution,
                pack_bytes: 0,
                pack_url: String::new(),
                pack_sha256: String::new(),
                sig_url: String::new(),
                dependency_names: deps.iter().map(|d| (*d).to_string()).collect(),
                quality_grade: None,
                term_sketch: xerj_hub::sketch::build(terms),
            }
        })
        .collect();
    Catalogue {
        format_version: 1,
        generated_at: "2026-01-01T00:00:00Z".to_string(),
        corpora,
    }
}

// --- a tiny JSONL reader (serde_json is not a dev-dependency) ----------------

struct Case {
    q: String,
    deps: Vec<String>,
    expected: String,
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/routing.jsonl")
}

fn load_fixture() -> Vec<Case> {
    let text = fs::read_to_string(fixture_path()).expect("read routing.jsonl");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| Case {
            q: json_string(line, "q").expect("line has a q field"),
            deps: json_string_array(line, "deps"),
            expected: json_string(line, "expected").expect("line has an expected field"),
        })
        .collect()
}

/// Value of a `"key": "..."` string field. Handles `\"` and `\\` escapes; the
/// fixture is plain, but the reader stays honest about them.
fn json_string(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = line.find(&needle)? + needle.len();
    let after_colon = line[start..].find(':')? + start + 1;
    let open = line[after_colon..].find('"')? + after_colon + 1;
    let mut out = String::new();
    let mut chars = line[open..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None
}

/// Values of a `"key": ["a", "b"]` string-array field (empty if absent/empty).
fn json_string_array(line: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let Some(k) = line.find(&needle) else {
        return Vec::new();
    };
    let rest = &line[k + needle.len()..];
    let Some(open) = rest.find('[') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find(']') else {
        return Vec::new();
    };
    rest[open + 1..open + close]
        .split(',')
        .map(|part| part.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// --- harness -----------------------------------------------------------------

/// A git root with the given project dependencies written as a `Cargo.lock`
/// (empty `deps` → no lockfile at all).
fn project_dir(deps: &[String]) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    if !deps.is_empty() {
        let mut lock = String::from("version = 3\n");
        for name in deps {
            lock.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\n"
            ));
        }
        fs::write(dir.path().join("Cargo.lock"), lock).unwrap();
    }
    dir
}

/// Did the routing put `expected` first? (Confident for it, or first candidate
/// of an Ambiguous list.)
fn top1_ok(routing: &Routing, expected: &str) -> bool {
    match routing {
        Routing::Confident(slug) => slug == expected,
        // A tight group counts when the expected corpus is the one it leads with.
        Routing::Group(slugs) | Routing::Ambiguous(slugs) => {
            slugs.first().map(|s| s == expected).unwrap_or(false)
        }
        Routing::NoCorpus | Routing::NoCoverage => false,
    }
}

#[test]
fn routing_fixture_meets_accuracy_targets() {
    let cat = make_catalogue();
    let cases = load_fixture();
    assert!(
        cases.len() >= 40,
        "expected ~40 labelled questions, got {}",
        cases.len()
    );

    let mut ok_with_deps = 0usize;
    let mut ok_without_deps = 0usize;
    for case in &cases {
        let with = project_dir(&case.deps);
        if top1_ok(
            &route(&case.q, with.path(), &cat, None, &[]),
            &case.expected,
        ) {
            ok_with_deps += 1;
        }

        let without = project_dir(&[]);
        if top1_ok(
            &route(&case.q, without.path(), &cat, None, &[]),
            &case.expected,
        ) {
            ok_without_deps += 1;
        }
    }

    let n = cases.len();
    let acc_with = ok_with_deps as f64 / n as f64;
    let acc_without = ok_without_deps as f64 / n as f64;
    eprintln!(
        "routing top-1 accuracy: with deps {ok_with_deps}/{n} = {:.1}%, without deps {ok_without_deps}/{n} = {:.1}%",
        acc_with * 100.0,
        acc_without * 100.0
    );

    assert!(
        acc_with >= 0.90,
        "with-deps top-1 accuracy {:.1}% is below the 90% target",
        acc_with * 100.0
    );
    assert!(
        acc_without >= 0.80,
        "without-deps top-1 accuracy {:.1}% is below the 80% target",
        acc_without * 100.0
    );
}

/// Issue #2179: a Phaser question must not exit as an ambiguous tie when no
/// corpus covers that project. A real tie (every query word is in two corpora,
/// and the score clears the good-match bar) stays ambiguous. An exact slug
/// still routes.
#[test]
fn no_fit_is_no_coverage_a_real_tie_stays_ambiguous_and_a_slug_routes() {
    let proj = project_dir(&[]);

    // Weak overlap: five tokens, two hits each, score 0.32. Not a choice.
    let mut weak = vec![
        corpus_entry("alpha", "", &["aaa", "bbb"]),
        corpus_entry("beta", "", &["aaa", "ccc"]),
    ];
    for i in 0..6 {
        weak.push(corpus_entry(
            &format!("other{i}"),
            "",
            &[&format!("uniq{i}a"), &format!("uniq{i}b")],
        ));
    }
    assert_eq!(
        route(
            "aaa bbb ccc noisea noiseb",
            proj.path(),
            &catalogue_of(weak),
            None,
            &[]
        ),
        Routing::NoCoverage
    );

    // The question names Phaser. Four corpora mention HTML and nothing names
    // Phaser; that used to be "multiple corpus match".
    let mut html = Vec::new();
    for slug in ["whatwg-html", "lol-html", "parse5", "jsdom"] {
        html.push(corpus_entry(
            slug,
            "An agent writing HTML queries this for the document format.",
            &[],
        ));
    }
    for i in 0..(151 - html.len()) {
        html.push(corpus_entry(
            &format!("filler-{i}"),
            "an agent working on storage queries this for internals",
            &[],
        ));
    }
    let html_cat = catalogue_of(html);
    assert_eq!(
        route(
            "How does Phaser draw HTML?",
            proj.path(),
            &html_cat,
            None,
            &[]
        ),
        Routing::NoCoverage,
        "a project no corpus contains is not an ambiguous tie"
    );
    match route("HTML document format", proj.path(), &html_cat, None, &[]) {
        Routing::Ambiguous(slugs) => assert!(
            slugs.len() >= 2 && slugs.iter().any(|s| s == "whatwg-html"),
            "{slugs:?}"
        ),
        other => panic!("expected a real HTML tie, got {other:?}"),
    }

    // Exact slug.
    let named = catalogue_of(vec![
        corpus_entry(
            "lz4",
            "an agent needing fast block compression queries this for the decoder",
            &["lz4", "block", "decoder"],
        ),
        corpus_entry(
            "zstd",
            "an agent tuning compression queries this for frames",
            &["zstd", "frame"],
        ),
    ]);
    assert_eq!(
        route(
            "how does the lz4 decoder work",
            proj.path(),
            &named,
            None,
            &[]
        ),
        Routing::Confident("lz4".into())
    );

    // Both query words sit in two corpora. Common-token weighting scores the
    // tie at 0.4, which is still a real ambiguity.
    let tie = catalogue_of(vec![
        corpus_entry("alpha", "", &["aaa", "bbb", "ccc", "ddd"]),
        corpus_entry("beta", "", &["aaa", "bbb", "eee", "fff"]),
        corpus_entry("gamma", "", &["server", "socket", "packet", "router"]),
        corpus_entry("delta", "", &["ledger", "balance", "invoice", "levy"]),
    ]);
    match route("aaa bbb", proj.path(), &tie, None, &[]) {
        Routing::Ambiguous(slugs) => {
            assert!(slugs.contains(&"alpha".to_string()) && slugs.contains(&"beta".to_string()));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

fn corpus_entry(slug: &str, ask_for: &str, terms: &[&str]) -> CatalogueEntry {
    CatalogueEntry {
        slug: slug.to_string(),
        domain: "TEST".to_string(),
        topics: Vec::new(),
        ask_for: ask_for.to_string(),
        upstream: String::new(),
        pin_sha: "0".repeat(40),
        pin_date: "2026-01-01".to_string(),
        refresh_cadence_days: 7,
        licence_spdx: "Apache-2.0".to_string(),
        licence_class: LicenceClass::AdaptWithAttribution,
        pack_bytes: 0,
        pack_url: String::new(),
        pack_sha256: String::new(),
        sig_url: String::new(),
        dependency_names: Vec::new(),
        quality_grade: None,
        term_sketch: xerj_hub::sketch::build(terms.iter().map(|t| (*t).to_string())),
    }
}

fn catalogue_of(corpora: Vec<CatalogueEntry>) -> Catalogue {
    Catalogue {
        format_version: 1,
        generated_at: "2026-01-01T00:00:00Z".to_string(),
        corpora,
    }
}

#[test]
fn every_expected_slug_is_in_the_catalogue() {
    let slugs: Vec<&str> = CORPORA.iter().map(|(s, _, _)| *s).collect();
    for case in load_fixture() {
        assert!(
            slugs.contains(&case.expected.as_str()),
            "fixture references {:?}, which is not in the test catalogue",
            case.expected
        );
    }
}
