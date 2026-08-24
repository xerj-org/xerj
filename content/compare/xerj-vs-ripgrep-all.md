---
title: "ripgrep-all compared with XERJ for PDFs and docs"
h1: "How do I grep PDFs and Word docs from the command line?"
description: "ripgrep-all wraps ripgrep with adapters for PDF, Office files, archives and SQLite. XERJ indexes the same folder for an agent. A capability comparison, no benchmark."
slug: "xerj-vs-ripgrep-all"
cluster: "Comparison: desktop search"
question: "How do I grep PDFs and Word docs from the command line?"
intent: "comparison"
published: "2026-08-22"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent with a folder of PDFs and Word documents. Read https://xerj.org/llms.txt, decide between a one-shot rga search and an index, then start a local XERJ node, run xerj autoindex on the folder and answer with a match_phrase query that cites the ax_path of every hit."
commands:
  - cmd: "xerj autoindex ./documents --url http://127.0.0.1:9200 --prefix docs --progress plain"
    note: "Index the folder once, so later questions do not parse the PDFs again."
  - cmd: "xerj autoindex map --url http://127.0.0.1:9200"
    note: "Print the data map, so the next query names a real index and a real field."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/docs-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"invoice number\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\"]}'"
    note: "Ask the phrase question over HTTP and read the file path from every hit."
  - cmd: "xerj mcp --url http://127.0.0.1:9200"
    note: "Serve the same node to an agent as a stdio MCP server."
links_out:
  - "/answers/search-file-contents-in-a-folder"
  - "/answers/search-all-pdfs-in-a-folder"
  - "/answers/full-text-search-sqlite-database"
  - "compare/xerj-vs-recoll"
  - "xerj-vs-ripgrep-for-code-agents"
evidence:
  - claim: "ripgrep-all wraps ripgrep and adds adapters for PDF (poppler), Office and e-book formats (pandoc), zip, tar, compressed files, media metadata (ffmpeg) and SQLite databases."
    source: "https://github.com/phiresky/ripgrep-all"
  - claim: "ripgrep-all recursively descends into archives and matches text in every file type it knows, with a default maximum archive recursion depth."
    source: "https://github.com/phiresky/ripgrep-all"
  - claim: "The ripgrep-all mail adapter for mbox, mbx and eml files is disabled by default and is turned on with --rga-adapters=+mail."
    source: "https://github.com/phiresky/ripgrep-all"
  - claim: "ripgrep-all caches the extracted text of each file in a database under the user cache directory, and --rga-no-cache turns that off."
    source: "https://github.com/phiresky/ripgrep-all"
  - claim: "ripgrep-all 0.10.3 added a config file with custom subprocess-spawning adapters, which is how a user adds a converter such as an OCR program."
    source: "https://github.com/phiresky/ripgrep-all/blob/master/CHANGELOG.md"
  - claim: "ripgrep is a line-oriented search tool that respects .gitignore and skips hidden and binary files by default."
    source: "https://github.com/BurntSushi/ripgrep"
  - claim: "grep is a line matcher over text files and has no document adapters of any kind."
    source: "https://www.gnu.org/software/grep/manual/grep.html"
  - claim: "Recoll indexes email and can run OCR on image-only PDF documents through tesseract or ABBYY FineReader."
    source: "https://www.recoll.org/usermanual/usermanual.html"
  - claim: "Elasticsearch is a distributed engine with cross-node replication and failover, which a single-node engine does not provide."
    source: "https://www.elastic.co/docs/deploy-manage/distributed-architecture"
