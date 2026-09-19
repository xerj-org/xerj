//! End-to-end mail ingest over the synthetic Google Takeout fixture: a real
//! `run_index` against the fake ES endpoint, checked against the GROUND TRUTH
//! the generator wrote — not against numbers typed into this file.
//!
//! The fixture is `tests/fixtures/takeout-small/`, produced by
//! `scripts/synthetic-takeout.py` with [`FIXTURE_ARGS`]. It is committed so
//! this test never depends on Python and is never skipped;
//! [`the_committed_fixture_is_what_the_generator_writes`] is the separate check
//! that the two have not drifted apart.
//!
//! ## What this cannot see
//! PDF extraction runs in an isolated worker process — `current_exe()
//! __extract-pdf` — and inside a unit test `current_exe()` is the TEST binary,
//! which has no such subcommand. So here every PDF attachment takes the
//! fallback path (a name card, counted as junk). That path is real and worth
//! pinning: it is what a scanned or corrupt PDF gets. PDF PAGE records from
//! mbox attachments are verified against the real `xerj` binary on a live
//! node by `benchmarks/mbox-ingest/verify.py`, and the result is recorded in
//! that directory's README.

use super::e2e::{cfg, journal_graph_summary, Docs, MockEs};
use super::emailthread::{ATTACHMENT_OF, REPLIES_TO, TAG};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const EDGES_INDEX: &str = ".xerj-memory-notes-edges";
const MBOX_REL: &str = "Takeout/Mail/All mail Including Spam and Trash.mbox";

/// The generator arguments the committed fixture was written with. Small
/// blobs and a short "long line" keep the tree under 300 KB; `--ascii-names`
/// keeps FILE names portable across every OS that checks the repo out (the
/// message CONTENT is non-ASCII regardless).
const FIXTURE_ARGS: &[&str] = &[
    "--seed",
    "42",
    "--messages",
    "36",
    "--blob-max",
    "2K",
    "--long-line-bytes",
    "3K",
    "--keep-notes",
    "4",
    "--ascii-names",
    "--with-archive",
    "--quiet",
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/takeout-small")
}

