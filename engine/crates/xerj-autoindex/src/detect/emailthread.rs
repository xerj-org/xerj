//! email-thread@1 — who replied to whom, and what was attached to what.
//!
//! Mail has the one relationship no filesystem detector can see: a reply is a
//! document ABOUT another document, and the sender said so, in a header, with
//! an id. Two edge types come out of that, both from structured fields the
//! email extractor already parsed (`extract::eml`), never from body text:
//!
//! - **`replies_to`** — reply → parent message. The parent is named by
//!   `In-Reply-To`; when that header is missing, or names a message that is
//!   not in the corpus, the `References` chain is walked nearest-ancestor
//!   first. The first id that resolves wins, and the evidence says which
//!   header it came from.
//! - **`attachment_of`** — attachment record → the message that carried it.
//!   One edge per attachment RECORD, so landing on page 7 of an attached PDF
//!   is one hop from the email it arrived in.
//!
//! The message's NODE is its first section, locator `msg-s0` (a standalone
//! `.eml`) or `m{offset}-msg-s0` (one entry of an mbox). Later sections of a
//! long body are not nodes here; they carry the same `email_message_id` field.
//!
//! ## Why resolution waits for the end of the run
//! A reply and its parent are usually in the same mailbox and often not in
//! order, and the same Message-ID can legitimately exist twice (one mailbox
//! exported twice, a message that is both in an mbox and saved as `.eml`).
//! Phase B reads files on parallel workers, so "the parent I have seen so far"
//! is a fact about thread scheduling. Every `replies_to` edge is therefore
//! drafted in `detect_corpus`, after every message of the run is known, and a
//! duplicated Message-ID resolves to the numerically smallest node id — a
//! function of the corpus, not of the interleaving. `attachment_of` needs no
//! table (parent and attachment share a file and a locator prefix) and is
//! drafted inline.
//!
//! ## What it costs
//! One table entry per message — 16-byte id hash, 16-byte node id, the
//! Message-ID string for the evidence quote — and one pending entry per reply
//! holding at most [`MAX_CANDIDATES`] 16-byte hashes. Nothing is proportional
//! to body size. The measured footprint on a real run is in
//! `benchmarks/mbox-ingest/README.md`.
//!
//! ## What it does not see
//! - A parent that is not in the corpus is a dangling reply: counted
//!   (`DetectorCounters::unresolved`), never invented.
//! - Subject-line threading (`Re: …` with no ids) is deliberately absent. It is
//!   a guess, and this detector's confidence says it does not guess.
//!
//! ## Runs that do not re-read every mailbox
//! An incremental run re-reads only the files that changed, and a resumed run
//! only the files the interrupted one had not finished. Resolution is still a
//! function of the CORPUS: the message nodes of every mail file this run did
//! not re-read are loaded back from the index (`carry_over`, fed by
//! `lib.rs::carry_over_unread_mail`) and take part in `detect_corpus` exactly
//! as if they had been read — both as parents (`by_id`) and as replies
//! (`pending`). Without that, `Inbox` + `Sent` — the normal layout of
//! Thunderbird, Apple Mail and mutt — lost every reply edge that crossed the
//! two files as soon as one of them changed, and an edge into a compacted
//! mailbox stayed live while pointing at a node id that no longer existed
//! (review finding on PR #949). Re-emitting an unchanged edge is free: its
//! `edge_id` is a function of (src, type, dst, file mtime), so it overwrites
//! itself. An edge of an un-re-read file that is NOT re-emitted — its parent
//! moved, or left the corpus — is soft-invalidated by the caller.

use super::{clip_quote, CorpusIndex, DetectorCounters, EdgeDetector, EdgeDraft, RecordCtx};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use xxhash_rust::xxh3::xxh3_128;

pub const TAG: &str = "email-thread@1";
pub const REPLIES_TO: &str = "replies_to";
pub const ATTACHMENT_OF: &str = "attachment_of";

