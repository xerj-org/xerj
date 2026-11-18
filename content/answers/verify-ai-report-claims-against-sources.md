---
title: "Check report claims against saved sources"
h1: "Verify claims in a report against the saved sources"
description: "Index the saved sources and run one match_phrase per claim. On 2026-10-20, 3 of 5 phrases were in the docs index and 2 were not."
slug: "verify-ai-report-claims-against-sources"
cluster: "Claim checks"
question: "Verify claims in a report against sources"
intent: "how-to"
published: "2026-10-20"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
agent_prompt: "Read https://xerj.org/llms.txt. Start one local node, index the saved source files, and run one match_phrase query per claim on the prose field from xerj autoindex map. Mark a claim found only when a hit comes back, and name ax_path. A miss means the words were not in that field."
answer: "Verify claims in a report against the saved sources. Index those files, then run one `match_phrase` per claim. A claim with no hit is unverified. On 2026-10-20, 3 of 5 phrases were in the docs index and 2 were not. The first check took 490 ms."
key_fact: "3 of 5 body phrases were found in the docs index; the first check took 490 ms."
key_fact_source: "benchmarks/agent-queries-2026-10-20/08-claims.jsonl"
key_fact_date: "2026-10-20"
intent_group: "Claim checks"
queries:
  - "verify claims in report against sources automatically"
  - "fact verification against source document tool"
  - "check llm output is supported by context"
  - "mark unverified facts in research output"
  - "attribute answer to source passage"
commands:
  - cmd: "xerj autoindex --url http://127.0.0.1:9217 --yes --no-graph --progress plain --disable-feedback /workspace/docs --prefix docs --state-dir /tmp/xerj-aq-2026-10-20/state-docs"
    note: "Index the saved sources. This docs folder exited 3 with 9,940 records."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/docs-docs/_search -H 'content-type: application/json' -d '{\"size\":1,\"_source\":[\"ax_path\",\"title\"],\"query\":{\"match_phrase\":{\"body\":\"the primary user is an AI agent\"}}}'"
    note: "One claim, one phrase query. Found in WHY_XERJ.md, took 490 ms."
  - cmd: "curl -s -XPOST http://127.0.0.1:9217/docs-docs/_search -H 'content-type: application/json' -d '{\"size\":1,\"_source\":[\"ax_path\"],\"query\":{\"match_phrase\":{\"text\":\"the primary user is an AI agent\"}}}'"
    note: "The same words on the wrong field. 0 hits, took 0."
links_out:
  - "how-xerj-autoindexes-a-folder"
  - "search-file-contents-in-a-folder"
  - "answer-questions-about-a-docs-folder-with-citations"
evidence:
  - claim: "The first body phrase took 490 ms and the second took 485 ms."
    source: "benchmarks/agent-queries-2026-10-20/08-claims.jsonl"
  - claim: "Three later body phrases took 1 ms each."
    source: "benchmarks/agent-queries-2026-10-20/08-claims.jsonl"
  - claim: "The docs folder index finished in 14.3 s."
    source: "benchmarks/agent-queries-2026-10-20/02-autoindex-docs.txt"
og_image: "og/answers-verify-ai-report-claims-against-sources.png"
og_image_alt: "Cover: five horizontal rows, three filled ochre and two hollow, for 3 phrases found and 2 missed."
faq:
  - q: "Verify claims in a report against sources?"
    a: "Index the sources you saved. Run one `match_phrase` per claim on the prose field. A hit names `ax_path`. No hit means unverified."
  - q: "Does a hit prove the claim is true?"
    a: "No. It proves those words occur together in the field. The sentence can still be a bad reading of the file."
  - q: "What about a reworded claim?"
    a: "This capture missed `Content sniffing never trusts the extension` even though the file talks about sniffing. The words have to be the words on disk."
  - q: "Why did the same phrase miss on text?"
    a: "On `docs-docs` the sentence was in `body`. The same five phrases on `text` returned 0 hits. Read the map before you pick the field."
  - q: "Is this a faithfulness score?"
    a: "No. There is no score for how well a report follows its sources. You get found or not found, plus the path."
---

**TL;DR** — Verify claims in a report against the saved sources. Index those files, then run one `match_phrase` per claim. A claim with no hit is unverified. On 2026-10-20, 3 of 5 phrases were in the docs index and 2 were not. The first check took 490 ms.

XERJ is a new way for AI to search for data.

## One phrase, one claim

The capture indexed `docs/` (wall 14.3 s, exit 3, 9,940 records) and queried `docs-docs`, whose prose field is `body`.

```sh
curl -s -XPOST http://127.0.0.1:9217/docs-docs/_search -H 'content-type: application/json' -d '{"size":1,"_source":["ax_path","title"],"query":{"match_phrase":{"body":"the primary user is an AI agent"}}}'
```

Found. `ax_path` was `WHY_XERJ.md`. `took` was 490.

The next phrase, `shares no code and no architecture`, was found in `ARCHITECTURE.md` (2 hits) and took 485 ms. `point-at-folder` was found in `WHY_XERJ.md` and took 1 ms.

## What a miss means

Two phrases were not found, each with `took` 1.

`Content sniffing never trusts the extension` is a rewording. The file does not contain that sentence. The check correctly refused it.

`XERJ is a fork of Lucene` is not in the file either. The check refused it too. A miss does not say the sentence is false in the world. It says these words are not in the field you searched.

The same five phrases against `text` on the same index returned 0 hits, and each `took` was 0. Wrong field, silent empty. `xerj autoindex map` is how you learn the field name.

## Measured on 2026-10-20

| Phrase on `body` | Result | `took` |
|---|---|---|
| the primary user is an AI agent | Found, `WHY_XERJ.md` | 490 ms |
| shares no code and no architecture | Found, `ARCHITECTURE.md`, 2 hits | 485 ms |
| point-at-folder | Found, `WHY_XERJ.md` | 1 ms |
| Content sniffing never trusts the extension | Not found | 1 ms |
| XERJ is a fork of Lucene | Not found | 1 ms |
| Same five phrases on `text` | 0 hits each | `took` 0 |

The first two checks were slower than the three that followed. Those are the measured times, not a single combined figure. One node, binary v1.0.0-rc.97.

A phrase match does not prove a claim. It shows the words are present.

## When not to use XERJ

If the report paraphrases, or writes a number in a different form, this check will miss a true sentence. Use it for claims you can quote, and treat a miss as "not in this field", not as a verdict.
