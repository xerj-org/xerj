---
title: "Why does a folder search miss files on disk?"
h1: "Why would a folder search miss files that I can see on disk?"
description: "XERJ prints one line per ignore rule and stores a refusal reason per file. A capture reconciles 26 files on disk to 1 indexed, 2 refused and 23 excluded."
slug: "why-autoindex-skipped-files"
cluster: "Operations: exclusions"
question: "Why would a folder search miss files that I can see on disk?"
intent: "troubleshooting"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run xerj autoindex with --dry-run against the folder, capture the ignore-rule lines from stderr, then query the autoindex-catalog index for doc_kind file and report the status and reason of every file."
commands:
  - cmd: "xerj autoindex ./mixed --url http://127.0.0.1:9200 --prefix excl --state-dir ./state-excl --dry-run"
    note: "Print every ignore rule with an exact file count, and write nothing."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/autoindex-catalog/_search -H 'content-type: application/json' -d '{\"query\":{\"term\":{\"doc_kind\":\"file\"}},\"size\":200,\"_source\":[\"path\",\"format\",\"status\",\"reason\"],\"sort\":[{\"path\":\"asc\"}]}'"
    note: "Read the per-file status and refusal reason from the catalog index."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/_all/_search -H 'content-type: application/json' -d '{\"query\":{\"query_string\":{\"query\":\"API_KEY\"}},\"size\":10,\"track_total_hits\":true}'"
    note: "Probe every index for a secret that should never have been indexed."
links_out:
  - "search-file-contents-in-a-folder"
  - "check-codebase-index-is-complete"
  - "estimate-autoindex-time-before-running"
faq:
  - q: "Why would a folder search miss files that I can see on disk?"
    a: "An ignore rule pruned them or an extractor refused them. XERJ prints one line per ignore rule and stores a refusal reason for every file it opened."
  - q: "My search tool skipped files that are sitting right there. Why?"
    a: "Either the path was pruned before it was opened, or the file was opened and refused. Ignore rules print to stderr during the run; refusals land in `autoindex-catalog` with a reason field."
  - q: "Does folder search honour gitignore?"
    a: "Yes. Your `.gitignore` is read as an ignore file, and every rule that fires is printed with the count of files behind it, for example `.gitignore:*.tmp — 1 file`."
  - q: "Can I force the indexer to include .env and .ssh files?"
    a: "Not through any flag this capture found. The built-in `hidden:dotfile` rule prunes hidden dotfiles and dot-directories. Copy the file to a non-hidden path you index deliberately instead."
  - q: "Which directories does autoindex prune by default?"
    a: "Built-in rules prune build trees such as target/ and node_modules/, and hidden dotfiles and dot-directories. Your .gitignore adds more, and each rule is printed with its file count."
  - q: "Does autoindex index my .env file?"
    a: "Not in this capture. Hidden dotfiles were pruned, and a query across _all for the fixture secrets returned 0 hits. Ignore rules are a filter, not an access control."
  - q: "What does exit code 3 from autoindex mean?"
    a: "The run finished but refused at least one file. The terminal line reads reason=completed-with-junk, and junk_files counts the refusals."
---

**TL;DR** — XERJ prints one line per ignore rule with an exact file count, and stores a refusal reason for every file it opened. A capture over 26 files on disk indexed 1, refused 2 with named reasons, and excluded 23 through ignore rules. The run exited 3 with `reason=completed-with-junk`.

## Files disappear for 2 different causes

A missing file was either never walked or was opened and refused, and the 2 cases have different evidence trails. Ignore rules print to stderr during the run; refusals land in the `autoindex-catalog` index with a reason string.

| Cause | Where the evidence is | Captured example |
| --- | --- | --- |
| Ignore rule | stderr, one line per rule | `.gitignore:*.tmp — 1 file` |
| Extractor refusal | `autoindex-catalog`, `reason` field | `binary content (zip)` |

## Read the ignore accounting first

A dry run prints the full accounting and writes nothing to the node. Run it before you change any configuration.

```sh
xerj autoindex ./mixed --url http://127.0.0.1:9200 --prefix excl --state-dir ./state-excl --dry-run
```

The captured accounting names every rule and counts the files behind it.

```text
autoindex: ignore rules: skipped 3 files and pruned 3 directories (18 non-hidden files inside them); 1 ignore file read
autoindex: ignore rules:   <built-in>:target/ — 1 directory pruned (12 non-hidden files inside)
autoindex: ignore rules:   .gitignore:scratch/ — 1 directory pruned (6 non-hidden files inside)
autoindex: ignore rules:   hidden:dotfile — 2 files, 1 directory pruned
autoindex: ignore rules:   .gitignore:*.tmp — 1 file
```

Every line names its own rule source. This capture shows `<built-in>` for build trees, `hidden:dotfile` for dotfiles and dot-directories, and `.gitignore` for your own rules. Other sources get their own line when they apply: `.xerjignore`, and the `symlink:` rules that refuse a followed link.

## Then read the per-file catalog

`autoindex` writes one catalog document per file, with the format it detected, the status it assigned, and the reason for any refusal. Query it directly.

```sh
curl -s -XPOST http://127.0.0.1:9200/autoindex-catalog/_search -H 'content-type: application/json' -d '{"query":{"term":{"doc_kind":"file"}},"size":200,"_source":["path","format","status","reason","records"],"sort":[{"path":"asc"}]}'
```

The capture returned 3 files that reached the catalog, and 2 of them carry a reason.

| Path | Format | Status | Reason |
| --- | --- | --- | --- |
| `README.txt` | `txt-prose` | `indexed` | none |
| `assets/logo.bin` | `binary` | `junk` | `binary content (unknown)` |
| `books/manual.epub` | `binary` | `junk` | `binary content (zip)` |

An unsupported format is refused as binary rather than partially extracted. The EPUB above has no extractor, so XERJ detects a zip container and stops.

## The arithmetic that closes the gap

26 files sat on disk, 22 of them non-hidden. 18 lived inside pruned directories, 3 more matched a skip rule, 2 were refused as binary, and 1 was indexed. Every path has a rule name or a reason string attached to it.

Publish that arithmetic in your own runbook. A folder that "lost" files almost always has a `.gitignore` entry or a build directory behind the difference.

## Secrets and hidden files

The built-in `hidden:dotfile` rule prunes hidden dotfiles, and the fixture `.env` never reached an index. A `query_string` probe across `_all` for the secrets inside it returned 0 hits.

Treat that as a default, not as a guarantee. Ignore rules are a content filter, and XERJ is single-node, so anything you do index sits in one process on one host.

## The exit code carries the verdict

The captured run ended with a terminal line that names both the outcome and the refusal count.

```text
xerj-done ok=true exit=3 reason=completed-with-junk wall=0.8s files=1 records=2 datasets=1 junk_files=2
```

Exit 0 with `reason=completed` means nothing was refused. Exit 3 with `reason=completed-with-junk` means the catalog holds at least one reason worth reading.