/// A reply header is an assertion by the sender's own mail client. It is wrong
/// only when a client mangles ids, which is why this is not 1.0.
pub const REPLY_WEIGHT: f32 = 0.9;
pub const REPLY_CONFIDENCE: f32 = 0.95;
/// Containment is not an inference at all: the MIME parser found the part
/// inside the message. Same figure `sequence` uses for the same reason.
pub const ATTACH_WEIGHT: f32 = 0.9;
pub const ATTACH_CONFIDENCE: f32 = 0.99;

/// Parent candidates kept per reply: `In-Reply-To` plus the nearest ancestors
/// from `References`. A thread hundreds deep names every ancestor; the nearest
/// few are the ones that can stand in for a missing parent.
pub const MAX_CANDIDATES: usize = 8;

/// Message-IDs are compared as the sender wrote them minus the angle brackets
/// and surrounding whitespace. NOT case-folded: the left-hand side of an id is
/// case-sensitive by RFC 5322, and folding it would merge distinct messages.
fn id_hash(message_id: &str) -> Option<u128> {
    let id = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!id.is_empty()).then(|| xxh3_128(id.as_bytes()))
}

/// `"m123-att0-p1-s0"` → `("m123-", "att0-p1-s0")`; `"msg-s0"` → `("", "msg-s0")`.
/// The container prefix is `m` + ASCII digits + `-`, so every byte index used
/// here sits on a char boundary by construction.
fn split_container_prefix(locator: &str) -> (&str, &str) {
    if let Some(rest) = locator.strip_prefix('m') {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest.as_bytes().get(digits) == Some(&b'-') {
            return locator.split_at(1 + digits + 1);
        }
    }
    ("", locator)
}

struct Seen {
    node: u128,
    message_id: String,
}

struct Pending {
    src: u128,
    /// Index into `State::files`.
    file: u32,
    /// (id hash, came from `In-Reply-To` rather than `References`).
    candidates: Vec<(u128, bool)>,
}

#[derive(Default)]
struct State {
    by_id: HashMap<u128, Seen>,
    pending: Vec<Pending>,
    files: Vec<String>,
    file_index: HashMap<String, u32>,
}

impl State {
    fn file(&mut self, rel: &str) -> u32 {
        if let Some(i) = self.file_index.get(rel) {
            return *i;
        }
        let i = self.files.len() as u32;
        self.files.push(rel.to_string());
        self.file_index.insert(rel.to_string(), i);
        i
    }
}

#[derive(Default)]
pub struct EmailThread {
    state: Mutex<State>,
    unresolved: AtomicU64,
    ambiguous: AtomicU64,
}

fn node_hex(node: u128) -> String {
    format!("{node:032x}")
}

impl EmailThread {
    /// Register one message NODE: its own Message-ID as a possible parent, and
    /// its reply headers as a pending edge. Shared by `detect_record` (a
    /// message read in this run) and `carry_over` (one loaded from the index).
    fn observe_message(&self, ctx: &RecordCtx<'_>) {
        let text = |k: &str| ctx.fields.get(k).and_then(Value::as_str);
        let Ok(node) = u128::from_str_radix(ctx.doc_id, 16) else {
            return;
        };
        let mut candidates: Vec<(u128, bool)> = Vec::new();
        if let Some(h) = text("email_in_reply_to").and_then(id_hash) {
            candidates.push((h, true));
        }
        if let Some(refs) = ctx.fields.get("email_references").and_then(Value::as_array) {
            // Nearest ancestor first: `References` lists oldest → newest.
            for r in refs.iter().rev().filter_map(Value::as_str) {
                if candidates.len() >= MAX_CANDIDATES {
                    break;
                }
                if let Some(h) = id_hash(r) {
                    if !candidates.iter().any(|(c, _)| *c == h) {
                        candidates.push((h, false));
                    }
                }
            }
        }
        let own = text("email_message_id").and_then(|id| Some((id_hash(id)?, id)));
        if own.is_none() && candidates.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((hash, id)) = own {
            match state.by_id.get_mut(&hash) {
                // The same NODE offered twice (a caller that both re-read and
                // carried a file over) is one message, not an ambiguity.
                Some(seen) if seen.node == node => {}
                Some(seen) => {
                    // The same Message-ID on two nodes. Smallest node id wins,
                    // whatever order the workers delivered them in.
                    self.ambiguous.fetch_add(1, Ordering::Relaxed);
                    if node < seen.node {
                        seen.node = node;
                    }
                }
                None => {
                    state.by_id.insert(
                        hash,
                        Seen {
                            node,
                            message_id: id.trim().to_string(),
                        },
                    );
                }
            }
        }
        if !candidates.is_empty() {
            let file = state.file(&ctx.file.rel);
            state.pending.push(Pending {
                src: node,
                file,
                candidates,
            });
        }
    }
}