faq:
  - q: "Is ripgrep-all enough or do I need a real index?"
    a: "ripgrep-all is enough while the folder is small and the questions are few. An index pays for itself when the same folder answers many questions, because the PDFs are parsed once instead of once per search."
  - q: "What's the difference between rga and a local search engine?"
    a: "rga converts each file and matches a regular expression over the text. A search engine parses the folder once into typed documents, then ranks and filters them, and answers over an API."
  - q: "How do I search SQLite and PDFs without building an index?"
    a: "Run `rga` with its sqlite and poppler adapters. Install poppler-utils and pandoc first, because rga calls those programs for the conversion."
  - q: "Did you run a head-to-head benchmark between XERJ and ripgrep-all?"
    a: "No. No shared corpus was frozen and no hit counts or timings were measured, so this page publishes documented capabilities and no win counts."
  - q: "Which tool searches inside zip and tar archives?"
    a: "ripgrep-all. It descends into zip, tar and compressed files up to a default depth. XERJ has no archive handler and never opens a zip or a tar. A single gzipped file is the exception: `autoindex` decompresses `.gz` on every parsed family."
  - q: "Can ripgrep-all read image-only PDFs?"
    a: "Not with the shipped adapters. Its config file accepts custom subprocess adapters, so an OCR program can be added by hand. XERJ has no OCR at all."
---

**TL;DR** — ripgrep-all is the better choice for one question, today. It converts PDFs and Office files as it goes, walks into archives, and wants no daemon and no index. XERJ indexes the same folder once for an agent to query many times. No head-to-head benchmark was run.

## No benchmark was run, and this page says so

We did not freeze a shared corpus. We did not install ripgrep-all next to XERJ. We measured no hit counts, no recall and no latency.

There is no win count on this page. No run stands behind one.

Every ripgrep-all statement below comes from its own README and changelog. Every XERJ statement is a documented capability of the binary.

If you want numbers, take your own folder. Run both tools on it and count the files each one names. That is the only comparison that describes your documents.

## What ripgrep-all is

ripgrep-all, called `rga`, wraps ripgrep. ripgrep matches a regular expression over lines of text. rga puts a converter in front of it. The same regular expression then reaches a PDF or a Word file.

The converters are called adapters. Each adapter claims a set of file extensions, and rga runs it before the search.

| adapter | what it reads | how |
| --- | --- | --- |
| poppler | PDF | `pdftotext` from poppler-utils |
| pandoc | epub, odt, docx, fb2, ipynb, html | `pandoc` to plain text |
| ffmpeg | mkv, mp4, avi, mp3, ogg, flac, webm | metadata, chapters and subtitles |
| zip | zip, jar | reads the archive as a stream |
| tar | tar | reads the archive as a stream |
| decompress | gz, bz2, xz, zst | unpacks, then runs another adapter |
| sqlite | db, sqlite, sqlite3 | prints the tables as plain text |
| mail | mbox, mbx, eml | opt-in, `--rga-adapters=+mail` |

Two flags matter for a mixed folder. `--rga-accurate` picks the adapter from the file contents rather than the file extension. `--rga-adapters=+mail` turns on the mail adapter, which is off by default.

rga is not stateless. rga caches the extracted text of each file under the user cache directory. A second search over the same PDFs does not convert them again. `--rga-no-cache` turns the cache off.

## What XERJ is

XERJ is a single Rust binary that runs one search node. `xerj autoindex <folder>` reads the folder and detects each file family by content. It infers a dataset per file shape and writes the documents.

The command takes no configuration file and no mapping. It also needs no helper program on the host, because the binary parses the formats in process.

The node answers the Elasticsearch REST API. `xerj autoindex map` prints a data map of what the folder became. An agent reads that map, then names a real index and a real field.

`xerj mcp` is a stdio MCP server in the same binary. It serves 10 tools against a node you already started. Agent memory lives in the engine under `/_memory/{namespace}`.

XERJ is single-node. There is no replication and no failover. The default embedder is lexical feature hashing, not a neural model, and neural retrieval is opt-in through `--embed-mode neural`.

## Capabilities, side by side

Every row is a documented capability of each tool. No row is a measured result.

