---
title: "Search a Gmail Takeout mbox on your own machine"
h1: "I exported my Gmail with Google Takeout. How do I search that mbox file locally?"
description: "Unzip the export, run xerj autoindex on the folder, and search messages, threads and attached PDF pages over HTTP. The mailbox is streamed, and with the default embedder no message text leaves your machine."
slug: "search-gmail-takeout-mbox-locally"
cluster: "Folder indexing"
question: "How do I search a Google Takeout mbox file locally?"
intent: "informational"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt. The user has a Google Takeout download, which is private mail. Unzip it first (autoindex does not open archives), start a node with `xerj --insecure --data-dir ~/xerj-data` on the user's own machine, run `xerj autoindex <unzipped folder> --url http://127.0.0.1:9200 --prefix mail --progress plain`, and relay the xerj-bar line while it runs. Before indexing, tell the user the node's memory limit (#948): at a 16 GiB laptop's cap, a 300 MB mailbox drove the node to 18-20 GiB. Answer questions with term filters on email_from_address / email_thread_id (hex) / attachment_name and match or match_phrase on body, which also holds the subject. Cite ax_locator and email_subject for every hit. If you file a field report, pass --pointed-at \"a private mailbox\" so the folder path is not published."
commands:
  - cmd: "bash -c 'unzip takeout-20260101T000000Z-001.zip -d ~/mail-export'"
    note: "Extract the download first. An unextracted .zip is named in the run's output with the exact command to run."
  - cmd: "xerj --insecure --data-dir ~/xerj-data"
    note: "Start a local node with no authentication on 127.0.0.1. Keep the data dir outside the folder you index. On a shared machine, drop --insecure and use the admin key."
  - cmd: "xerj autoindex ~/mail-export --url http://127.0.0.1:9200 --prefix mail --progress plain"
    note: "Index the export. The mbox is detected by content and streamed, and the progress bar moves inside the file."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/mail-*/_search -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"term\":{\"email_from_address\":\"alice@example.org\"}}],\"must\":[{\"match_phrase\":{\"body\":\"signed lease\"}}]}},\"_source\":[\"email_subject\",\"email_date\",\"ax_locator\"]}'"
    note: "Ask the mailbox a question, scoped to one sender by bare address."
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
  - claim: "The splitter holds one buffered chunk, at most 1024 bytes of the current line while deciding what it is, and the one message being assembled, capped at 64 MB. The bytes of messages in flight on the parse pool are bounded at 256 MB for the whole process."
    source: "engine/crates/xerj-autoindex/src/extract/mbox.rs"
  - claim: "Messages are parsed on a pool of --workers threads and forwarded in message order, so the record stream is identical to the sequential path; a test pins that."
    source: "engine/crates/xerj-autoindex/src/extract/mbox.rs"
  - claim: "Message records carry email_from, email_to, email_cc, their bare lower-cased addresses in email_from_address, email_to_address and email_cc_address (at most 1,024 per header), email_subject, email_date, email_message_id, email_in_reply_to, email_references, email_thread_id and email_labels. The first 512 characters of the subject open the first body section. Attachment records add attachment_name, attachment_content_type and attachment_bytes, and a PDF page is titled with the attachment's name."
    source: "engine/crates/xerj-autoindex/src/extract/eml.rs"
  - claim: "X-GM-THRID is written in decimal and stored in email_thread_id as the same number in lower-case hex; Google documents the decimal id as the equivalent of the hex id used in the Gmail web interface and API."
    source: "engine/crates/xerj-autoindex/src/extract/eml.rs"
  - claim: "A folder holding archive_browser.html is a Takeout root; that page and a Keep note's .html twin are skipped by the rules default:takeout-index and default:takeout-keep-html-twin, both reported in the ignore accounting."
    source: "engine/crates/xerj-autoindex/src/walk.rs"
  - claim: "A .zip or .tgz is recorded as junk with the reason 'autoindex does not open archives — extract it first', naming the command for the kind in hand, and the run prints that line under its summary and in the --json summary as unextracted_archives."
    source: "engine/crates/xerj-autoindex/src/lib.rs"
  - claim: "The email-thread@1 detector writes replies_to edges from In-Reply-To or the nearest resolvable References ancestor, and attachment_of edges from attachment records to their message; every edge carries the header it came from as evidence. A run that re-reads only some mailboxes loads the message nodes of the others back from the index, so a reply across Inbox and Sent survives an incremental run; a test pins that and the compacted-mailbox case."
    source: "engine/crates/xerj-autoindex/src/detect/e2e_mail.rs"
  - claim: "The Takeout layout rules and every number in this article come from a synthetic mailbox written by scripts/synthetic-takeout.py — it plants quoted and unquoted From lines, 8-bit bodies, a truncated multipart, a 300 KB line and a duplicated Message-ID; none of it is verified on a real Takeout export."
    source: "scripts/synthetic-takeout.py"
  - claim: "A per-item HTTP 429 from the server's memory circuit breaker is re-offered for up to 600 seconds instead of aborting the run; per-item 5xx and write blocks are not waited on."
    source: "engine/crates/xerj-autoindex/src/esclient.rs"
  - claim: "On the 1 GB synthetic mailbox the client peaked at 296 MiB and finished in 279 s; all 2,530 planted needles were found in the right message, 2,529 as exactly one document and one as two overlapping sections of its message."
    source: "benchmarks/mbox-ingest/results/after-mixed-1G-runC-verify-every-needle.json"
  - claim: "That complete run needed the node's cap lifted and a server peak of 66.9 GiB; under the default 16 GiB cap the server sat at its watermark from 87 % of the mailbox on and the run aborted after ten minutes with 82,422 of 106,581 documents indexed."
    source: "benchmarks/mbox-ingest/README.md"
  - claim: "A 300 MB synthetic mailbox against a node capped at 8 GiB, the default on a 16 GiB machine, completed twice with every needle found, and the server's peak resident memory was 18.2 GiB and 19.9 GiB."
    source: "benchmarks/mbox-ingest/results/after-mixed-300M-cap8g-run2.json"
  - claim: "The 1 GB index reopened under the default cap answered 16 and 17 of 40 first-time single-term queries in over a second (slowest 8.7 s and 16.6 s in two measurements); with the cap lifted, 4 of 40."
    source: "benchmarks/mbox-ingest/results/after-mixed-1G-runC-restart-query-latency.txt"
  - claim: "xerj feedback auto-fills 'Pointed at' from the running node's catalog, which names the indexed folder, unless --pointed-at is given; --no-autofill skips the probe and --dry-run prints the report without publishing it."
    source: "engine/crates/xerj-autoindex/src/feedback.rs"
faq:
  - q: "How do I search a Google Takeout mbox file locally?"
    a: "Unzip the download, start a local XERJ node, and run `xerj autoindex <folder> --prefix mail`. The mbox is detected by content and streamed. Then query `mail-*` over HTTP: filter on `email_from_address`, `email_thread_id` or `attachment_name`, and use `match` or `match_phrase` on `body`."
  - q: "Why does a term filter on email_from find nothing?"
    a: "`email_from` holds the header as a person reads it, such as `Alice Anders <alice@example.org>`, and it is a keyword, so only the whole string matches. Filter on `email_from_address` (or `email_to_address`, `email_cc_address`) with the bare, lower-case address."
  - q: "The mbox is 8 GB. Does it get loaded into memory?"
    a: "Not by autoindex. The splitter streams the file, and the client peaked at 296 MiB on a 1 GB mailbox. The node is the limit today. On that 1 GB mailbox the server needed 66.9 GiB with its cap lifted and did not finish under its default 16 GiB cap. At a 16 GiB laptop's 8 GiB cap, a 300 MB mailbox completed, but the server peaked at 18–20 GiB. That is filed as #948. Do not expect a multi-GB export to finish on a laptop until it is fixed."
  - q: "Can it search inside the PDFs people attached?"
    a: "Yes. A PDF attachment goes through the PDF extractor and becomes one record per page, titled with the attachment's name and carrying the parent's `email_message_id`, `email_subject` and `email_from_address`. An `attachment_of` edge points back to the message."
  - q: "Does it keep the Gmail threads?"
    a: "Yes. Gmail's `X-GM-THRID` is stored as `email_thread_id` in lower-case hex, the form the Gmail web interface uses. The export writes it in decimal, so convert with `printf '%x\\n' <decimal>` before you filter. `In-Reply-To` and `References` become `replies_to` edges in the brain, so a whole thread is one term filter and a reply chain is a graph walk."
  - q: "Does my mail leave my machine?"
    a: "Not with the default configuration. The default embedder is lexical (feature hashing, not neural) and runs inside the node, and autoindex talks only to the node URL you give it. `--embed-mode proxy` would send body text to the endpoint you configure. A field report filed with `xerj feedback` is public and auto-fills the indexed folder's path, so for private mail pass `--pointed-at \"a private mailbox\"`. `--insecure` means no authentication for anyone on the same machine."
  - q: "Do I have to unzip the Takeout download?"
    a: "Yes. autoindex does not open archives. An unextracted `.zip` or `.tgz` is recorded as junk, the run prints it under its summary with the exact command to run, and the run continues."
  - q: "Has this been tested on a real Takeout export?"
    a: "No. The mbox splitter and the message extractor are tested against a generated mailbox that reproduces the hard cases real exports contain. The Takeout folder layout rules have only seen that synthetic tree. Treat them as unverified on a real export until someone runs one."
---

**TL;DR** — Unzip the Takeout download, run `xerj autoindex` on the folder with its own `--prefix`, then search the mailbox over HTTP. The mbox is recognised by its content and streamed one message at a time. Attached PDFs become per-page documents linked to their message. Senders are filterable by bare address, and Gmail thread ids and reply headers become filters and graph edges. With the default embedder, which is lexical (feature hashing, not neural), no message text leaves your machine. The node's memory is the limit today: at a 16 GiB laptop's memory cap, a 300 MB mailbox already drove the node past that laptop's RAM in our runs.

## Unzip first

Google Takeout hands you one or more `.zip` (or `.tgz`) files. autoindex does not open archives, so extract them before you index.

An unextracted archive is recorded as junk, and the run prints it under its summary with the command to run. The line starts `not indexed — archives are never opened` and names the file. The same list is in the `--json` summary as `unextracted_archives`. The run goes on with whatever else is in the folder.

```sh
unzip takeout-20260101T000000Z-001.zip -d ~/mail-export
```

The result is a `Takeout/` folder. It holds `Mail/All mail Including Spam and Trash.mbox`, an `archive_browser.html` index page, and the other products you exported.

## Index the folder

```sh
xerj --insecure --data-dir ~/xerj-data
xerj autoindex ~/mail-export --url http://127.0.0.1:9200 --prefix mail --progress plain
```

`--insecure` starts the node with no authentication on `127.0.0.1`. On your own laptop that is fine. On a machine other people use, anyone there can read the mailbox while the node runs. In that case start the node without the flag, and pass the key it writes to `~/xerj-data/admin.key` as `XERJ_API_KEY="$(cat ~/xerj-data/admin.key)"`.

autoindex recognises the mailbox by its content: a `From <sender> <date>` separator line followed by mail headers. It does not look at the `.mbox` extension. A renamed or extension-less mailbox is still a mailbox, and a text file called `notes.mbox` is not.

The file is streamed. Messages are parsed on a pool of `--workers` threads, and the output is forwarded in message order. The bytes of messages in flight are capped at 256 MB for the whole process. The progress bar advances inside the file, so a one-file corpus does not sit at zero for the whole run.

A folder that holds `archive_browser.html` is treated as a Takeout root. That page is skipped. A Keep note's `.html` twin is skipped when the `.json` beside it is the note. Both rules are named in the ignore accounting the run prints, and both are off under `--no-default-ignores`. Everything else in the export is indexed by ordinary sniffing.

## What comes back

Every message becomes a document with the decoded plain-text `body` and these fields: `email_from`, `email_to`, `email_cc`, `email_subject`, `email_date`, `email_message_id`, `email_in_reply_to`, `email_references`, `email_thread_id` and `email_labels`.

Two details decide whether a query works:

- **Filter people by address.** `email_from` is the header as written, such as `Alice Anders <alice@example.org>`. It is a keyword, so a `term` on the bare address matches nothing. Use `email_from_address`, `email_to_address` or `email_cc_address`, which hold the bare addresses in lower case.
- **Subject words are in `body`.** `email_subject` is a keyword and matches only the whole subject. The subject is also the first paragraph of the message's first `body` section, so a `match` query on `body` finds its words.

Gmail's `X-GM-THRID` is stored in `email_thread_id` as lower-case hex, the form the Gmail web interface uses. The export writes it in decimal, so a filter on the decimal you see in the mbox finds nothing. Convert it first with `printf '%x\n' 2301773278856733157`, which prints `1ff18a9a0fbde1e5`. `email_labels` holds Gmail's `X-Gmail-Labels`.

Every attachment becomes its own document: one per page for a PDF, sectioned text for a text attachment, and a name/type/size card for anything else. An attachment document carries `attachment_name`, `attachment_content_type` and `attachment_bytes`, plus the parent's message id, subject and sender. A PDF page is titled with the attachment's name.

```sh
curl -s -XPOST http://127.0.0.1:9200/mail-*/_search -H 'content-type: application/json' -d '{
  "query": {"bool": {"filter": [{"term": {"attachment_content_type": "application/pdf"}}],
                     "must": [{"match_phrase": {"body": "INV-2024-0117"}}]}},
  "_source": ["attachment_name", "email_subject", "email_from", "ax_locator"]}'
```

`ax_locator` is positional and stable. `m<offset>-msg-s0` is a message, and `m<offset>-att2-p7-s0` is page 7 of its third attachment. A long body is cut into sections that overlap by a paragraph, so a word in that shared paragraph can come back from two sections of the same message. Collapse on `email_message_id` when you want messages, not sections. Re-running the command overwrites by the same ids instead of duplicating.

## Threads become a graph

The `email-thread@1` detector writes two edge types. A `replies_to` edge comes from the `In-Reply-To` header. When that header is missing or names a message outside the export, the nearest ancestor in `References` is used. An `attachment_of` edge goes from every attachment document to the message that carried it.

Each edge quotes the header it came from. A parent that is not in the export is counted as unresolved and never invented. Subject-line threading is deliberately not attempted.

The edges are a function of the whole corpus, not of what one run re-read. When only one mailbox changed, the run loads the other mailboxes' messages back from the index before it resolves replies. A reply in `Inbox` to a message in an untouched `Sent` therefore survives. The brain is bi-temporal: a superseded edge is kept with `invalid_at` set. To see live edges only, add `"must_not": [{"exists": {"field": "invalid_at"}}]` to an edge query.

## Privacy

- **Message text stays on your machine** with the default configuration. The default embedder is the built-in lexical feature hasher and runs inside the node. autoindex talks only to the node URL you give it.
- **`--embed-mode proxy` changes that.** The node then sends the body text it embeds to the embedding endpoint you configured. Do not use it on private mail unless that endpoint is yours.
- **A field report is public.** The agent instructions in llms.txt ask for one. `xerj feedback` fills its "Pointed at" line from the running node, and that line includes the indexed folder's path. Then it prints the commands that publish the report to the public repository. For private mail, pass `--pointed-at "a private mailbox"` or `--no-autofill`, or file no report. `xerj feedback --dry-run` shows the report before anything is published.
- **`--insecure` means no authentication.** Anyone on the same machine can read the mailbox while the node runs. The section on indexing above shows how to start with a key instead.

## Limits, stated plainly

The mailbox splitter and the message extractor were tested against a generated mailbox. It contains what real exports contain: quoted and unquoted `From ` lines in bodies, 8-bit bodies with and without a declared charset, encoded-word subjects, and a truncated multipart. It also plants a 300 KB line, a duplicated Message-ID, and a final message with no trailing newline.

The Takeout layout rules have only seen that synthetic tree. They are not verified on a real Takeout export. Outlook PST/OST and Maildir have no extractor: convert them to mbox with a tool of your choice first. We have not tested any converter, so we do not recommend one. Undeclared Cyrillic or CJK legacy encodings are not detected.

**Server memory is the limit today.** On the 1 GB synthetic mailbox, `xerj autoindex` itself peaked at 296 MiB and finished in 279 s. All 2,530 planted needles were found in the right message. The node needed **66.9 GiB** of peak memory to get there, with its process cap lifted.

Under the default cap on the same machine (16 GiB), the server sat at its memory watermark from 87 % of the mailbox on. The run aborted after ten minutes of waiting, with 82,422 of 106,581 documents indexed.

A 16 GiB laptop's default cap is 8 GiB. A 300 MB synthetic mailbox run at that cap on the same large machine completed twice, and every needle was found. But the server's peak memory was **18.2 GiB and 19.9 GiB**, which is more than that laptop has.

The breaker stops accepting new work at the cap, but work already accepted keeps growing. We have not run it on a 16 GiB machine. This is the engine's memory while it indexes, filed as [#948](https://github.com/xerj-org/xerj/issues/948).

Until it is fixed, do not count on a 16 GiB laptop for more than a small mailbox. We have not measured where that limit is, only that 300 MB is past it on our machine.

**Searching after a restart is uneven.** We reopened the same 1 GB index on a fresh node under the default cap. Most first-time questions took under 50 ms, and others took several seconds. In two measurements, 16 and 17 of 40 queries took over a second, and the slowest took 16.6 s.

With the cap lifted, 4 of 40 did. Every answer was correct. The machine was shared during those runs, so treat the seconds as upper bounds. This is reported on #948 as well.

Wall time, throughput, memory and index size are in the repository's `benchmarks/mbox-ingest/README.md`, with the machine and the exact commands.