impl EdgeDetector for EmailThread {
    fn tag(&self) -> &'static str {
        TAG
    }

    fn detect_record(&self, ctx: &RecordCtx<'_>, out: &mut Vec<EdgeDraft>) {
        if !matches!(ctx.file.family.as_str(), "eml" | "mbox") {
            return;
        }
        let (prefix, rest) = split_container_prefix(ctx.locator);
        let text = |k: &str| ctx.fields.get(k).and_then(Value::as_str);

        // ── attachment record → its message ──
        if rest.starts_with("att") {
            let Some(name) = text("attachment_name") else {
                return;
            };
            let parent = crate::ids::doc_id(
                &ctx.file.dataset_slug,
                &ctx.file.file_key,
                &format!("{prefix}msg-s0"),
            );
            let of = match (text("email_message_id"), text("email_subject")) {
                (Some(id), _) => format!("message <{id}>"),
                (None, Some(subject)) => format!("message \"{subject}\""),
                (None, None) => "its message".to_string(),
            };
            out.push(EdgeDraft {
                src: ctx.doc_id.to_string(),
                dst: parent,
                edge_type: ATTACHMENT_OF,
                weight: ATTACH_WEIGHT,
                confidence: ATTACH_CONFIDENCE,
                valid_at_ms: ctx.file.mtime_ms,
                src_file: ctx.file.rel.clone(),
                quote: clip_quote(&format!("attachment \"{name}\" of {of}")),
                offset: 0,
                src_format: ctx.file.format.clone(),
                dst_format: ctx.file.format.clone(),
            });
            return;
        }

        // ── the message node itself ──
        if rest == "msg-s0" {
            self.observe_message(ctx);
        }
    }

    /// A message node of a mail file this run did NOT re-read, loaded back
    /// from the index. It joins the same tables a freshly read message joins,
    /// so `detect_corpus` cannot tell the two apart — which is the point.
    fn carry_over(&self, ctx: &RecordCtx<'_>) {
        if !matches!(ctx.file.family.as_str(), "eml" | "mbox") {
            return;
        }
        if split_container_prefix(ctx.locator).1 == "msg-s0" {
            self.observe_message(ctx);
        }
    }

    fn detect_corpus(&self, corpus: &CorpusIndex, out: &mut Vec<EdgeDraft>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // Worker interleaving decided the push order; the node id decides the
        // output order.
        state.pending.sort_by_key(|p| p.src);
        // One reply per node, however many times the node was offered.
        state.pending.dedup_by_key(|p| p.src);
        let state = &*state;
        for reply in &state.pending {
            let parent = reply.candidates.iter().find_map(|(hash, direct)| {
                state
                    .by_id
                    .get(hash)
                    .filter(|seen| seen.node != reply.src)
                    .map(|seen| (seen, *direct))
            });
            let Some((seen, direct)) = parent else {
                self.unresolved.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let rel = state.files.get(reply.file as usize).map(String::as_str);
            let Some(file) = rel.and_then(|rel| corpus.files.get(rel)) else {
                continue;
            };
            let header = if direct { "In-Reply-To" } else { "References" };
            out.push(EdgeDraft {
                src: node_hex(reply.src),
                dst: node_hex(seen.node),
                edge_type: REPLIES_TO,
                weight: REPLY_WEIGHT,
                confidence: REPLY_CONFIDENCE,
                valid_at_ms: file.mtime_ms,
                src_file: file.rel.clone(),
                quote: clip_quote(&format!("{header}: <{}>", seen.message_id)),
                offset: 0,
                src_format: file.format.clone(),
                // The parent may live in another file; its format is not
                // tracked per message, and an empty label is omitted from the
                // stored edge rather than guessed.
                dst_format: String::new(),
            });
        }
    }

    fn counters(&self) -> DetectorCounters {
        DetectorCounters {
            unresolved: self.unresolved.load(Ordering::Relaxed),
            ambiguous: self.ambiguous.load(Ordering::Relaxed),
            capped: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{corpus_file, CorpusFile, CorpusIndex};
    use super::*;
    use serde_json::{json, Map};

    fn corpus() -> CorpusIndex {
        CorpusIndex::build(vec![
            corpus_file("Takeout/Mail/All mail.mbox", "kmbox", "docs", "mbox", 1_000),
            corpus_file("saved/reply.eml", "keml", "docs", "eml", 2_000),
            corpus_file("notes.md", "kmd", "docs", "txt-prose", 3_000),
        ])
    }

    fn fields(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    fn observe(
        det: &EmailThread,
        corpus: &CorpusIndex,
        file: &CorpusFile,
        locator: &str,
        f: Value,
        out: &mut Vec<EdgeDraft>,
    ) -> String {
        let id = crate::ids::doc_id(&file.dataset_slug, &file.file_key, locator);
        let f = fields(f);
        det.detect_record(
            &RecordCtx {
                corpus,
                file,
                locator,
                doc_id: &id,
                fields: &f,
            },
            out,
        );
        id
    }

    #[test]
    fn locator_prefixes_split_on_the_container_offset_only() {
        assert_eq!(split_container_prefix("m0-msg-s0"), ("m0-", "msg-s0"));
        assert_eq!(
            split_container_prefix("m18446744073709551615-att3-p2-s1"),
            ("m18446744073709551615-", "att3-p2-s1")
        );
        // A standalone .eml: `msg-s0` starts with `m` and is NOT a prefix.
        assert_eq!(split_container_prefix("msg-s0"), ("", "msg-s0"));
        assert_eq!(split_container_prefix("att0-card"), ("", "att0-card"));
        assert_eq!(split_container_prefix("m12"), ("", "m12"));
        assert_eq!(split_container_prefix("m-x"), ("", "m-x"));
        assert_eq!(split_container_prefix("mé-1"), ("", "mé-1"));
        assert_eq!(split_container_prefix(""), ("", ""));
    }

    #[test]
    fn a_reply_links_to_its_parent_whatever_order_they_arrive_in() {
        let corpus = corpus();
        let mbox = &corpus.files["Takeout/Mail/All mail.mbox"];
        for reply_first in [false, true] {
            let det = EmailThread::default();
            let mut inline = Vec::new();
            let parent = json!({"email_message_id": "root@x", "email_subject": "Deal"});
            let reply = json!({"email_message_id": "re1@x", "email_in_reply_to": "<root@x>",
                               "email_references": ["root@x"]});
            let (p, r) = if reply_first {
                let r = observe(&det, &corpus, mbox, "m900-msg-s0", reply, &mut inline);
                let p = observe(&det, &corpus, mbox, "m0-msg-s0", parent, &mut inline);
                (p, r)
            } else {
                let p = observe(&det, &corpus, mbox, "m0-msg-s0", parent, &mut inline);
                let r = observe(&det, &corpus, mbox, "m900-msg-s0", reply, &mut inline);
                (p, r)
            };
            assert!(inline.is_empty(), "replies are never drafted inline");
            let mut out = Vec::new();
            det.detect_corpus(&corpus, &mut out);
            assert_eq!(out.len(), 1);
            let e = &out[0];
            assert_eq!((e.src.as_str(), e.dst.as_str()), (r.as_str(), p.as_str()));
            assert_eq!(e.edge_type, REPLIES_TO);
            assert_eq!(e.quote, "In-Reply-To: <root@x>");
            assert_eq!(e.src_file, "Takeout/Mail/All mail.mbox");
            assert_eq!(e.valid_at_ms, 1_000);
            assert_eq!(e.src_format, "mbox");
            assert_eq!(det.counters(), DetectorCounters::default());
        }
    }

    /// `In-Reply-To` names a message that is not in the corpus; the nearest
    /// `References` ancestor that IS in it stands in, and the evidence says so.
    #[test]
    fn references_stand_in_for_a_missing_parent_nearest_first() {
        let corpus = corpus();
        let mbox = &corpus.files["Takeout/Mail/All mail.mbox"];
        let det = EmailThread::default();
        let mut sink = Vec::new();
        let root = observe(
            &det,
            &corpus,
            mbox,
            "m0-msg-s0",
            json!({"email_message_id": "root@x"}),
            &mut sink,
        );
        let mid = observe(
            &det,
            &corpus,
            mbox,
            "m10-msg-s0",
            json!({"email_message_id": "mid@x", "email_in_reply_to": "root@x"}),
            &mut sink,
        );
        let leaf = observe(
            &det,
            &corpus,
            mbox,
            "m20-msg-s0",
            json!({"email_message_id": "leaf@x", "email_in_reply_to": "gone@x",
                   "email_references": ["root@x", "mid@x", "gone@x"]}),
            &mut sink,
        );
        let mut out = Vec::new();
        det.detect_corpus(&corpus, &mut out);
        let by_src: HashMap<&str, &EdgeDraft> = out.iter().map(|e| (e.src.as_str(), e)).collect();
        assert_eq!(out.len(), 2);
        assert_eq!(by_src[mid.as_str()].dst, root);
        assert_eq!(
            by_src[leaf.as_str()].dst,
            mid,
            "nearest ancestor, not the root"
        );
        assert_eq!(by_src[leaf.as_str()].quote, "References: <mid@x>");
    }

    #[test]
    fn dangling_and_self_replies_draw_nothing_and_are_counted() {
        let corpus = corpus();
        let mbox = &corpus.files["Takeout/Mail/All mail.mbox"];
        let det = EmailThread::default();
        let mut sink = Vec::new();
        observe(
            &det,
            &corpus,
            mbox,
            "m0-msg-s0",
            json!({"email_message_id": "a@x", "email_in_reply_to": "nobody@x"}),
            &mut sink,
        );
        observe(
            &det,
            &corpus,
            mbox,
            "m50-msg-s0",
            json!({"email_message_id": "self@x", "email_in_reply_to": "self@x"}),
            &mut sink,
        );
        let mut out = Vec::new();
        det.detect_corpus(&corpus, &mut out);
        assert!(
            out.is_empty(),
            "{:?}",
            out.iter().map(|e| &e.quote).collect::<Vec<_>>()
        );
        assert_eq!(det.counters().unresolved, 2);
    }

    /// The same Message-ID in an mbox and in a saved .eml: both arrival orders
    /// must pick the same parent node.
    #[test]
    fn a_duplicated_message_id_resolves_deterministically() {
        let corpus = corpus();
        let mbox = &corpus.files["Takeout/Mail/All mail.mbox"];
        let eml = &corpus.files["saved/reply.eml"];
        let mut picked = Vec::new();
        for flip in [false, true] {
            let det = EmailThread::default();
            let mut sink = Vec::new();
            let dup = json!({"email_message_id": "dup@x"});
            if flip {
                observe(&det, &corpus, eml, "msg-s0", dup.clone(), &mut sink);
                observe(&det, &corpus, mbox, "m0-msg-s0", dup, &mut sink);
            } else {
                observe(&det, &corpus, mbox, "m0-msg-s0", dup.clone(), &mut sink);
                observe(&det, &corpus, eml, "msg-s0", dup, &mut sink);
            }
            observe(
                &det,
                &corpus,
                mbox,
                "m70-msg-s0",
                json!({"email_message_id": "r@x", "email_in_reply_to": "dup@x"}),
                &mut sink,
            );
            let mut out = Vec::new();
            det.detect_corpus(&corpus, &mut out);
            assert_eq!(out.len(), 1);
            assert_eq!(det.counters().ambiguous, 1);
            picked.push(out[0].dst.clone());
        }
        assert_eq!(picked[0], picked[1]);
    }

    #[test]
    fn every_attachment_record_points_at_its_own_message() {
        let corpus = corpus();
        let mbox = &corpus.files["Takeout/Mail/All mail.mbox"];
        let eml = &corpus.files["saved/reply.eml"];
        let det = EmailThread::default();
        let mut out = Vec::new();
        let att =
            json!({"attachment_name": "term-sheet — 設計書.pdf", "email_message_id": "deal@x"});
        let page = observe(
            &det,
            &corpus,
            mbox,
            "m4096-att0-p2-s0",
            att.clone(),
            &mut out,
        );
        let card = observe(
            &det,
            &corpus,
            eml,
            "att1-card",
            json!({"attachment_name": "logo.png", "email_subject": "logo"}),
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].src, page);
        assert_eq!(
            out[0].dst,
            crate::ids::doc_id("docs", "kmbox", "m4096-msg-s0"),
            "the message in the SAME container slot, not m0"
        );
        assert_eq!(out[0].edge_type, ATTACHMENT_OF);
        assert_eq!(
            out[0].quote,
            "attachment \"term-sheet — 設計書.pdf\" of message <deal@x>"
        );
        assert_eq!(out[1].src, card);
        assert_eq!(out[1].dst, crate::ids::doc_id("docs", "keml", "msg-s0"));
        assert_eq!(out[1].quote, "attachment \"logo.png\" of message \"logo\"");
        let mut corpus_pass = Vec::new();
        det.detect_corpus(&corpus, &mut corpus_pass);
        assert!(corpus_pass.is_empty(), "attachments are not replies");
    }

    /// Only mail families are read: a markdown record that happens to carry a
    /// field called `email_message_id` is not a message.
    #[test]
    fn records_of_other_families_are_ignored() {
        let corpus = corpus();
        let md = &corpus.files["notes.md"];
        let det = EmailThread::default();
        let mut out = Vec::new();
        observe(
            &det,
            &corpus,
            md,
            "msg-s0",
            json!({"email_message_id": "a@x", "email_in_reply_to": "b@x",
                   "attachment_name": "x"}),
            &mut out,
        );
        det.detect_corpus(&corpus, &mut out);
        assert!(out.is_empty());
        assert_eq!(det.counters(), DetectorCounters::default());
    }

    /// A long subject must clip on a char boundary (panic = abort).
    #[test]
    fn evidence_quotes_clip_multibyte_text_safely() {
        let corpus = corpus();
        let mbox = &corpus.files["Takeout/Mail/All mail.mbox"];
        let det = EmailThread::default();
        let mut out = Vec::new();
        for n in 225..245 {
            let name = "設".repeat(n);
            observe(
                &det,
                &corpus,
                mbox,
                "m1-att0-card",
                json!({"attachment_name": name, "email_subject": "مرحبا".repeat(60)}),
                &mut out,
            );
        }
        assert!(out.iter().all(|e| e.quote.chars().count() <= 240));
    }
}
