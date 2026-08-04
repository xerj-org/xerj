# Second brain: point one binary at your notes

`xerj brain <folder>` turns a folder of notes into a queryable, bi-temporal,
provenance-carrying knowledge graph — one command, one static binary, no graph
database, no LLM, no embedding service, no config. A console dashboard then
shows what your notes believe, and an MCP surface gives your agent the same
four verbs.

Everything below was run live against this repository's own `docs/` folder —
121 real files, 5.7 MB, including code, images, and config junk. Every number
and every JSON body in this document is captured output, not an illustration.
Where something was *not* measured, it says so.

**The positioning sentence, first:** XERJ does not understand your notes. It
makes the structure you already wrote queryable, timestamped, and
evidence-backed — deterministically. It is a search engine with a graph-shaped
index over its own documents. It is **not** a graph database and it is not a
brain: there is no query language, no shortest-path, no PageRank, and no
unbounded traversal — by design. Edges are ordinary documents; expansion is a
bounded, batched read (at most 2 hops per call).

---

## The three claims

1. **Every link shows its evidence.** A link is not an assertion, it is
   evidence: for authored links (wiki-links, markdown links, hrefs) the
   detector records the exact text that justified the link, with its byte
   offset. Structural links (files that sit together, sections in sequence)
   carry a generated rationale instead — and the UI labels those as
   rationales, never as quotes. Links with no evidence say so.
2. **Replayable at any moment.** Every link is bi-temporal: it has a
   believed-since moment and, if retired, a retired moment. Retired links are
   invalidated, never deleted. You can ask what the brain believed last
   Tuesday, and the retired link comes back — with its retirement visible.
3. **One binary, zero config.** `xerj brain <folder>` discovers the files,
   boots (or attaches to) the server, indexes, detects links, and prints the
   dashboard URL. Re-running the same command converges and adds nothing.

## The real run

```
$ xerj brain /home/claude/ai/xerj/docs
brain 'docs': 121 files (5 MB) under /home/claude/ai/xerj/docs
booted xerj server (pid 339338) — data: …/p5-brain/data
phase A: 7 datasets inferred, 14 junk/skipped files
phase B: indexing 106 files with 8 workers → http://localhost:9340
done in 0.5s — 7 datasets, 561 records live …
✓ your second brain is ready — 121 files, 364 links, 0.7s
  → http://localhost:9340/_xerj-console/#/second-brain?brain=docs
  agents: XERJ_URL=http://localhost:9340 xerj-mcp
```

0.7 s cold on this corpus. Exit code was 3 — the documented
"ready-with-junk" code, because 14 of the 121 files (images, binaries, one
YAML-frontmatter markdown file the sniffer misread — see the warts section)
were skipped rather than indexed. Re-running the same command attached to the
live server, converged in 0.14 s, exited 0, and created zero duplicate links.
Determinism and idempotence on real data, observed, not asserted.

## Claim 1, shown: every link shows its evidence

A live edge from the ego endpoint of one note in the docs brain
(`GET /_graph/docs/ego?node=<id>&include_nodes=true`):

```json
{
  "edge_id": "2b9e125d0a5ca4962fce942d6fd80a48",
  "src": "ca404d04d54db94a7d60f873cfabc8dc",
  "dst": "a640b6d0f75c3f1d405153dc6315b075",
  "type": "mdlink",
  "weight": 0.9,
  "hop": 1,
  "direction": "in",
  "valid_at": 1785165764150,
  "invalid_at": null,
  "created_at": 1785405638505,
  "detector": "mdlink@1",
  "confidence": 0.9,
  "evidence": {
    "quote": "Consistent with the [earlier run](wordpress-audit-with-xerj.md), and now with a",
    "source": "research/wordpress-authz-agentic-audit.md",
    "offset": 1568
  }
}
```

The quote was verified **byte-for-byte against the file on disk**: that exact
text exists verbatim in `research/wordpress-authz-agentic-audit.md`. The
offset is section-relative (byte offset within the section the link was found
in), which is what the contract mandates.

The honest fine print, which the UI also carries: the schema permits links
without evidence (a manually asserted link may carry none — the UI then says
"no evidence recorded — asserted, not detected"), and structural detectors
(`samedir@1`, `sequence@1`) emit rationales like *"a.md and b.md share
directory notes/"*, not text extracted from your notes. So the claim is
"every link shows its evidence", not "every link has a quote".

## Claim 2, shown: replayable at any moment

The full loop, run live, captured responses:

```
# 1. Assert a manual link
POST /_graph/docs/link                    → 201, edge_id 51a2a80c…

# 2. Retire it
DELETE /_graph/docs/link/51a2a80c…        → {"invalidated":true,"invalid_at":1785405730710}

# 3. Ask now
GET /_graph/docs/ego?node=…               → 0 edges, "not_shown": {"expired_excluded": 1}

# 4. Ask about the moment before the retirement
GET /_graph/docs/ego?node=…&as_of=<t_before_retire>
                                          → the link is BACK, with its quote,
                                            and its later invalid_at visible

# 5. Ask now, including retired links
GET /_graph/docs/ego?node=…&include_expired=true
                                          → the tombstone is present
```

