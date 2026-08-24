---
title: "Should agent memory use a graph?"
h1: "Should agent memory use a knowledge graph?"
description: "XERJ builds a bounded link index over indexed documents, not a graph database. Our capture found 7 edges, answered 1 and 2 hops, and refused 3."
slug: "knowledge-graphs-for-agent-memory"
cluster: "Agent memory: link retrieval"
question: "Ask HN: Anyone using knowledge graphs for LLM agent memory/context management?"
intent: "tool-selection"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run `xerj autoindex ./notes --url http://127.0.0.1:9430 --prefix br --brain notes --progress plain`, GET /_graph/notes/overview to read the totals and the detector split, then request /_graph/notes/ego at hops=1, hops=2 and hops=3 and report the edge counts plus the exact text of the refusal you get at 3 hops."
commands:
  - cmd: "xerj autoindex ./notes --url http://127.0.0.1:9430 --prefix br --state-dir ./state-br --brain notes --progress plain"
    note: "Index a notes folder and build its second brain in the same run."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9430/_graph/notes/overview'"
    note: "Read the edge total, the node total and the detector breakdown."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9430/_graph/notes/ego?node=0ddfc0b862bd2b7ee0b308b8ce4768c9&hops=2&include_nodes=true'"
    note: "Expand two hops from one document. Three hops returns HTTP 400."
links_out:
  - "evidence-for-agent-memory-links"
  - "private-agent-memory-namespaces"
  - "coding-agent-memory-across-sessions"
faq:
  - q: "Does XERJ give me a graph database for agent memory?"
    a: "No. XERJ builds a bounded link index over its own indexed documents. It has no Cypher, no shortest-path and no variable-depth traversal."
  - q: "How many hops can I traverse?"
    a: "One or two. A 3-hop request returns HTTP 400 with the cap stated, and the response tells you to expand again from the returned reachable ids."
  - q: "What creates the links?"
    a: "Deterministic detectors. Our run used `wikilink@2`, `sequence@2` and `samedir@2`, which read wikilinks, section order and shared directories."
  - q: "Are the detectors reading meaning?"
    a: "No. Seven of the eight detectors are structural, and the one content-reading detector compares unstemmed strings, so it matches text rather than concepts."
  - q: "What are the graph bounds?"
    a: "Hops cap 2, 4,096 frontier ids per hop, 100,000 result edges per request, 64 ego seeds, 1,000 ego edges and 10,000 restrict ids."
  - q: "Can recall use the links?"
    a: "Yes. Pass a `graph` object to `_recall` with `mode` set to `restrict` or `blend`, plus `seeds`, `hops` of 1 or 2, and an optional `weight`."
  - q: "Where do the edges live?"
    a: "In an index named `.xerj-memory-{brain}-edges`. `autoindex` creates one per indexed folder by default, named after the folder."
---

**TL;DR** — Links help, and XERJ is not a graph database. XERJ builds a bounded, graph-shaped link index over the documents it already indexed. Our capture produced 7 edges over 6 documents, answered 1 hop with 3 edges and 2 hops with 6, and refused 3 hops.

## What XERJ actually builds

The XERJ second brain is a search engine with a graph-shaped index over its own documents, not a graph database. Deterministic detectors read the indexed files and write edges into an index named `.xerj-memory-{brain}-edges`.

One `autoindex` run built ours. The terminal line reports the whole result, including the counts that did not happen.

```text
graph: 7 edges → .xerj-memory-notes-edges (samedir@2 2, sequence@2 3, wikilink@2 2);
0 unresolved, 0 ambiguous, 0 capped, 0 self-dropped, 0 invalidated
```

## What the overview reports

`GET /_graph/{brain}/overview` answers with totals, the detector split, the type split, and the hubs by in-degree and out-degree. Our 3-file notes folder produced 6 nodes and 7 live edges, with 0 invalidated.

| detector | live edges | edge type |
| --- | --- | --- |
| `sequence@2` | 3 | `sequence` |
| `samedir@2` | 2 | `same_dir` |
| `wikilink@2` | 2 | `wikilink` |

The overview also names the embedder in use, which was `lexical-feature-hash` on our default node. The embedder name matters here. The same brain backs the optional `semantic: true` recall mode, and the default embedder is lexical feature hashing rather than a neural model.

## The detectors are structural

Seven of the eight edge detectors read structure: wikilinks, Markdown links, hrefs, path citations, section order and directory chains. The one content-reading detector compares unstemmed strings, so it matches literal text rather than concepts.

Every edge in our capture came from a structural detector. A `wikilink` edge came from the literal line `See [[00-overview]] for context.`, and a `same_dir` edge came from two files sharing a directory.

Structural detection is the reason the output is stable. The same folder always produces the same edges, with no model in the path to drift.

## Expansion is bounded, and says so

Ego expansion answers at 1 or 2 hops. Our 1-hop request from one document returned 3 edges and 3 neighbors, and the 2-hop request from the same seed returned 6 edges.

A 3-hop request is a hard refusal rather than a truncation.

```text
hops is capped at 2: XERJ's second brain is a relationship layer over documents,
not a graph database (no Cypher, no shortest-path, no variable-depth traversal).
Iterate: expand again from this response's 'reachable' ids.
```

Every other bound is reported rather than silent. The response carries a `not_shown` object with counts for clipped edges, a clipped frontier, excluded expired edges and type-filtered edges, and ours were all 0.

| bound | limit |
| --- | --- |
| hops | 2 |
| frontier ids per hop | 4,096 |
| result edges per request | 100,000 |
| ego seeds | 64 |
| ego edges per response | 1,000 |
| restrict prefilter ids | 10,000 |

## Using links inside recall

Recall accepts a `graph` object with a required `mode` of `restrict` or `blend`. The other fields are `seeds`, `hops` of 1 or 2, `types`, a `weight` that defaults to 0.3, and `as_of`. Proximity is 1.0 for a seed and `0.5^hop` times the clamped weight otherwise.

The link index earns its place for an agent here. Use `restrict` to confine recall to documents near a known anchor. Use `blend` to shift ranking while keeping every candidate.

## When to choose something else

Pick a real graph database when you need variable-depth traversal, shortest paths, a query language or reasoning over typed entities. XERJ is not a graph database and answers none of those. The refusal body says so rather than degrading quietly.

Pick the XERJ link index when the relationships are already in your files and you want the same answer every run. One single-node process is then the whole budget.

## What this capture does not show

The fixture is a 3-file notes folder that produced 6 documents and 7 edges, so the counts demonstrate the mechanism and nothing about scale. Only 3 of the 8 detectors fired on this fixture. No large brain, no multi-node run and no recall-with-graph measurement exists in this pass.
