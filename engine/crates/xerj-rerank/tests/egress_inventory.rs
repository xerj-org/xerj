//! Every outbound connection the engine source can open, checked against the
//! list `docs/RERANK.md` publishes ("Every way data leaves a XERJ node").
//!
//! Why this exists: the first version of the rerank documentation told
//! operators — and, through `llms.txt` and the MCP tool descriptions, the
//! agents that talk to them — that reranking was "the ONE XERJ feature that
//! sends document text off the machine". Proxy embeddings and the WAL tap
//! already did. A privacy statement is only as good as the inventory under it,
//! so the inventory is a test: a new outbound client anywhere under
//! `engine/crates/*/src` fails here until it is classified below AND the
//! published list accounts for it.
//!
//! The scan is textual (it looks for the constructors of every network client
//! the workspace depends on), so it can over-report — that is the safe
//! direction. A file whose only use is test code is classified `TestOnly`, and
//! that claim is itself checked: every marker must sit after the file's first
//! `#[cfg(test)]`, or the whole file must be a module declared under
//! `#[cfg(test)]`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Constructors and entry points of outbound network clients. Types used only
/// for parsing (`reqwest::Url`) are deliberately absent.
const MARKERS: &[&str] = &[
    // Object storage: aws-sdk-s3 is already a workspace dependency and the
    // S3 backend/source PRs put it on the engine path. Without these the
    // inventory would stay green while a whole outbound client went unlisted.
    "aws_sdk_s3::",
    "aws_config::",
    "object_store::",
    "reqwest::Client",
    "reqwest::ClientBuilder",
    "reqwest::blocking::Client",
    "reqwest::get(",
    "reqwest::blocking::get(",
    "hf_hub::",
    "ureq::",
    "TcpStream::connect",
    "TcpSocket::",
    "UdpSocket::",
    "hyper::client",
    "hyper_util::client",
    "tonic::transport::Channel",
    "tonic::transport::Endpoint",
    "tungstenite",
    "lettre::",
    "isahc::",
    "attohttpc::",
    "surf::",
    "Command::new(\"gh\")",
    "Command::new(\"curl\")",
    "Command::new(\"wget\")",
    "Command::new(\"ssh\")",
];