Retired, never deleted, replayable. The ledger accounting stayed exact
through the whole session: 366 total belief intervals = 364 detected + 1
manual + 1 new interval from a re-assert; 364 live / 2 retired — and the CLI,
the HTTP API, and the MCP surface all reported the same numbers.

Note step 3: when links are excluded from an answer, the response *says so*
(`not_shown` accounting) instead of silently narrowing the world. Every list
endpoint carries this.

## The dashboard

`xerj brain` prints a console URL that opens the SECOND BRAIN dashboard: one
note in the center, inbound links left, outbound right, grouped by what
taught them, with a belief-time scrubber that narrates what changed between
then and now ("2 appeared · 1 retired between …"). Hovering a link floats up
its evidence; clicking pins the full paper trail.

The dashboard's language rule is binding: human-visible surfaces say *link /
believed since / retired / what taught this* — never `edge`, `src`, `dst`,
`valid_at`, `as_of`. Schema vocabulary appears in exactly one place, the
pinned paper trail, because that is the copy-for-your-agent surface.

Verification status, stated precisely: the dashboard's entire data contract
was executed against the live docs brain — the real data layer plus the real
render functions for all nine panels ran in Node, 47/47 checks passing,
including the retire-then-replay loop and the language rule on real rendered
output. What was **not** verified: real-browser pixel and interaction
behavior (drag, hover cards) in an actual browser. The rendering logic is
proven; the pixels are not.

## The agent surface

`xerj-mcp` (stdio, `XERJ_URL=http://localhost:9340 xerj-mcp`) exposes four
brain tools, proven live in this session: `xerj_brain_ego`,
`xerj_brain_link`, `xerj_brain_unlink`, `xerj_brain_overview`. A real
`xerj_brain_overview` call round-tripped with the correct live/retired counts
and `"contract": "xerj-second-brain/1"`. Because the whole pipeline is
deterministic — same folder in, byte-identical brain out — an agent can cite
it.

## Measured numbers (this corpus only)

| What | Measured |
| --- | --- |
| Corpus | 121 files, 5.7 MB (52 markdown + 69 code/image/config) |
| Links detected | 364 |
| Cold run | 0.7 s |
| Convergent re-run | 0.14 s, exit 0, zero duplicates |
| Data dir | 12 MB (~2.1× source — includes full document storage + graph index) |
| Test suites | graph_expand 5/5 · brain_cli 3/3 · autoindex 109/109 · console-api 76/76 · mcp 25/25 · engine lib 241/241 |
| ES-compat conformance gate | 1360 passed · 0 failed · 3 skipped, with the graph feature on the same server |

**Not measured:** token economics for agents, behavior at large corpus sizes
(nothing beyond the 5.7 MB corpus here and a 518 MB corpus in earlier
autoindex work, which did not exercise the graph), and any comparison against
other note tools. No such numbers are claimed.

## What this does NOT do

- **It does not understand your notes.** The detectors are structural and
  lexical: wiki-links, markdown links, hrefs, directory adjacency, section
  sequence. There is no semantic similarity, no entity extraction, no LLM in
  the loop. The node-store embedder is a lexical feature hash and the API
  says so verbatim (`"embedder": "lexical-feature-hash"`).
- **It is not a graph database.** No Cypher, no shortest-path, no
  variable-depth traversal. Expansion is capped at 2 hops per call; you
  iterate from the response's reachable set. The 400 you get for `hops > 2`
  tells you exactly this.
- **On a corpus without authored links, the brain is mostly sort order.**
  Directory-adjacency and sequence links dominate when there are no
  wiki/markdown links; the dashboard splits AUTHORED vs STRUCTURAL counts so
  this is visible instead of flattering.
- **Scale is unproven.** 121 files in 0.7 s is the measurement; behavior at
  100k notes is not measured and not claimed.

## Known warts (real, observed, not blocking)

- **CLI progress-counter off-by-one:** on this corpus the phase-B progress
  line printed "365 edges (mdlink 84)" while the server truth — and the final
  summary — was 364 (mdlink 83). Suspected cause is a byte-identical
  duplicate-path alias producing two detections of one link; hypothesis, not
  root-caused.
- **YAML-frontmatter markdown can sniff as YAML:** one `SKILL.md` with
  frontmatter landed in junk/skipped instead of being indexed.
- **Stale resume journals can pollute links** after a wiped data dir. `xerj
  brain` no longer guesses: it reports the disagreement between the journal
  and the server instead of resetting on its own. If the data directory really
  was wiped, rerun with `--fresh` to rebuild the plan and republish every note
  in place; if it was not, point at the data directory the journal was written
  against, or rebuild under a new state directory, prefix and brain and
  validate before switching readers.
- **Wikilink detection was not exercised on this corpus** (real docs use
  markdown links); it is covered by the demo-corpus run (30 wikilinks) and
  the autoindex test suite.
- One pre-existing red test in the tree
  (`reindex_pages_past_10k_via_keyset`, inherited from `main`) — unrelated to
  this feature, real, out of scope here.

## Reproduce it

Every step, exactly: [REPRODUCE.md](REPRODUCE.md).
The normative contract: [docs/design/SECOND_BRAIN_SPEC.md](../../design/SECOND_BRAIN_SPEC.md).
