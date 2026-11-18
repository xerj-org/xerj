---
title: "How do I read email and PDFs after indexing a folder?"
h1: "I indexed a folder of email and PDFs. How do I actually read what it found?"
description: "xerj brain indexes the folder and the console opens any record: an email with its attachments, a PDF page, and the files it links to. Extracted text only."
slug: "read-indexed-email-and-pdf-folder"
cluster: "Files and formats"
question: "I indexed a folder of email and PDFs. How do I actually read what it found?"
intent: "how-to"
published: "2026-09-18"
updated: "2026-10-30"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run `xerj brain ./casefile --no-open` on a folder of .eml and .pdf files, search the index it reports for a word from one email subject, then give the person the console link `/_xerj-console/#/reader?index=INDEX&id=ID` for the best hit instead of pasting the whole document into the chat."
answer: "I indexed a folder of email and PDFs; how do I actually read what it found? Run xerj brain on the folder and open the console it prints. The Corpus page lists what was indexed, and the Reader opens an email with headers, body and attachments, or one PDF page. It shows extracted text only."
key_fact: "In the recorded run, xerj brain turned 10 files into 81 records in one dataset and wrote 33 links into the brain."
key_fact_source: "docs/usecases/console-reader/live-e2e.json"
key_fact_date: "2026-10-19"
intent_group: "logs and docs"
queries:
  - "index pdf folder full text search offline"
commands:
  - cmd: "xerj brain ./casefile --no-open"
    note: "Index the folder, detect the links between files, and print the console URL and the one-time passkey setup link."
  - cmd: "curl -s -XPOST http://localhost:9200/ax-docs/_search -H \"Authorization: ApiKey $XERJ_API_KEY\" -H 'content-type: application/json' -d '{\"query\":{\"bool\":{\"filter\":[{\"wildcard\":{\"email_subject\":{\"value\":\"*invoice*\",\"case_insensitive\":true}}}],\"must_not\":[{\"exists\":{\"field\":\"attachment_name\"}}]}},\"_source\":[\"email_subject\",\"email_from\",\"email_date\",\"ax_file\"],\"size\":5}'"
    note: "Find an email by a word in its subject. The _id of a hit goes into the Reader link, and its attachments share its ax_file."
links_out:
  - "search-all-pdfs-in-a-folder"
  - "search-obsidian-pdf-docx-attachments"
  - "how-xerj-autoindexes-a-folder"
  - "knowledge-graphs-for-agent-memory"
  - "/docs/security"
  - "/answers/search-word-documents-in-a-folder"
  - "/answers/share-folder-read-only-search-without-uploading"
  - "/compare/xerj-vs-localsynapse"
evidence:
  - claim: "In the recorded run xerj brain turned 10 files into 81 records in one dataset, ax-docs, and wrote 33 links into the brain casefile."
    source: "docs/usecases/console-reader/live-e2e.json"
  - claim: "The shared index held 6 emails and 43 attachment records; the guest key got 200 on its own index and on the brain, 403 on another index, on autoindex-catalog and on a write, and 401 from the console API; the guest tab made 0 console API requests."
    source: "docs/usecases/console-reader/live-e2e.json"
  - claim: "The graph panel of the hostile email showed 2 linked records, both same_dir links of the file the email came from; the signed-in operator's graph panel was refused with HTTP 401 on the auth-enabled node."
    source: "docs/usecases/console-reader/live-e2e.json"
  - claim: "On an index where autoindex typed email_subject as keyword, match on email_subject for Lunch returned 0 hits with the subject Lunch on Friday? indexed, and the case-insensitive wildcard *lunch* returned 1."
    source: "docs/usecases/console-reader/review-repro/review-repro.json"
  - claim: "The console page is served with a Content-Security-Policy whose script-src and connect-src are 'self'."
    source: "engine/crates/xerj-console-api/src/spa.rs"
  - claim: "A guest session can build only _search, _count, _mapping and ego requests, and fails closed on a malformed or expired record."
    source: "xerj-ux/src/data/guest.js"