#[derive(Debug, Clone, Copy)]
enum Role {
    /// The running node opens this connection. The string is the row label
    /// the published table must carry.
    Node(&'static str),
    /// A command-line client in the same binary. The string must appear in
    /// the published section.
    Client(&'static str),
    /// Only test code in this file opens a connection.
    TestOnly,
}

/// Paths are relative to `engine/crates`.
const KNOWN: &[(&str, Role)] = &[
    ("xerj-rerank/src/lib.rs", Role::Node("**Rerank provider**")),
    ("xerj-ai/src/embed.rs", Role::Node("**Proxy embeddings**")),
    ("xerj-engine/src/wal_tap.rs", Role::Node("**WAL tap**")),
    (
        "xerj-ai/src/neural.rs",
        Role::Node("**Neural model download**"),
    ),
    (
        "xerj-cluster/src/transport.rs",
        Role::Node("**Cluster transport**"),
    ),
    ("xerj-mcp/src/lib.rs", Role::Client("`xerj mcp`")),
    (
        "xerj-autoindex/src/esclient.rs",
        Role::Client("`xerj autoindex`"),
    ),
    (
        "xerj-autoindex/src/feedback.rs",
        Role::Client("`xerj feedback --open-pr`"),
    ),
    // Object storage, added by #966 (the backend) and #970 (`autoindex s3://`).
    // Both talk to whatever endpoint the operator names, so they belong on the
    // published list even though neither sends DOCUMENT TEXT anywhere: the
    // source reads objects in, and the backend is not on the segment path yet
    // (`storage.backend = "s3"` still refuses to start).
    (
        "xerj-storage/src/s3.rs",
        Role::Node("**Object storage backend**"),
    ),
    (
        "xerj-autoindex/src/objsource.rs",
        Role::Client("`xerj autoindex s3://`"),
    ),
    // The watcher polls the same bucket on an interval, so unlike a one-shot
    // run it keeps talking to the endpoint for as long as it is up.
    (
        "xerj-autoindex/src/objwatch/s3.rs",
        Role::Client("`xerj autoindex s3:// --watch`"),
    ),
    (
        "xerj-autoindex/src/objsource_minio_tests.rs",
        Role::TestOnly,
    ),
    ("xerj-autoindex/src/objsource_s3_tests.rs", Role::TestOnly),
    ("xerj-api/src/binary_protocol.rs", Role::TestOnly),
    ("xerj-server/src/grpc.rs", Role::TestOnly),
    ("xerj-server/src/main.rs", Role::TestOnly),
    (
        "xerj-autoindex/src/incremental_reconcile_http_tests.rs",
        Role::TestOnly,
    ),
    (
        "xerj-autoindex/src/failure_resume_http_tests.rs",
        Role::TestOnly,
    ),
    ("xerj-autoindex/src/detect/e2e.rs", Role::TestOnly),
];

/// Phrasings that were false and must not come back on a published surface.
const FALSE_CLAIMS: &[&str] = &[
    "the one xerj feature",
    "the one feature that sends",
    "one feature that sends document text",
    "nothing else in xerj sends",
    "no other request sends",
    "every other request — indexing, search, embedding",
    "none of which a search request can trigger",
    "not something a search request can trigger",
    "two other outbound paths",
    "the other two outbound paths",
    "the other outbound paths",
    "other operator-configured outbound paths",
];

/// The surfaces that carry the rerank egress statement.
const SURFACES: &[&str] = &[
    "docs/RERANK.md",
    // ROADMAP.md and ZERO_TOKEN_DIRECTION.md repeated the false claim for a
    // week after every other surface was fixed, because neither was scanned —
    // and landing/llms.txt sends agents straight to ROADMAP.md#the-zero-token-direction.
    "ROADMAP.md",
    "docs/ZERO_TOKEN_DIRECTION.md",
    "docs/ARCHITECTURE.md",
    "docs/recipes/air-gapped-deployment.md",
    "README.md",
    "CHANGELOG.md",
    "content/answers/rerank-search-results-calibrated-judge.md",
    "content/answers/hybrid-search-quality-measured-ndcg.md",
    "landing/answers/rerank-search-results-calibrated-judge.md",
    "landing/answers/rerank-search-results-calibrated-judge.html",
    "landing/llms.txt",
    "landing/llms-full.txt",
    "landing/openapi.json",
    "landing/docs/agents/schemas/mcp-tools.json",
    "landing/docs/agents/schemas/anthropic-tools.json",
    "landing/docs/agents/schemas/openai-tools.json",
    "landing/docs/api-es-compat.html",
    "landing/docs/config.html",
    "landing/docs/env.html",
    "landing/docs/recipes/air-gapped-deployment.html",
    "engine/xerj.default.toml",
    "engine/crates/xerj-common/src/config.rs",
    "engine/crates/xerj-api/src/rerank_stage.rs",
    "engine/crates/xerj-mcp/src/lib.rs",
    "engine/crates/xerj-rerank/src/lib.rs",
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .to_path_buf()
}

fn repo_root() -> PathBuf {
    crates_dir()
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The file with comment-only lines removed. A marker named in prose — a doc
/// comment saying "in production replace this with `aws_sdk_s3::Client`" — is
/// not an outbound client, and counting it would force a real entry in KNOWN
/// for a file that opens no connection. Lines with code before a trailing `//`
/// are kept whole, so a marker can never hide behind a comment.
fn code_of(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under `engine/crates/*/src` that names a network client,
/// keyed by its path relative to `engine/crates`.
fn files_with_outbound_clients() -> Vec<(String, String)> {
    let crates = crates_dir();
    let mut found = Vec::new();
    let mut crate_dirs: Vec<PathBuf> = std::fs::read_dir(&crates)
        .expect("read engine/crates")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("src").is_dir())
        .collect();
    crate_dirs.sort();
    for krate in crate_dirs {
        let mut files = Vec::new();
        rust_sources(&krate.join("src"), &mut files);
        files.sort();
        for file in files {
            let text = read(&file);
            if MARKERS.iter().any(|m| code_of(&text).contains(m)) {
                let rel = file
                    .strip_prefix(&crates)
                    .expect("under engine/crates")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push((rel, text));
            }
        }
    }
    found
}

/// Is `file` (relative to `engine/crates`) a module its parent declares under
/// `#[cfg(test)]`?
fn declared_test_only(rel: &str) -> bool {
    let path = crates_dir().join(rel);
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let dir = path.parent().unwrap();
    let parents = [dir.join("mod.rs"), dir.join("lib.rs"), dir.join("main.rs")];
    parents.iter().filter(|p| p.is_file()).any(|p| {
        let lines: Vec<String> = read(p).lines().map(|l| l.trim().to_string()).collect();
        lines.windows(2).any(|w| {
            w[0] == "#[cfg(test)]"
                && (w[1] == format!("mod {stem};")
                    || w[1] == format!("pub mod {stem};")
                    || w[1] == format!("pub(crate) mod {stem};"))
        })
    })
}

fn published_section() -> String {
    let doc = read(&repo_root().join("docs/RERANK.md"));
    let start = doc
        .find("### Every way data leaves a XERJ node")
        .expect("docs/RERANK.md has the 'Every way data leaves a XERJ node' section");
    let rest = &doc[start..];
    // The section runs to the next heading of the same or a higher level.
    let end = rest[4..]
        .find("\n## ")
        .into_iter()
        .chain(rest[4..].find("\n### "))
        .min()
        .map(|i| i + 4)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn every_outbound_client_in_the_engine_is_on_the_published_list() {
    let found = files_with_outbound_clients();
    let found_set: BTreeSet<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
    let known_set: BTreeSet<&str> = KNOWN.iter().map(|(p, _)| *p).collect();

    let unlisted: Vec<&&str> = found_set.difference(&known_set).collect();
    assert!(
        unlisted.is_empty(),
        "new outbound network client(s) in {unlisted:?}. Classify each in KNOWN \
         (node / client / test-only) and, if the node or a client can now send \
         data somewhere, add it to docs/RERANK.md \"Every way data leaves a XERJ \
         node\", to xerj_rerank::DATA_EGRESS and to the air-gapped recipe."
    );
    let stale: Vec<&&str> = known_set.difference(&found_set).collect();
    assert!(
        stale.is_empty(),
        "{stale:?} no longer open(s) a connection: remove the entry from KNOWN and \
         check whether the published list still describes the engine"
    );

    for (rel, text) in &found {
        let role = KNOWN.iter().find(|(p, _)| p == rel).unwrap().1;
        if let Role::TestOnly = role {
            if declared_test_only(rel) {
                continue;
            }
            let test_start = text.find("#[cfg(test)]").unwrap_or_else(|| {
                panic!("{rel} is classified test-only but has no #[cfg(test)] section")
            });
            for m in MARKERS {
                if let Some(at) = text.find(m) {
                    assert!(
                        at > test_start,
                        "{rel} is classified test-only, but `{m}` appears outside its \
                         test code: classify it as a node or client path and publish it"
                    );
                }
            }
        }
    }
}

#[test]
fn the_published_list_names_every_path() {
    let section = published_section();
    for (rel, role) in KNOWN {
        match role {
            Role::Node(label) | Role::Client(label) => assert!(
                section.contains(label),
                "docs/RERANK.md \"Every way data leaves a XERJ node\" does not mention \
                 {label} (from {rel})"
            ),
            Role::TestOnly => {}
        }
    }
    // The air-gapped recipe is the other place an operator looks.
    let recipe = read(&repo_root().join("docs/recipes/air-gapped-deployment.md"));
    for row in [
        "| External proxy |",
        "| WAL tap |",
        "| Neural model |",
        "| Cluster |",
        "| Rerank provider |",
    ] {
        assert!(recipe.contains(row), "air-gapped recipe lost its {row} row");
    }
}

#[test]
fn the_status_statement_names_every_node_path_and_the_docs_quote_it() {
    let s = xerj_rerank::DATA_EGRESS;
    for needle in [
        "only search-time feature",
        "default_endpoint",
        "WAL tap",
        "HuggingFace",
        "cluster mode",
    ] {
        assert!(
            s.contains(needle),
            "DATA_EGRESS does not mention {needle}: {s}"
        );
    }
    let doc = read(&repo_root().join("docs/RERANK.md"));
    assert!(
        doc.contains(s),
        "docs/RERANK.md's GET /_xerj/rerank example must quote DATA_EGRESS verbatim"
    );
}

#[test]
fn no_published_surface_repeats_a_false_egress_claim() {
    let root = repo_root();
    let mut hits = Vec::new();
    for surface in SURFACES {
        let path = root.join(surface);
        let Ok(text) = std::fs::read_to_string(&path) else {
            panic!("surface {surface} is missing — update SURFACES");
        };
        // Folded whitespace and case: the claims were wrapped across lines and
        // written in more than one case.
        let folded = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        for claim in FALSE_CLAIMS {
            // This file lists the phrases it forbids.
            if folded.contains(claim) && !surface.ends_with("egress_inventory.rs") {
                hits.push(format!("{surface}: \"{claim}\""));
            }
        }
    }
    assert!(hits.is_empty(), "false egress claims: {hits:#?}");
}