fn truth() -> Value {
    serde_json::from_slice(&std::fs::read(fixture().join("truth.json")).unwrap()).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// Pins the PDF worker to a binary that does not exist for the life of the
/// guard, under the crate-wide env lock. Without this the outcome of every PDF
/// attachment depends on which sibling test last set `XERJ_PDF_WORKER_BIN`
/// (some point it at a stub that answers for ANY path).
struct NoPdfWorker {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl NoPdfWorker {
    fn pin() -> Self {
        let lock = crate::extract::pdf::WORKER_BIN_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = std::env::var_os("XERJ_PDF_WORKER_BIN");
        std::env::set_var("XERJ_PDF_WORKER_BIN", "/nonexistent/xerj-pdf-worker");
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for NoPdfWorker {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(v) => std::env::set_var("XERJ_PDF_WORKER_BIN", v),
            None => std::env::remove_var("XERJ_PDF_WORKER_BIN"),
        }
    }
}

struct Indexed {
    /// Node docs of every dataset index: `_id` → source.
    nodes: BTreeMap<String, Value>,
    edges: BTreeMap<String, Value>,
    catalog: BTreeMap<String, Value>,
    graph: Value,
}

fn split(docs: &Docs, state: &Path) -> Indexed {
    let mut nodes = BTreeMap::new();
    for (index, store) in docs {
        if index.starts_with("ax-") {
            nodes.extend(store.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    Indexed {
        nodes,
        edges: docs.get(EDGES_INDEX).cloned().unwrap_or_default(),
        catalog: docs.get("autoindex-catalog").cloned().unwrap_or_default(),
        graph: journal_graph_summary(state),
    }
}

fn index_fixture() -> (Indexed, MockEs, tempfile::TempDir, tempfile::TempDir) {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    copy_tree(&fixture().join("tree"), corpus.path());
    let es = MockEs::start();
    // 3 = completed-with-junk: the two unextracted archives are refused by
    // design, and that is exactly what this exit code reports.
    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        3
    );
    let indexed = split(&es.docs.lock().unwrap(), state.path());
    (indexed, es, corpus, state)
}

fn s<'a>(doc: &'a Value, key: &str) -> &'a str {
    doc.get(key).and_then(Value::as_str).unwrap_or("")
}

fn mbox_records(ix: &Indexed) -> Vec<(&String, &Value)> {
    ix.nodes
        .iter()
        .filter(|(_, d)| {
            s(d, "ax_path") == MBOX_REL && s(d, "ax_locator") != super::FILE_CARD_LOCATOR
        })
        .collect()
}

#[test]
fn a_takeout_mailbox_indexes_to_what_the_ground_truth_says() {
    let _pdf = NoPdfWorker::pin();
    let truth = truth();
    let (ix, _es, _corpus, _state) = index_fixture();
    let recs = mbox_records(&ix);
    let n = |key: &str| truth[key].as_u64().unwrap() as usize;

    // ── every entry of the mailbox is accounted for ──
    // One node per message (`…-msg-s0`), or `…-raw-s0` for an entry the MIME
    // parser refused; the separator-only entry has no text and is junk.
    let heads: Vec<&str> = recs
        .iter()
        .map(|(_, d)| s(d, "ax_locator"))
        .filter(|l| l.ends_with("-msg-s0") || l.ends_with("-raw-s0"))
        .collect();
    assert_eq!(
        heads.len(),
        n("entries") - n("entries_empty"),
        "every entry but the separator-only one, which is junk: {heads:?}"
    );
    assert!(
        heads.iter().all(|l| l.starts_with('m')),
        "every locator is namespaced by byte offset: {heads:?}"
    );
    let unique: BTreeSet<&&str> = heads.iter().collect();
    assert_eq!(
        unique.len(),
        heads.len(),
        "offsets are unique within the file"
    );
    for (_, d) in &recs {
        assert_eq!(s(d, "ax_format"), "mbox");
    }

    // ── the unquoted prose `From ` line did NOT split its message ──
    let prose = recs
        .iter()
        .find(|(_, d)| s(d, "body").contains("From what I understand, the deal closes Monday"))
        .expect("the prose line is body text of some record");
    let after = truth["needles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|nd| nd["where"] == "after-unquoted-prose-from")
        .unwrap();
    assert!(
        s(prose.1, "body").contains(after["token"].as_str().unwrap()),
        "the sentence after the prose `From ` line is in the SAME record"
    );
    assert_eq!(
        s(prose.1, "email_message_id"),
        after["message_id"].as_str().unwrap()
    );

    // ── mboxrd quoting is undone by exactly one level ──
    let line_starts = |needle: &str| {
        recs.iter()
            .filter(|(_, d)| s(d, "body").lines().any(|l| l.starts_with(needle)))
            .count()
    };
    let from_lines = ["From the desk of", "From what I can tell", "From now on,"];
    let unquoted: usize = from_lines.iter().map(|f| line_starts(f)).sum();
    assert_eq!(
        unquoted,
        n("from_lines_in_bodies"),
        "`>From ` came back as `From `"
    );
    let still_quoted: usize = from_lines
        .iter()
        .map(|f| line_starts(&format!(">{f}")))
        .sum();
    assert_eq!(still_quoted, 0, "no `>From the desk…` left behind");
    assert_eq!(
        line_starts(">From the archive"),
        n("quoted_from_lines_in_bodies"),
        "`>>From ` lost ONE `>`, not both"
    );

    // ── every planted needle is found exactly once, where it was planted ──
    let all: Vec<&Value> = ix.nodes.values().collect();
    for nd in truth["needles"].as_array().unwrap() {
        let (token, place) = (nd["token"].as_str().unwrap(), nd["where"].as_str().unwrap());
        let hits: Vec<&&Value> = all
            .iter()
            .filter(|d| s(d, "body").contains(token))
            .collect();
        if place == "pdf-attachment" {
            // See the module docs: no PDF worker inside a unit test.
            assert!(
                hits.is_empty(),
                "{token}: PDF text cannot be extracted here"
            );
            continue;
        }
        assert_eq!(
            hits.len(),
            1,
            "{place} needle {token} must be found exactly once"
        );
        let hit = hits[0];
        match place {
            // 8-bit bodies: declared latin-1, and UNDECLARED cp1252, both decoded.
            "latin1-8bit-body" => assert!(s(hit, "body").contains("Grüße aus Köln"), "{hit}"),
            "cp1252-undeclared-body" => {
                assert!(
                    s(hit, "body").contains("\u{201c}Quoted\u{201d} price: \u{20ac}420"),
                    "{hit}"
                )
            }
            "text-attachment" => {
                assert!(s(hit, "ax_locator").contains("-att"), "{hit}");
                assert!(!s(hit, "attachment_name").is_empty());
                assert_eq!(
                    s(hit, "email_message_id"),
                    nd["message_id"].as_str().unwrap()
                );
            }
            // Exactly once is the point: the `.html` twin of each note is skipped.
            "keep-note" => {
                assert!(s(hit, "ax_path").starts_with("Takeout/Keep/"));
                assert!(s(hit, "ax_path").ends_with(".json"));
                assert_eq!(s(hit, "ax_locator"), "note-s0");
                assert!(
                    hit.get("keep_trashed").is_some(),
                    "a document, with Keep's fields"
                );
            }
            "drive-markdown" => assert_eq!(s(hit, "ax_path"), "Takeout/Drive/meeting-notes.md"),
            "body" | "after-unquoted-prose-from" => {
                assert_eq!(
                    s(hit, "email_message_id"),
                    nd["message_id"].as_str().unwrap()
                )
            }
            other => panic!("the generator plants a kind this test does not check: {other}"),
        }
    }

    // ── Gmail's headers became fields ──
    let labelled = recs
        .iter()
        .filter(|(_, d)| d.get("email_labels").is_some())
        .count();
    assert!(
        labelled >= n("messages_regular"),
        "every regular message carries its labels"
    );
    for (_, d) in recs
        .iter()
        .filter(|(_, d)| d.get("email_thread_id").is_some())
    {
        let id = s(d, "email_thread_id");
        assert!(
            id.len() <= 16 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "thread id is hex, never a digit string that coerces to a long: {id}"
        );
    }
    // A non-ASCII subject arrives decoded, not as `=?UTF-8?B?…?=`. (The one
    // deliberately BROKEN encoded-word, with invalid UTF-8 inside it, is left
    // as the sender wrote it — garbage in, the same garbage out, no panic.)
    assert!(recs
        .iter()
        .filter(|(_, d)| !s(d, "email_message_id").starts_with("badword-"))
        .all(|(_, d)| !s(d, "email_subject").contains("=?UTF-8?")));
    assert!(recs.iter().any(|(_, d)| !s(d, "email_subject").is_ascii()));
}

#[test]
fn thread_and_attachment_edges_match_the_ground_truth_and_carry_evidence() {
    let _pdf = NoPdfWorker::pin();
    let truth = truth();
    let (ix, _es, _corpus, _state) = index_fixture();
    let n = |key: &str| truth[key].as_u64().unwrap();

    let mail: Vec<&Value> = ix
        .edges
        .values()
        .filter(|e| s(e, "detector") == TAG)
        .collect();
    let replies: Vec<&&Value> = mail.iter().filter(|e| s(e, "type") == REPLIES_TO).collect();
    let attached: Vec<&&Value> = mail
        .iter()
        .filter(|e| s(e, "type") == ATTACHMENT_OF)
        .collect();
    assert_eq!(
        replies.len() + attached.len(),
        mail.len(),
        "two edge types, nothing else"
    );

    // replies_to: one per reply whose parent — or nearest References ancestor —
    // is in the mailbox. The generator knows how many that is.
    assert_eq!(replies.len() as u64, n("replies_resolvable"));
    let via_refs = replies
        .iter()
        .filter(|e| s(&e["evidence"], "quote").starts_with("References: <"))
        .count() as u64;
    assert_eq!(
        via_refs,
        n("replies_via_references"),
        "resolved through References, and SAYS so"
    );
    for e in &replies {
        let quote = s(&e["evidence"], "quote");
        assert!(
            quote.starts_with("In-Reply-To: <") || quote.starts_with("References: <"),
            "evidence names the header and the id: {quote}"
        );
        for end in ["src", "dst"] {
            let node = ix
                .nodes
                .get(s(e, end))
                .unwrap_or_else(|| panic!("{end} of {e} is a ghost"));
            assert!(
                s(node, "ax_locator").ends_with("-msg-s0"),
                "{end} is a message node"
            );
        }
        assert_ne!(s(e, "src"), s(e, "dst"));
        assert_eq!(s(e, "src_file"), MBOX_REL);
    }
    // A parent that is not in the mailbox is COUNTED, never invented; the
    // duplicated Message-ID is counted as ambiguous.
    assert_eq!(
        ix.graph["edges_unresolved"].as_u64().unwrap(),
        n("replies_dangling")
    );
    assert_eq!(ix.graph["edges_ambiguous"], serde_json::json!(1));
    assert_eq!(
        ix.graph["by_detector"][TAG].as_u64().unwrap(),
        mail.len() as u64
    );

    // attachment_of: exactly one per attachment RECORD, to the message in the
    // same container slot.
    let att_records: BTreeMap<&String, &Value> = ix
        .nodes
        .iter()
        .filter(|(_, d)| d.get("attachment_name").is_some())
        .collect();
    assert_eq!(attached.len(), att_records.len());
    let total_attachments: u64 = truth["attachments"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert!(
        att_records.len() as u64 >= total_attachments,
        "at least one record per attachment"
    );
    for e in &attached {
        let src = att_records
            .get(&s(e, "src").to_string())
            .expect("src is an attachment record");
        let dst = ix.nodes.get(s(e, "dst")).expect("dst exists");
        let slot = |d: &Value| {
            s(d, "ax_locator")
                .split('-')
                .next()
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(slot(src), slot(dst), "same mbox slot (m<offset>)");
        assert!(s(dst, "ax_locator").ends_with("-msg-s0"));
        assert_eq!(s(src, "email_message_id"), s(dst, "email_message_id"));
        let quote = s(&e["evidence"], "quote");
        assert!(quote.starts_with("attachment \""), "{quote}");
        assert!(quote.contains(s(src, "attachment_name")), "{quote}");
    }
    // The PDFs (cards here — see the module docs) stay findable by name.
    let pdf_cards = att_records
        .values()
        .filter(|d| {
            s(d, "attachment_name").ends_with(".pdf") && s(d, "ax_locator").ends_with("-card")
        })
        .count() as u64;
    assert_eq!(
        pdf_cards,
        truth["attachments"]["pdf"].as_u64().unwrap()
            + truth["attachments_malformed"]["pdf"].as_u64().unwrap(),
        "every PDF, the truncated one in the malformed block included"
    );
}

#[test]
fn takeout_noise_is_skipped_and_archives_say_extract_me_first() {
    let _pdf = NoPdfWorker::pin();
    let truth = truth();
    let (ix, _es, _corpus, state) = index_fixture();

    let paths: BTreeSet<&str> = ix.nodes.values().map(|d| s(d, "ax_path")).collect();
    assert!(
        !paths.iter().any(|p| p.ends_with("archive_browser.html")),
        "{paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|p| p.starts_with("Takeout/Keep/") && p.ends_with(".html")),
        "Keep html twins are skipped: {paths:?}"
    );
    let notes = ix
        .nodes
        .values()
        .filter(|d| s(d, "ax_locator") == "note-s0")
        .count() as u64;
    assert_eq!(notes, truth["keep_notes"].as_u64().unwrap());

    for archive in truth["archives"].as_array().unwrap() {
        let name = archive.as_str().unwrap();
        let row = ix
            .catalog
            .values()
            .find(|d| s(d, "path") == name)
            .unwrap_or_else(|| panic!("{name} must be in the catalog, not silently gone"));
        assert_eq!(s(row, "status"), "junk");
        let reason = s(row, "reason");
        assert!(reason.contains("extract it first"), "{name}: {reason}");
        assert!(
            reason.contains("does not open archives"),
            "{name}: {reason}"
        );
        assert!(!paths.contains(name), "{name} was not indexed as content");
    }
    let tgz = ix
        .catalog
        .values()
        .find(|d| s(d, "path").ends_with(".tgz"))
        .unwrap();
    assert!(
        s(tgz, "reason").contains("tar -xzf"),
        "a .tgz gets the tar command, not unzip"
    );

    // …and the RUN says so, not only the catalog (review finding on PR #949: a
    // folder holding just the Takeout .zip ended `ok=true exit=3`, 0 records,
    // and never mentioned extracting). The run summary names every archive
    // with its command; the human summary prints the same lines.
    let journal = std::fs::read_to_string(state.path().join("journal.ndjson")).unwrap();
    let summary = journal
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .rfind(|v| v.get("kind").and_then(Value::as_str) == Some("finish"))
        .map(|v| v["summary"].clone())
        .unwrap();
    let said: Vec<&str> = summary["unextracted_archives"]
        .as_array()
        .expect("the run summary lists unextracted archives")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for archive in truth["archives"].as_array().unwrap() {
        let name = archive.as_str().unwrap();
        assert!(
            said.iter()
                .any(|line| line.starts_with(name) && line.contains("extract it first")),
            "{name} missing from the run's own summary: {said:?}"
        );
    }
}

/// Same bytes → same ids. A second run over the same state, and a run from a
/// brand-new state directory, must both leave the node set exactly as it was.
#[test]
fn rerunning_never_duplicates_a_message() {
    let _pdf = NoPdfWorker::pin();
    let (first, es, corpus, state) = index_fixture();
    let ids = |ix: &Indexed| ix.nodes.keys().cloned().collect::<BTreeSet<String>>();
    let mail_edges = |ix: &Indexed| {
        ix.edges
            .iter()
            .filter(|(_, e)| s(e, "detector") == TAG)
            .map(|(k, _)| k.clone())
            .collect::<BTreeSet<String>>()
    };

    assert_eq!(
        crate::run_index(cfg(corpus.path(), state.path(), &es.url)).unwrap(),
        3
    );
    let again = split(&es.docs.lock().unwrap(), state.path());
    assert_eq!(ids(&again), ids(&first), "incremental re-run");

    let fresh_state = tempfile::tempdir().unwrap();
    assert_eq!(
        crate::run_index(cfg(corpus.path(), fresh_state.path(), &es.url)).unwrap(),
        3
    );
    let fresh = split(&es.docs.lock().unwrap(), fresh_state.path());
    assert_eq!(
        ids(&fresh),
        ids(&first),
        "full re-index from an empty state dir"
    );
    assert_eq!(
        mail_edges(&fresh),
        mail_edges(&first),
        "edge ids are deterministic too"
    );
}

// ─── runs that do not re-read every mailbox (review finding, PR #949) ─────

/// One mbox entry. Fixed-width on purpose: every message of a tree built from
/// these is the same length, so removing one moves each later message onto the
/// byte offset — and therefore the node id — of its successor. That is the
/// nastiest form of the compaction defect: not a dangling edge, a WRONG one.
fn entry(id: &str, from: &str, subject: &str, body: &str, reply_to: Option<&str>) -> String {
    let mut m = format!(
        "From {from} Tue Nov 14 22:13:20 2023\nFrom: {from}\nTo: bob@example.org\n\
         Subject: {subject}\nDate: Tue, 14 Nov 2023 22:13:20 +0000\nMessage-ID: <{id}>\n"
    );
    if let Some(parent) = reply_to {
        m.push_str(&format!("In-Reply-To: <{parent}>\n"));
    }
    m.push_str(&format!("\n{body}\n\n"));
    m
}

/// Live `replies_to` edges as (file that taught it, evidence quote, dst node).
fn live_replies(es: &MockEs) -> BTreeSet<(String, String, String)> {
    es.index(EDGES_INDEX)
        .values()
        .filter(|e| s(e, "type") == REPLIES_TO && e.get("invalid_at").is_none())
        .map(|e| {
            (
                s(e, "src_file").to_string(),
                e.pointer("/evidence/quote")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                s(e, "dst").to_string(),
            )
        })
        .collect()
}

/// Node id of the message with this Message-ID, from the published docs.
fn node_of(es: &MockEs, message_id: &str) -> String {
    let docs = es.docs.lock().unwrap();
    let mut found = docs
        .iter()
        .filter(|(index, _)| index.starts_with("ax-"))
        .flat_map(|(_, store)| store.iter())
        .filter(|(_, d)| {
            s(d, "email_message_id") == message_id && s(d, "ax_locator").ends_with("msg-s0")
        })
        .map(|(id, _)| id.clone());
    let id = found
        .next()
        .unwrap_or_else(|| panic!("no node for <{message_id}>"));
    assert!(found.next().is_none(), "<{message_id}> indexed twice");
    id
}

fn run_ok(corpus: &Path, state: &Path, es: &MockEs) {
    assert_eq!(crate::run_index(cfg(corpus, state, &es.url)).unwrap(), 0);
}

/// `Inbox` + `Sent` is how Thunderbird, Apple Mail and mutt store mail, and a
/// conversation crosses the two files on every turn. An incremental run
/// re-reads only the file that changed; the reply edges must not depend on
/// which one that was.
///
/// Before the fix: appending one unrelated message to `Inbox` invalidated
/// `Inbox`'s edges (correct), re-read `Inbox` alone, could not see `<s2@x>` in
/// the un-re-read `Sent`, and the Inbox → Sent reply edge was gone — 2 live
/// edges became 1, with `unresolved: 1`, while both messages were still there.
#[test]
fn a_reply_across_two_mailboxes_survives_an_incremental_run() {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mail = corpus.path().join("Mail");
    std::fs::create_dir_all(&mail).unwrap();
    let inbox = entry(
        "a1@x",
        "alice@example.org",
        "Budget",
        "how much is left",
        None,
    ) + &entry(
        "a3@x",
        "alice@example.org",
        "Re: Budget",
        "thanks",
        Some("s2@x"),
    );
    std::fs::write(mail.join("Inbox"), &inbox).unwrap();
    std::fs::write(
        mail.join("Sent"),
        entry(
            "s2@x",
            "bob@example.org",
            "Re: Budget",
            "about four thousand",
            Some("a1@x"),
        ),
    )
    .unwrap();
    let es = MockEs::start();
    run_ok(corpus.path(), state.path(), &es);

    let expected = |es: &MockEs| -> BTreeSet<(String, String, String)> {
        [
            ("Mail/Inbox", "In-Reply-To: <s2@x>", node_of(es, "s2@x")),
            ("Mail/Sent", "In-Reply-To: <a1@x>", node_of(es, "a1@x")),
        ]
        .into_iter()
        .map(|(f, q, d)| (f.to_string(), q.to_string(), d))
        .collect()
    };
    assert_eq!(live_replies(&es), expected(&es), "full run");

    // One unrelated new mail lands in Inbox. Sent is untouched and not re-read.
    let appended = inbox + &entry("a4@x", "dora@example.org", "Lunch", "friday?", None);
    std::fs::write(mail.join("Inbox"), appended).unwrap();
    run_ok(corpus.path(), state.path(), &es);
    assert_eq!(
        live_replies(&es),
        expected(&es),
        "the Inbox → Sent reply must survive a run that re-read only Inbox"
    );
    let graph = journal_graph_summary(state.path());
    assert_eq!(graph["edges_unresolved"], 0, "{graph}");

    // And the other direction: a reply lands in the un-re-read file's PARENT
    // position — a new Sent message answering the new Inbox mail, then only
    // Sent changes. Inbox is carried over this time.
    let sent = std::fs::read_to_string(mail.join("Sent")).unwrap()
        + &entry("s5@x", "bob@example.org", "Re: Lunch", "yes", Some("a4@x"));
    std::fs::write(mail.join("Sent"), sent).unwrap();
    run_ok(corpus.path(), state.path(), &es);
    let mut want = expected(&es);
    want.insert((
        "Mail/Sent".into(),
        "In-Reply-To: <a4@x>".into(),
        node_of(&es, "a4@x"),
    ));
    assert_eq!(live_replies(&es), want);

    // What an incremental history converges to is what a clean index holds.
    // Compared without node ids: a file's key survives an in-place edit, so an
    // edited history and a clean index name the same messages by different ids
    // (each side's ids were checked against its own docs above).
    let clean = MockEs::start();
    let clean_state = tempfile::tempdir().unwrap();
    run_ok(corpus.path(), clean_state.path(), &clean);
    let shape = |es: &MockEs| -> BTreeSet<(String, String)> {
        live_replies(es)
            .into_iter()
            .map(|(f, q, _)| (f, q))
            .collect()
    };
    assert_eq!(shape(&es), shape(&clean));
    assert_eq!(shape(&clean).len(), 3);
}

/// Thunderbird's "compact folder" removes deleted messages and every later
/// message moves down. Node ids are positional (`m{offset}-msg-s0`), so a
/// saved `.eml` that replies into the mailbox — a file the run does NOT
/// re-read — holds an edge to the parent's OLD node id.
///
/// Before the fix that edge stayed live. With equal-length messages it is not
/// even dangling: the old id of `<b4@x>` is now the id of `<b5@x>`, so the
/// graph said, with 0.95 confidence and the quote `In-Reply-To: <b4@x>`, that
/// the reply answers a different message.
#[test]
fn a_reply_into_a_compacted_mailbox_follows_its_parent() {
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(corpus.path().join("Mail")).unwrap();
    std::fs::create_dir_all(corpus.path().join("saved")).unwrap();
    let msgs: Vec<String> = (1..=5)
        .map(|i| {
            entry(
                &format!("b{i}@x"),
                "bea@example.org",
                &format!("Topic {i}"),
                &format!("body of message {i}"),
                None,
            )
        })
        .collect();
    let mbox = corpus.path().join("Mail/Inbox.mbox");
    std::fs::write(&mbox, msgs.concat()).unwrap();
    std::fs::write(
        corpus.path().join("saved/reply.eml"),
        "From: bob@example.org\nTo: bea@example.org\nSubject: Re: Topic 4\n\
         Date: Wed, 15 Nov 2023 09:00:00 +0000\nMessage-ID: <r4@x>\nIn-Reply-To: <b4@x>\n\n\
         replying to topic four\n",
    )
    .unwrap();
    let es = MockEs::start();
    run_ok(corpus.path(), state.path(), &es);
    let before = node_of(&es, "b4@x");
    let one = |dst: String| -> BTreeSet<(String, String, String)> {
        [(
            "saved/reply.eml".to_string(),
            "In-Reply-To: <b4@x>".to_string(),
            dst,
        )]
        .into()
    };
    assert_eq!(live_replies(&es), one(before.clone()));

    // Compact: <b2@x> is gone, every later message moves down one slot.
    let compacted: String = msgs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, m)| m.as_str())
        .collect();
    std::fs::write(&mbox, compacted).unwrap();
    run_ok(corpus.path(), state.path(), &es);

    let after = node_of(&es, "b4@x");
    assert_ne!(after, before, "the parent moved — that is the scenario");
    assert_eq!(
        node_of(&es, "b5@x"),
        before,
        "…onto an id that is now a DIFFERENT message, so a stale edge is wrong, not just dangling"
    );
    assert_eq!(
        live_replies(&es),
        one(after),
        "the un-re-read .eml's reply edge must follow <b4@x> to its new node"
    );
    // The superseded edge is history, not gone: invalidated, still stored.
    let superseded: Vec<Value> = es
        .index(EDGES_INDEX)
        .values()
        .filter(|e| s(e, "type") == REPLIES_TO && s(e, "dst") == before)
        .cloned()
        .collect();
    assert_eq!(superseded.len(), 1);
    assert!(superseded[0].get("invalid_at").is_some());
    assert!(journal_graph_summary(state.path())["edges_invalidated"].as_u64() >= Some(1));

    // The parent leaves the corpus altogether: no live edge, one unresolved.
    let without_b4: String = msgs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 1 && *i != 3)
        .map(|(_, m)| m.as_str())
        .collect();
    std::fs::write(&mbox, without_b4).unwrap();
    run_ok(corpus.path(), state.path(), &es);
    assert!(live_replies(&es).is_empty(), "{:?}", live_replies(&es));
    assert_eq!(journal_graph_summary(state.path())["edges_unresolved"], 1);
}

/// The committed fixture must be exactly what `scripts/synthetic-takeout.py`
/// writes for [`FIXTURE_ARGS`] — otherwise "the tests use the generator" stops
/// being true the first time someone edits one and not the other.
///
/// `XERJ_REGENERATE_TAKEOUT_FIXTURE=1` rewrites the fixture instead of
/// checking it (that is the supported way to update it).
///
/// Honest limits: the generator's byte-identical guarantee is only VERIFIED
/// within one Python minor version (CPython documents `random()` as stable
/// across versions, not `choices`/`randint`). So a mismatch under a different
/// Python than the fixture was written with is reported and tolerated, and a
/// machine with no Python at all cannot run this check — loudly, and as a
/// failure when `CI` is set, because CI is where drift has to be caught.
#[test]
fn the_committed_fixture_is_what_the_generator_writes() {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/synthetic-takeout.py");
    assert!(
        script.is_file(),
        "generator missing at {}",
        script.display()
    );
    let python = ["python3", "python"].into_iter().find(|p| {
        std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    let Some(python) = python else {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI has no python3: fixture drift cannot be checked"
        );
        eprintln!("NOT CHECKED: no python3 on PATH, fixture drift check did not run");
        return;
    };
    let version = std::process::Command::new(python)
        .args(["-c", "import sys;print('%d.%d'%sys.version_info[:2])"])
        .output()
        .unwrap();
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();

    let out = tempfile::tempdir().unwrap();
    let tree = out.path().join("tree");
    let truth_path = out.path().join("truth.json");
    let status = std::process::Command::new(python)
        .arg(&script)
        .arg("--out")
        .arg(&tree)
        .arg("--truth")
        .arg(&truth_path)
        .args(FIXTURE_ARGS)
        .status()
        .unwrap();
    assert!(status.success(), "generator failed");

    fn files(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                files(root, &e.path(), out);
            } else {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(e.path()).unwrap());
            }
        }
    }
    let mut generated = BTreeMap::new();
    files(&tree, &tree, &mut generated);
    generated.insert("../truth.json".into(), std::fs::read(&truth_path).unwrap());

    if std::env::var_os("XERJ_REGENERATE_TAKEOUT_FIXTURE").is_some() {
        let _ = std::fs::remove_dir_all(fixture().join("tree"));
        copy_tree(&tree, &fixture().join("tree"));
        std::fs::copy(&truth_path, fixture().join("truth.json")).unwrap();
        std::fs::write(
            fixture().join("GENERATED_WITH_PYTHON.txt"),
            format!("{version}\n"),
        )
        .unwrap();
        eprintln!("fixture regenerated with python {version}");
        return;
    }

    let mut committed = BTreeMap::new();
    files(
        &fixture().join("tree"),
        &fixture().join("tree"),
        &mut committed,
    );
    committed.insert(
        "../truth.json".into(),
        std::fs::read(fixture().join("truth.json")).unwrap(),
    );
    let differing: Vec<&String> = generated
        .keys()
        .chain(committed.keys())
        .filter(|k| generated.get(*k) != committed.get(*k))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if differing.is_empty() {
        return;
    }
    // WHICH files exist is not a property of the Python version: the generator
    // decides names from its arguments, and only the bytes inside a file can
    // move between interpreter releases. So a file on one side only is never
    // tolerated. This is the check that catches a fixture file that never
    // reached the commit (the repo's blanket `*.md` ignore rule swallowed
    // `Drive/meeting-notes.md`, the tree was complete on the author's disk and
    // absent from every clean checkout; on CI's different Python the tolerant
    // branch below then reported "NOT VERIFIED" and passed).
    let one_sided: Vec<&String> = differing
        .iter()
        .copied()
        .filter(|k| generated.contains_key(*k) != committed.contains_key(*k))
        .collect();
    assert!(
        one_sided.is_empty(),
        "the committed fixture and the generator disagree about WHICH files exist \
         (python {version}): {one_sided:?}\n\
         a file the generator writes but the checkout lacks is usually an ignore rule: \
         `git check-ignore -v <file>`, then `git add -f` it or re-include the path in .gitignore"
    );
    let written_with =
        std::fs::read_to_string(fixture().join("GENERATED_WITH_PYTHON.txt")).unwrap_or_default();
    if written_with.trim() != version {
        eprintln!(
            "NOT VERIFIED: fixture written with python {}, this is {version}; {} file(s) differ \
             and the cause cannot be told apart from a generator change",
            written_with.trim(),
            differing.len()
        );
        return;
    }
    panic!(
        "fixture drifted from scripts/synthetic-takeout.py (same python {version}): {differing:?}\n\
         regenerate: XERJ_REGENERATE_TAKEOUT_FIXTURE=1 cargo test -p xerj-autoindex --lib \
         the_committed_fixture_is_what_the_generator_writes"
    );
}
