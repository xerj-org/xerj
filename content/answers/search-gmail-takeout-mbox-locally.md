---
title: "Search a Gmail Takeout mbox on your own machine"
h1: "I exported my Gmail with Google Takeout. How do I search that mbox file locally?"
description: "Unzip the export, run xerj autoindex on the folder, and search messages, threads and attached PDF pages over HTTP. The mailbox is streamed and stays on your machine."
slug: "search-gmail-takeout-mbox-locally"
cluster: "Folder indexing"
question: "How do I search a Google Takeout mbox file locally?"
intent: "informational"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt. The user has a Google Takeout download. Unzip it first (autoindex does not open archives), start a node with `xerj --insecure --data-dir ~/xerj-data`, run `xerj autoindex <unzipped folder> --url http://127.0.0.1:9200 --prefix mail --progress plain`, relay the xerj-bar line while it runs, then answer questions with term filters on email_from / email_thread_id / attachment_name and match_phrase on body, citing ax_locator and email_subject for every hit."
commands:
  - cmd: "bash -c 'unzip takeout-20260101T000000Z-001.zip -d ~/mail-export'"
    note: "Extract the download first. An unextracted .zip is recorded as junk with the exact command to run."
  - cmd: "xerj --insecure --data-dir ~/xerj-data"
    note: "Start a local node. Keep the data dir outside the folder you index."
  - cmd: "xerj autoindex ~/mail-export --url http://127.0.0.1:9200 --prefix mail --progress plain"
    note: "Index the export. The mbox is detected by content, streamed, and the bar moves inside the file."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/mail-*/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"term\":{\"email_from\":\"alice@example.org\"}}],\"must\":[{\"match_phrase\":{\"body\":\"signed lease\"}}]}},\"_source\":[\"email_subject\",\"email_date\",\"ax_locator\"]}'"
    note: "Ask the mailbox a question, scoped to one sender."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/mail-*/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"term\":{\"attachment_content_type\":\"application/pdf\"}}],\"must\":[{\"match_phrase\":{\"body\":\"INV-2024-0117\"}}]}},\"_source\":[\"attachment_name\",\"email_subject\",\"ax_locator\"]}'"
    note: "Find the page of an attached PDF that mentions something, and the mail it came with."
links_out:
  - "how-xerj-autoindexes-a-folder"
  - "autoindex-says-extract-the-archive-first"
  - "read-autoindex-progress"
  - "resume-interrupted-autoindex-run"
  - "why-autoindex-skipped-files"
  - "/docs/recipes/zero-config-autoindex"
evidence:
  - claim: "An mbox is detected by content — a From separator line carrying an asctime date followed by RFC 5322 headers — and the .mbox extension proves nothing either way."
    source: "engine/crates/xerj-autoindex/src/sniff.rs"
  - claim: "The splitter holds one buffered chunk, at most 1024 bytes of the current line while deciding what it is, and the one message being assembled, capped at 64 MB; nothing is proportional to the file."
    source: "engine/crates/xerj-autoindex/src/extract/mbox.rs"
  - claim: "Messages are parsed on a pool of --workers threads and forwarded in message order, so the record stream is identical to the sequential path; a test pins that."
    source: "engine/crates/xerj-autoindex/src/extract/mbox.rs"
  - claim: "Message records carry email_from, email_to, email_cc, email_subject, email_date, email_message_id, email_in_reply_to, email_references, email_thread_id and email_labels; attachment records add attachment_name, attachment_content_type and attachment_bytes."
    source: "engine/crates/xerj-autoindex/src/extract/eml.rs"
  - claim: "A folder holding archive_browser.html is a Takeout root; that page and a Keep note's .html twin are skipped by the rules default:takeout-index and default:takeout-keep-html-twin, both reported in the ignore accounting."
    source: "engine/crates/xerj-autoindex/src/walk.rs"
  - claim: "A .zip or .tgz is recorded as junk with the reason 'autoindex does not open archives — extract it first', naming the command for the kind in hand."
    source: "engine/crates/xerj-autoindex/src/sniff.rs"
  - claim: "The email-thread@1 detector writes replies_to edges from In-Reply-To or the nearest resolvable References ancestor, and attachment_of edges from attachment records to their message; every edge carries the header it came from as evidence."
    source: "engine/crates/xerj-autoindex/src/detect/emailthread.rs"
  - claim: "The Takeout layout rules and every number in this article come from a synthetic mailbox written by scripts/synthetic-takeout.py — it plants quoted and unquoted From lines, 8-bit bodies, a truncated multipart, a 300 KB line and a duplicated Message-ID; none of it is verified on a real Takeout export."
    source: "scripts/synthetic-takeout.py"
  - claim: "The end-to-end test checks every count against the generator's ground-truth file; PDF attachment pages are verified on a live node by benchmarks/mbox-ingest/verify.py because a unit test binary has no PDF worker."
    source: "engine/crates/xerj-autoindex/src/detect/e2e_mail.rs"
  - claim: "A per-item HTTP 429 from the server's memory circuit breaker is re-offered for up to 600 seconds instead of aborting the run; per-item 5xx and write blocks are not waited on."
    source: "engine/crates/xerj-autoindex/src/esclient.rs"
  - claim: "On the 1 GB synthetic mailbox the client peaked at 296 MB and finished in 279 s with every needle exactly-once; the node needed 68.5 GB of peak RSS with its cap lifted, and under the default 16 GiB cap the run aborted after ten minutes at the memory watermark with 82,422 of 106,581 documents indexed."
    source: "benchmarks/mbox-ingest/README.md"
