---
title: "Evidence behind agent-memory links"
h1: "How can agent-memory links retain evidence?"
description: "Every detected link in XERJ carries the exact line that taught it. All 7 edges in our capture had an evidence quote, and unlink wrote a tombstone."
slug: "evidence-for-agent-memory-links"
cluster: "Agent memory: provenance"
question: "Agent memory: knowledge graphs aren't enough, we need a know-how graph"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, build a XERJ brain over a notes folder, search the .xerj-memory-<brain>-edges index for documents with a src field, and report how many edges carry an evidence quote and what each quote says."
commands:
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/.xerj-memory-notes-edges/_search' -H 'content-type: application/json' -d '{\"query\":{\"exists\":{\"field\":\"src\"}},\"size\":50,\"_source\":true,\"track_total_hits\":true}'"
    note: "Read every detected edge with its evidence. The exists filter drops the brain metadata document."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_graph/notes/link' -H 'content-type: application/json' -d '{\"src\":\"note:manual-a\",\"dst\":\"note:manual-b\",\"type\":\"related\",\"weight\":0.9,\"confidence\":0.5}'"
    note: "Assert a link by hand. The created edge carries no evidence object."
  - cmd: "curl -s -XDELETE 'http://127.0.0.1:9430/_graph/notes/link/36fda0a72f2b01de15ed79c7b0909c0a'"
    note: "Retire an edge. XERJ writes a tombstone rather than deleting the document."
links_out:
  - "knowledge-graphs-for-agent-memory"
  - "coding-agent-memory-across-sessions"
  - "/use-cases/second-brain"
faq:
  - q: "Do XERJ links keep the text that created them?"
    a: "Yes, for detected links. All 7 detected edges in our capture carried an `evidence` object with a quote, a source filename and a character offset."
  - q: "What does an evidence object contain?"
    a: "Three fields: `quote` with the exact line, `source` with the filename, and `offset` with the character position inside that file."
  - q: "Do hand-asserted links carry evidence?"
    a: "No. A link created through `POST /_graph/{brain}/link` has detector `manual@1` and no `evidence` object, and the console labels it as asserted rather than detected."
  - q: "Does deleting a link erase it?"
    a: "No. Unlink re-indexes the edge with `invalid_at` and `expired_at` set. The edge document remains readable in the edges index."
  - q: "How do I read the graph as it was?"
    a: "Pass `as_of` with a timestamp. An edge is visible when `valid_at` is at or before that time and `invalid_at` is absent or later."
  - q: "What does confidence mean on an edge?"
    a: "It is a per-detector constant, not a learned score. Our wikilink edges carried 0.95, sequence edges 0.99 and same-directory edges 0.4."
  - q: "Where do I find the edges?"
    a: "In the index named `.xerj-memory-{brain}-edges`. Filter on `{\"exists\":{\"field\":\"src\"}}` to skip the brain metadata document, which has no `src`."
---

**TL;DR** — XERJ stores the line that taught each link. All 7 detected edges in our capture carried an `evidence` object with a quote, a filename and an offset. A hand-asserted link carried none, and retiring a link wrote a tombstone instead of deleting the edge.

## Detected links carry the exact line

Each detected edge in a XERJ brain states where it came from. The `evidence` object holds `quote`, `source` and `offset`, so a reader can open the file at that character position and see the sentence for themselves.

Our capture built a brain from a 3-file notes folder and produced 7 edges. Every one of the 7 carried an evidence quote.

```json
{"src":"c17355db8d2a82cfe436140a39e2d7ad","dst":"0ddfc0b862bd2b7ee0b308b8ce4768c9",
 "type":"wikilink","detector":"wikilink@2","confidence":0.95,"weight":1.0,
 "evidence":{"quote":"See [[00-overview]] for context.","source":"01-runbook.md","offset":170},
 "valid_at":1768478400000,"invalid_at":null}
```

## Structural detectors make the quote checkable

The quote is useful because the detectors are structural rather than interpretive. A `wikilink` edge quotes the wikilink line. A `same_dir` edge quotes its own rationale, `00-overview.md and 01-runbook.md share directory .`, which the console shows as a reason rather than as a quotation.

Confidence is a per-detector constant, not a learned number. Our run carried 0.99 on `sequence` edges, 0.95 on `wikilink` edges and 0.4 on `same_dir` edges.

XERJ invents nothing to fill a gap. XERJ renders a real quote, a structural rationale and an asserted link as three different things.

## An asserted link carries no evidence

A link you create by hand is honest about being a claim. The route `POST /_graph/{brain}/link` accepts `src`, `dst`, `type`, `weight` and `confidence`. It returns an edge whose detector is `manual@1`.

```json
{"created":true,"edge_id":"36fda0a72f2b01de15ed79c7b0909c0a",
 "edge":{"src":"note:manual-a","dst":"note:manual-b","type":"related",
 "detector":"manual@1","confidence":0.5,"weight":0.9,"valid_at":1787302747613}}
```

There is no `evidence` key in that response, and the console labels such a link `no evidence recorded — asserted, not detected`.

## Retiring a link writes a tombstone

Unlink never deletes the edge document. `DELETE /_graph/{brain}/link/{edge_id}` re-indexes the same edge with two new timestamps and returns them.

```json
{"brain":"notes","edge_id":"36fda0a72f2b01de15ed79c7b0909c0a",
 "invalidated":true,"invalid_at":1787302747760,"expired_at":1787302747760}
```

A direct search of the edges index afterwards still returns the document, now carrying `valid_at`, `invalid_at` and `expired_at` together. The present-time ego view returns no edges and reports the reason in its `not_shown` object as `expired_excluded: 1`.

## Reading history with as_of

Visibility is bi-temporal. An edge is visible at a given time when `valid_at` is at or before that time, and `invalid_at` is either absent or later than it.

That rule lets an agent ask what it believed last week rather than only what it believes now. Pass `as_of` with a timestamp, and add `include_expired=true` to see retired edges in their old window.

Our capture ran that query with `as_of=2026-08-21T00:00:00Z`, which is earlier than the asserted edge's own `valid_at`. The response therefore returned no edges, and it is proof that the filter applies rather than proof of a recovered history.

## Why this matters for an agent

An agent that acts on a link must know whether a person asserted it or a file stated it. XERJ answers that from the edge itself: a detector name, a confidence constant, and either a quote or nothing.

Retirement without deletion matters for the same reason. A wrong link stops changing recall at once, and the edge document that proves the old belief stays readable.

## What this capture does not show

The fixture is a 3-file notes folder, so 7 edges from 3 detectors is the whole sample. The `as_of` probe in this pass predates the edge it queried, so no before-and-after history pair was captured. No large brain and no multi-node run exists in this pass.
