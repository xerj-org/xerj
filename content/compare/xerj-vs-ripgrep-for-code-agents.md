---
title: "XERJ compared with ripgrep for code agents"
h1: "Is ripgrep enough for a coding agent, or do I need a code index?"
description: "30 disclosed tasks on one 331-file code tree. The two tools agree on 18. ripgrep wins 5 named tasks outright, including a substring inside an identifier."
slug: "xerj-vs-ripgrep-for-code-agents"
cluster: "Comparison: code retrieval"
question: "Is ripgrep enough for a coding agent, or do I need a code index?"
intent: "comparison"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, index one code tree with xerj autoindex, then run the same 30 literal, phrase and symbol tasks through ripgrep and through the index, reduce both sides to the set of distinct files, publish every disagreement in both directions, and say which tool you would reach for first."
commands:
  - cmd: "xerj autoindex ./crates --url http://127.0.0.1:9510 --prefix c07 --state-dir ./state-c07 --progress plain --disable-feedback"
    note: "Build the index whose cost ripgrep never pays."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9510/c07-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"defs\":\"Ctx\"}},\"size\":100,\"_source\":[\"ax_path\"],\"track_total_hits\":true}'"
    note: "The symbol-definition query, read from the returned hits and not from an aggregation."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9510/c07-*/_search' -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"data dir\"}},\"size\":1000,\"_source\":[\"ax_path\"],\"track_total_hits\":true}'"
    note: "The phrase task where ripgrep named 49 files and the index named 37."
links_out:
  - "index-monorepo-for-agent"
  - "syntax-aware-code-search-refactoring"
  - "/answers/code-search-mcp-for-claude-code"
evidence:
  - claim: "ripgrep is a line-oriented recursive search tool that respects .gitignore and skips hidden and binary files by default, which is why its file sets and the index file sets can differ on the same tree."
    source: "https://github.com/BurntSushi/ripgrep"
faq:
  - q: "Should a code agent use ripgrep or an indexed search?"
    a: "Use ripgrep for one-off literals and substrings on a tree that changes constantly. Use an index when the same tree is queried hundreds of times."
  - q: "How often did the two tools agree?"
    a: "On 18 of 30 tasks the file sets matched exactly. The other 12 disagreements are published in both directions in the capture."
  - q: "When should I choose ripgrep instead?"
    a: "Choose ripgrep for substrings inside identifiers, for regular expressions over raw bytes, for tiny trees, and for files that changed 1 second ago."
  - q: "Which task did ripgrep win most clearly?"
    a: "T25. Searching for `ompat_por`, a substring inside `es_compat_port`, ripgrep named 7 files and the index named 0."
  - q: "Why can an index not find a substring inside an identifier?"
    a: "Because an index matches tokens. The analyzer splits text into whole terms, so a run of characters that starts mid-token is not a term and cannot match."
  - q: "Where did the index win?"
    a: "Symbol definitions and analyzed phrases. On T22 one `defs` query found 2 files where a Rust-specific regular expression found 1."
  - q: "Are the timings in this comparison a benchmark?"
    a: "No. The binary is a `ci-test` build, ripgrep's number includes process start, and XERJ's excludes the cost of running a node. Do not quote either as a benchmark."
  - q: "How many queries before the index pays for itself?"
    a: "Order of magnitude, several hundred on this tree. The derived figure was about 713 queries, and it ignores index memory, disk and staleness."
---

**TL;DR** — Keep ripgrep for one-off questions and reach for the index when an agent asks one tree many questions. On one 331-file code tree the two named the same files on 18 of 30 disclosed tasks. Five of the 30 went to ripgrep 14.1.1 outright. On a substring inside an identifier it found 7 files and the index found 0.

## What we compared, and on what

The corpus is a snapshot of this repository's `engine/crates` tree: 331 files, 13,677,241 bytes, tree SHA-256 `230298e5…`. Of those files, 315 are Rust and 16 are TOML. The tree is real first-party source, not a generated fixture.

The competing tool is ripgrep 14.1.1 from `/usr/bin/rg`, with PCRE2 10.43 available. XERJ ran as 1 single-node process over the same tree.

The unit of comparison is the **set of files each tool names**. ripgrep reports lines and XERJ reports documents. The harness reduces both sides to distinct files, and every file set below comes from the returned hits.

## The cost ripgrep never pays

One cold `xerj autoindex` run over the snapshot took 5.717 seconds and refused nothing.

```text
xerj-done ok=true exit=0 reason=completed wall=5.6s files=331 records=674 datasets=1 junk_files=0 code_files=315 code_files_indexed=315 code_files_junked=0
```

ripgrep has no build step, holds no state and needs no running process. That asymmetry is the whole decision, and every result below sits underneath it.

## The headline: 18 of 30 agree

The 2 tools returned exactly the same file set on 18 of the 30 tasks. The capture publishes all 12 disagreements with both sides listed.

| outcome | tasks |
| --- | --- |
| file sets identical | 18 |
| ripgrep named files the index did not | 5 |
| the index named files ripgrep did not | 7 |

Those 2 disagreement rows overlap, because several tasks disagree in both directions at once.

## Where ripgrep wins

ripgrep won 5 named tasks. Every row below is a measured result, not a courtesy paragraph.

| task | what we asked for | ripgrep | XERJ |
| --- | --- | --- | --- |
| T25 | the substring `ompat_por` | **7 files** | **0 files** |
| T28 | `hnsw` regardless of case (`rg -i -F`) | **19 files** | 16 files |
| T10 | the literal token `es_compat_port` | **7 files** | 6 files |
| T11 | the literal token `autoindex` | **51 files** | 48 files |
| T17 | the phrase `data dir` | **49 files** | 37 files |