faq:
  - q: "How do I search a Google Takeout mbox file locally?"
    a: "Unzip the download, start a local XERJ node, and run `xerj autoindex <folder> --prefix mail`. The mbox is detected by content and streamed. Then query `mail-*` over HTTP with filters on `email_from`, `email_thread_id` or `attachment_name` and `match_phrase` on `body`."
  - q: "The mbox is 8 GB. Does it get loaded into memory?"
    a: "Not by autoindex: the splitter streams the file and holds one message at a time, capped at 64 MB, and the client peaked under 300 MB on a 1 GB mailbox. The node is the limit today: on that 1 GB mailbox the server needed 68.5 GB of RSS to finish and did not finish under its default 16 GiB cap. That is filed as #948; do not expect a multi-GB export to finish on a laptop until it is fixed."
  - q: "Can it search inside the PDFs people attached?"
    a: "Yes. A PDF attachment goes through the PDF extractor and becomes one record per page, carrying the parent's `email_message_id`, `email_subject` and `email_from`, and an `attachment_of` edge points back to the message."
  - q: "Does it keep the Gmail threads?"
    a: "Gmail's `X-GM-THRID` header is kept as `email_thread_id`, and `In-Reply-To` / `References` become `replies_to` edges in the brain, so a whole thread is one term filter and a reply chain is a graph walk."
  - q: "Do I have to unzip the Takeout download?"
    a: "Yes. autoindex does not open archives. An unextracted `.zip` or `.tgz` is recorded as junk with the exact command to run, and the run continues."
  - q: "What about Outlook PST files or a Maildir?"
    a: "Not supported. There is no extractor for them. Export to mbox first — Thunderbird can do that — and index the mbox."
  - q: "Has this been tested on a real Takeout export?"
    a: "The mbox splitter and the message extractor are tested against a generated mailbox that reproduces the hard cases real exports contain. The Takeout folder layout rules have only seen that synthetic tree; that is stated as not verified on a real export until someone runs one."
---

**TL;DR** — Unzip the Takeout download, run `xerj autoindex` on the folder with its own `--prefix`, then search the mailbox over HTTP. The mbox is recognised by its content and streamed one message at a time. Attached PDFs become per-page documents linked to their message, and Gmail thread ids and reply headers become filters and graph edges. Nothing is uploaded.

## Unzip first

Google Takeout hands you one or more `.zip` (or `.tgz`) files. autoindex does not open archives, so extract before you index.

An unextracted archive is marked as junk, and the reason quotes the command to run: `unextracted zip archive: autoindex does not open archives — extract it first (unzip <file>), then run autoindex on the extracted folder`. The run goes on with whatever else is in the folder.

```sh
unzip takeout-20260101T000000Z-001.zip -d ~/mail-export
```