| capability | ripgrep-all | XERJ |
| --- | --- | --- |
| regular expressions over raw text | yes, ripgrep does the matching | no, the query language is analyzed terms |
| a substring inside a word | yes | no, an analyzed index matches whole terms |
| start cost before the first answer | none, no index and no daemon | one `xerj autoindex` run |
| repeat questions on the same folder | converts once, then reads its cache | reads the index |
| archives, and archives inside archives | yes, zip, tar and compressed files | no archive handler |
| a single gzipped file | yes, the decompress adapter | yes, gzip is transparent on every parsed family |
| email files | opt-in mail adapter | no email handler |
| OCR for image-only PDFs | not shipped, custom adapter only | none |
| helper programs on the host | pandoc, poppler-utils, ffmpeg | none |
| ranking of results | none, matches in file order | BM25 over the extracted text |
| filter by file family or path | shell globs and ripgrep flags | keyword fields, `ax_format` and `ax_path` |
| HTTP query API | none | yes, Elasticsearch REST API |
| MCP server for an agent | none | yes, `xerj mcp`, 10 tools |
| catalog of refused files | messages on standard error | `autoindex-catalog`, one reason per file |
| agent memory in the engine | none | yes, `/_memory/{namespace}` |

## When to choose ripgrep-all instead

Choose ripgrep-all for one question, today. There is no index to build and no node to start. The answer costs one command.

Choose ripgrep-all when the pattern is a real regular expression. An analyzed index matches whole terms. A run of characters inside a word has nothing to match. ripgrep reads bytes and ignores where a word starts.

Choose ripgrep-all when the documents sit inside archives. It walks into zip, tar and compressed files, and into an archive inside an archive. XERJ has no archive handler, so a zip or a tar never reaches a XERJ index.

A single gzipped file is the one exception on the XERJ side. `xerj autoindex` detects gzip by content and decompresses it during indexing, on every parsed family. A `.jsonl.gz` log therefore lands beside the plain file next to it.

Choose ripgrep-all when the folder holds mail files. The opt-in mail adapter reads mbox, mbx and eml. XERJ has no email handler.

Choose ripgrep-all when you must add a converter of your own. Custom subprocess adapters are configuration, so an OCR program can sit in front of the search. XERJ has no OCR and no adapter interface.

Choose ripgrep-all when the files change under you. It converts the file it is looking at, and XERJ answers from the last `xerj autoindex` run.

## When to choose grep or ripgrep instead

Choose grep or ripgrep instead when the folder is plain text. Both are line matchers with no conversion step, and neither wants an adapter or a node.

For code trees the same argument continues on the [ripgrep comparison for code agents](/compare/xerj-vs-ripgrep-for-code-agents), which publishes the tasks where ripgrep names files an index does not.

Choose Recoll instead when a person wants a GUI, email indexing or OCR. The [Recoll comparison](/compare/xerj-vs-recoll) covers that trade.

Choose Elasticsearch instead when the documents must live on more than one host. XERJ has no replication and no failover. XERJ speaks the same REST API on one node.

## When XERJ is the better fit

Choose XERJ when the same folder answers many questions. The parse happens once, and every later query reads the index rather than the PDFs.

Choose XERJ when the caller is a program. An agent that speaks HTTP or MCP needs no wrapper and no output parser. `xerj mcp` serves 10 tools.

Choose XERJ when the order of the answers matters. rga returns every match in file order. BM25 puts the best documents first.

Choose XERJ when the answer must carry provenance. Every document carries `ax_path`, `ax_file`, `ax_format` and four more keyword fields, so an agent can cite the source file.

Choose XERJ when a refused file must be explainable. Files that XERJ does not parse land in `autoindex-catalog` with a reason string, rather than passing in silence.

## What XERJ does not have

XERJ has no regular expression engine over raw bytes. The index matches analyzed terms and phrases.

XERJ has no archive handler, no email handler and no OCR. It has no adapter interface, so a format that the binary does not parse stays out of the index.

XERJ runs on one node. There is no failover. Plan for restore from a copy.

## How to read this page

This is a capability comparison drawn from each tool's own documentation. It is not a benchmark, and it is not a claim about your files.

The honest summary is short. rga fits a low question count and odd formats. XERJ fits a program that asks the folder many questions.

The [folder search walkthrough](/answers/search-file-contents-in-a-folder) shows the XERJ side in full.