T25 is the structural point. `ompat_por` is a run of characters inside `es_compat_port`. ripgrep is a byte matcher and does not care about token boundaries.

An analyzed index does care. The analyzer emits whole terms, so a query that starts mid-term has nothing to match and returns 0. No configuration fixes that.

## Why T11, T17 and T28 went ripgrep's way

The misses in those 3 tasks are mostly files whose family is not `code`. A `Cargo.toml` file carries both the token `autoindex` and the token `hnsw`. Each lands in a family whose fields differ from a source file's.

```text
T11 ripgrep-only: xerj-autoindex/Cargo.toml, xerj-engine/Cargo.toml, xerj-server/Cargo.toml, ...
T28 ripgrep-only: xerj-api/tests/sq8_codes_follow_document_updates.rs, xerj-common/src/types.rs, xerj-vector/Cargo.toml
```

ripgrep reads bytes and does not know what a family is. On a mixed tree that is an advantage, and on this tree it cost the index 3 files on T28 and 3 on T11.

## Where the index wins

The index won on symbol definitions and on analyzed phrases.

| task | what we asked for | ripgrep | XERJ |
| --- | --- | --- | --- |
| T22 | where `Ctx` is defined | 1 file | **2 files** |
| T20 | where `run` is defined | 7 files | **26 files** |
| T15 | the phrase `vector search` | 3 files | **6 files** |
| T26 | the alternation `(HNSW\|IVF\|PQ)` | 17 files | **24 files** |
| T18 | the phrase `index not found` | 3 files | **4 files** |

For T22 and T20 ripgrep needs a language-specific regular expression, here `struct\s+Ctx` and `pub\s+async\s+fn\s+run`. XERJ queries the extracted `defs` field, which is language-independent and covers 34 languages with one query.

```sh
curl -s -XPOST 'http://127.0.0.1:9510/c07-*/_search' -H 'content-type: application/json' -d '{"query":{"match":{"defs":"Ctx"}},"size":100,"_source":["ax_path"],"track_total_hits":true}'
```

## A bigger number is not a better answer

T27 asked for the operator sequence `=>`. ripgrep named 206 files and the index named 315, a strict superset of ripgrep's list.

That is not a win. Analyzers discard punctuation, so the index answered a different question and named every file with anything at that position.

Read T26 the same way. An alternation query over analyzed terms is not the regular expression that ripgrep ran.

## Read the hits, never the aggregation

A terms aggregation on `ax_path` disagreed with the returned hits on 16 of the 30 tasks. On one `match_phrase` query the aggregation returned 0 buckets while 11 documents in 11 distinct files matched.

| query type | documents matched | aggregation buckets |
| --- | --- | --- |
| `match_all` | 674 | 331 |
| `exists` | 315 | 315 |
| `term` | 630 | 315 |
| `match_phrase` | 11 | **0** |
| `match` | 11 | 74 |

Our first run of this comparison derived the file sets from that aggregation and reported 5 of 30 agreeing. Reading the hits instead gives 18 of 30, and we publish only the corrected run.

## The timings are not benchmarks

We measured latency, and the numbers are not publishable as performance figures. State the method before the number.

Per task the harness discarded 2 warmup runs, then timed 10. The median of the per-task p50 values was ripgrep 8.909 ms and XERJ 0.887 ms. The host 1-minute load average was 3.07 before and 2.35 after.

Three things disqualify those figures as a benchmark. The binary is a `--profile ci-test` build with link-time optimization off. ripgrep's number includes process start. XERJ's number is 1 HTTP round trip, and it excludes the cost of a live node.

## The break-even, as an order of magnitude

Divide the index build by the per-query difference and the answer is about 713 queries on this tree. Treat that as *several hundred*, never as a precise figure.

It also ignores index memory, index disk and index staleness. All 3 favor ripgrep, and none of them is in the arithmetic.

## When to choose ripgrep instead

Choose ripgrep when the query is a substring inside an identifier. T25 is the proof: ripgrep 7 files, the index 0, with no setting that closes the gap.

Choose ripgrep for real regular expressions over raw bytes. Choose it for case-insensitive literals across mixed file types, and for a tree that changed 1 second ago. An index answers from the last run, and ripgrep answers from the disk.

Choose ripgrep for tiny trees and for one-off questions. Several hundred queries against one snapshot is the threshold. Under it, the 5.717-second build and a live node have no return.

## When the index is worth its cost

Choose the index when an agent asks one tree many questions per session. Choose it when the questions are about symbols rather than bytes. One `defs` query replaces a per-language regular expression and covers 34 languages.

Choose the index when you want structure alongside text. The index carries `language`, `symbol_count`, aggregations and `_source` narrowing. ripgrep returns lines, and lines are all it returns.

## The limits you inherit either way

XERJ is single-node here and everywhere. There is no replication and no failover, so the index is exactly as durable as the 1 host it lives on.

Extraction is definitions only, with no call graph and no cross-file reference resolution. XERJ never reports who calls a function, and neither does ripgrep.

## What this comparison is and is not

One tree, 331 files, 30 disclosed tasks, 1 cold index build and 10 timed runs per task on a shared host. The task list, both file sets and every disagreement are in the capture, so the parts you disagree with are checkable.

It is not a benchmark, not a language-coverage test, and not a claim about any tree but this one.