The result is a `Takeout/` folder. It holds `Mail/All mail Including Spam and Trash.mbox`, an `archive_browser.html` index page, and the other products you exported.

## Index the folder

```sh
xerj --insecure --data-dir ~/xerj-data
xerj autoindex ~/mail-export --url http://127.0.0.1:9200 --prefix mail --progress plain
```

autoindex recognises the mailbox by its content: a `From <sender> <date>` separator line followed by mail headers. It does not look at the `.mbox` extension. A renamed or extension-less mailbox is still a mailbox, and a text file called `notes.mbox` is not.

The file is streamed, and the splitter holds one message at a time. Messages are parsed on a pool of `--workers` threads, and the output is forwarded in message order. The progress bar advances inside the file, so a one-file corpus does not sit at zero for the whole run.

A folder that holds `archive_browser.html` is treated as a Takeout root. That page is skipped. A Keep note's `.html` twin is skipped when the `.json` beside it is the note. Both rules are named in the ignore accounting the run prints, and both are off under `--no-default-ignores`. Everything else in the export is indexed by ordinary sniffing.

## What comes back

Every message becomes a document with the decoded plain-text `body` and these fields: `email_from`, `email_to`, `email_cc`, `email_subject`, `email_date`, `email_message_id`, `email_in_reply_to`, `email_references`, `email_thread_id` (Gmail's `X-GM-THRID`) and `email_labels` (Gmail's `X-Gmail-Labels`).

Every attachment becomes its own document: one per page for a PDF, sectioned text for a text attachment, a name/type/size card for anything else. An attachment document carries `attachment_name`, `attachment_content_type` and `attachment_bytes`, plus the parent's message id, subject and sender.

```sh
curl -s -XPOST http://127.0.0.1:9200/mail-*/_search -H 'content-type: application/json' -d '{
  "query": {"bool": {"filter": [{"term": {"attachment_content_type": "application/pdf"}}],
                     "must": [{"match_phrase": {"body": "INV-2024-0117"}}]}},
  "_source": ["attachment_name", "email_subject", "email_from", "ax_locator"]}'
```

`ax_locator` is positional and stable. `m<offset>-msg-s0` is a message, and `m<offset>-att2-p7-s0` is page 7 of its third attachment. Re-running the command overwrites by the same ids instead of duplicating.

## Threads become a graph

The `email-thread@1` detector writes two edge types. A `replies_to` edge comes from the `In-Reply-To` header. When that header is missing or names a message outside the export, the nearest ancestor in `References` is used. An `attachment_of` edge goes from every attachment document to the message that carried it.

Each edge quotes the header it came from. A parent that is not in the export is counted as unresolved and never invented. Subject-line threading is deliberately not attempted.

## Limits, stated plainly

The mailbox splitter and the message extractor were tested against a generated mailbox. It contains what real exports contain: quoted and unquoted `From ` lines in bodies, 8-bit bodies with and without a declared charset, encoded-word subjects, and a truncated multipart. It also plants a 300 KB line, a duplicated Message-ID, and a final message with no trailing newline.

The Takeout layout rules have only seen that synthetic tree. They are not verified on a real Takeout export. Outlook PST/OST and Maildir have no extractor, and undeclared Cyrillic or CJK legacy encodings are not detected.

**Server memory is the limit today.** On the 1 GB synthetic mailbox, `xerj autoindex` itself peaked at 296 MB and finished in 279 s with every planted needle found exactly once. The node needed **68.5 GB** of peak RSS to get there, with its process cap lifted.

Under the default cap on the same machine (16 GiB), the server sat at its memory watermark from 87 % of the mailbox on. The run aborted after ten minutes of waiting, with 82,422 of 106,581 documents indexed. A 16 GiB laptop's default cap is 8 GiB.

This is the engine's memory while it indexes, filed as [#948](https://github.com/xerj-org/xerj/issues/948). Until it is fixed, treat a mailbox of a few hundred MB as the practical ceiling on a laptop. That ceiling is an expectation from the 1 GB run, not a measurement.

Wall time, throughput, memory and index size for the 1 GB synthetic mailbox are in the repository's `benchmarks/mbox-ingest/README.md`, with the machine and the exact commands.