faq:
  - q: "I indexed a folder of email and PDFs. How do I actually read what it found?"
    a: "Open the console the run printed. The Corpus page lists what was indexed, and the Reader opens any record by id: an email with its headers, body text and attachment list, or one page of a PDF."
  - q: "Does the Reader show the original PDF or the HTML version of an email?"
    a: "No. It shows the text the extractor indexed. XERJ stores extracted text, not attachment bytes, and the Reader never renders email HTML. An HTML body is displayed as its source."
  - q: "How are an email and its attachments connected in the index?"
    a: "Every record that came out of one .eml file carries the same ax_file, and that is what the Reader joins on. A PDF attachment becomes one record per page section, so the Reader lists each attachment once. The email_message_id field is also copied to the attachments, but only when the message has a Message-ID header."
  - q: "Why does the panel of linked records say the links belong to the file?"
    a: "The same-folder, Markdown-link and path-citation detectors link file records, not the records inside a file. The Reader reads the links of the email's file as well and marks them FILE."
  - q: "Is it safe to open a hostile email in the Reader?"
    a: "The Reader builds the page from text nodes, never from markup, and links no URL found in a document. In the recorded run a script-bearing subject and attachment name were displayed as text and nothing executed. That is a tested design, not a guarantee against every browser bug."
  - q: "Why does the linked-records panel say the graph API refused my session?"
    a: "On a node with auth on, which is the default, the console holds a passkey session and the graph API wants an API key. Search, records and attachments still work. The panel works on a node started with --insecure."
  - q: "Can I let someone else read one index without giving them my node?"
    a: "The console has a read-only guest mode driven by a share record that names one key, the shared indices, an optional brain and an expiry. It can only search and read those indices, and it logs out on expiry."
---

**TL;DR** Run `xerj brain <folder>` and open the console it prints. Corpus lists what was indexed. The Reader opens one email with its attachments, or one PDF page, as extracted text.

```visual
email-reader-flow
```

## Index the folder, then open the console

`xerj brain <folder>` starts a local node if none is running, indexes every readable file, detects the links between files and prints a console URL. A first run also prints a one-time passkey setup link. The node it starts has auth on, and its admin key is in `<data-dir>/admin.key`.

## Corpus lists what was indexed

Corpus is the console's landing page. It reads `autoindex-catalog` and shows one card per dataset: record, file and byte counts, formats, date range, fields with types, and sample queries. A sample query opens the Reader with that search already run.

The record count is the index total. File count, formats and field types come from the last run over that dataset, and the card labels them `last run`. An empty node shows one command, `xerj brain <folder>`, and no sample data.

## The Reader opens one record

The route is `/_xerj-console/#/reader?index=<index>&id=<id>`. Every hit in Discover links there, so an agent can hand a person that link instead of pasting a document into a chat.

```visual
email-reader-shapes
```

The second command above finds an email by a word in its subject. It uses `wildcard`, not `match`. `xerj autoindex` usually types `email_subject` as `keyword`, and on a `keyword` field `match` finds only the whole subject. In a recorded check, `match` for `Lunch` returned 0 hits on the subject `Lunch on Friday?`, and the `wildcard` for `*lunch*` returned 1. The `must_not` drops attachment records, which each carry a copy of the subject.

## How one email is stored

`xerj autoindex` writes one record for the file, one for the message and one for each page section of each attachment. All of them share `ax_file`. In the recorded run the shared index held 6 emails and 43 attachment records. To list one email's attachments, filter on its `ax_file`:

```sh
curl -s -XPOST 'http://localhost:9200/ax-docs/_search' \
  -H "Authorization: ApiKey $XERJ_API_KEY" -H 'content-type: application/json' \
  -d '{"query":{"bool":{"filter":[{"term":{"ax_file":"AX_FILE_OF_THE_EMAIL"}},{"exists":{"field":"attachment_name"}}]}},"_source":["attachment_name","page","ax_locator"],"size":50}'
```

Attachment records also copy `email_message_id` without the angle brackets, but only when the message has a `Message-ID` header, and two files can carry the same id. If one email has more than 50 records, raise `size` or add `from`.

## Linked records come from the brain

Under the record, the Reader shows its one-hop neighbours from `GET /_graph/{brain}/ego`, grouped by link type. The detectors link file records, so the Reader also reads the links of the record's file and marks them `FILE`. In the recorded run the hostile email had 2 linked records, both same-folder links of its file. The detectors are structural and read no meaning from the text.

## Text on purpose

A subject, an attachment name and a body are text a stranger wrote. The Reader builds its page from text nodes, renders no email HTML and links no URL found in a document. The console's Content-Security-Policy allows scripts and connections from its own origin only.

The recorded run includes a hostile message with a `<script>` in the subject, an `<svg onload>` in the sender name and a PDF attachment whose name carries an `onerror` payload. The Reader showed all of it as text, nothing executed, and no request left the origin. A control page that renders the same subject through `innerHTML` does get compromised, so the fixtures are live.

## Two limits with auth on

The linked-records panel is refused for a signed-in operator on a node with auth on, which is the default. The console holds a passkey session and the graph API wants an API key. Search, records and attachments still work, and the panel works on a node started with `--insecure`.

Passkey sign-in works only at `http://localhost:9200`. A node on another port refuses the enrolment. Both limits are tracked as #935 and #936.

## Read-only guest mode

A share link hands the console one record: a read-only API key, the shared index names, an optional brain, an expiry and a label. The console then opens a separate read-only shell with Corpus and the Reader for those indices, under a banner with the expiry time.

```visual
email-guest-access
```
