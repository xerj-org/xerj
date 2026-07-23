# XERJ for an AI coding agent — real scenarios, measured

This is the honest answer to "does XERJ help Claude Code, and when?" Each
scenario below is a task an agent actually performs, with the measured
before/after and a copy-paste reproduction. Every number came from a run in
this repository; none is projected. Where XERJ does **not** help, that is
stated as plainly as where it does — an agent that trusts a dishonest benchmark
makes worse decisions, not better ones.

The measurement rules (bytes on the wire, savings only over jointly-correct
tasks, question-vocabulary baselines, corpus composition reported) are the ones
enforced by `gate.py` and `gate_retrieval.py` in this directory. Read
`README.md` for why each rule exists, and `../../docs/TOKEN_USAGE.md` for the
token model the scenarios rely on.

## The one law that predicts every result

> **Retrieval savings scale with the searchable-prose fraction of the corpus.
> Analytics savings scale with record count — but analytics answers are small
> either way, so there the win is correctness and latency, not tokens.**

Two corpora, same tasks, opposite outcomes, both measured:

| corpus | prose fraction | XERJ vs grep, tokens | XERJ vs grep, correctness |
|---|--:|---|---|
| 170k LOC source + docs | ~100% | **5.3× fewer** | 7/8 vs 8/8 |
| 36 MB logs/CSV, few docs | 0.01% | 110× **more** | **6/6 vs 4/6** |

Neither number alone is the truth. Pick the scenario that matches your corpus.

---

## Scenario 1 — Orient in an unfamiliar dataset (agent's first move)

**Task:** You are handed a folder of mixed files and asked a question about it.
Before answering anything you must learn what is there.

**Without XERJ:** `ls -R`, then `head` each file, then guess schemas, then
sample rows to infer types. On a heterogeneous folder this is many tool calls
and grows with file count. You still do not know cross-file relationships.

**With XERJ:**
```bash
xerj autoindex ./data          # one command, content-sniffed, zero config
xerj autoindex map             # the data map
```
**Measured:** a 5.65M-record corpus mapped in **~890 tokens, flat regardless of
corpus size** — every dataset's fields, types, null%, example values, time
range, ready-to-send queries, **and auto-detected cross-dataset joins**
(`user_id ↔ user_id, live-confirmed 20/20`). The naive `ls`+`head` equivalent
is smaller only because it contains far less: no types, no cardinality, no
relationships.

**Verdict:** always worth it. This is the single most reliable win, because the
map's cost does not grow with the data.

---

## Scenario 2 — "Where/why is X" in a large codebase

**Task:** "Why did search relevance drop?" — answered in your own words, not
the codebase's.

**Without XERJ:** `grep -rn` a noun you hope appears. grep is *lexical*: if the
answer says "stemming" and you grepped "relevance", you miss it. On a large
tree the candidate set is large and you read many false hits.

**With XERJ:** ranked search returns the one passage, with a highlighted
fragment and a line number to jump to.
```bash
curl -s "localhost:9200/ax-*/_search?filter_path=hits.hits._source.ax_path,hits.hits.highlight" \
  -H 'Content-Type: application/json' -d '{"size":1,
    "query":{"match":{"body":"why did search relevance drop"}},
    "highlight":{"fields":{"body":{"fragment_size":200,"number_of_fragments":1}}}}'
```
**Measured, 170k-LOC corpus:** XERJ **1,457 tokens vs grep 7,762** at equal
recall — **5.3× fewer**, no steering (queries used only the question's words).
**Measured, small prose corpus** (`gate_retrieval.py`): correctness **6/7 vs
grep 3/7** — grep missed four answers whose wording differed from the question.

**Verdict:** the flagship agent win. Bigger and more text-heavy the repo, bigger
the margin. Requires the chunking that `autoindex` now does by default.

---

## Scenario 3 — Analytical question over logs

**Task:** "Error rate by service", "which paths return 502", "refunded value by
country".

**Without XERJ:** `grep | awk | sort | uniq`, or a bespoke Python pass over the
file. Correct if you write it carefully; easy to get subtly wrong (a `*.log`
glob silently skips the `.gz`; a decimal-comma CSV needs `.replace(',','.')`).

**With XERJ:** one aggregation over typed, parsed fields.
```bash
curl -s "localhost:9200/ax-logs/_search?filter_path=aggregations.p.buckets" \
  -H 'Content-Type: application/json' -d '{"size":0,
    "query":{"term":{"status":502}},
    "aggs":{"p":{"terms":{"field":"path","size":6}}}}'
```
**Measured** (`gate.py`, 458k records): correctness **6/6 vs grep 4/6** — the
gate itself caught the baseline silently skipping a gzipped log and mis-parsing
a decimal-comma CSV. Tokens are ~even (both return a few numbers); XERJ is
5–15× faster in wall time.

**Verdict:** win on **correctness and latency**, not tokens. The engine parses
formats (gzip, decimal-comma CSV, nginx log lines) that a quick grep gets wrong.

---

## Scenario 4 — Incident drill-down (each step depends on the last)

**Task:** "Show the failing requests → now that user's other events → now by
region → now by hour." Interactive investigation.

**Without XERJ:** every step re-scans the whole file. Cost is O(corpus) **per
question**, forever: ~7 s per lookup at 1.2 GB, ~1 min at 12 GB. At some size
the investigation stops being interactive.

**With XERJ:** every step is an indexed lookup or aggregation.
**Measured:** a single-user lookup was **0.007 s (XERJ) vs 7.3 s (grep) at
1.2 GB — 1,043× faster**, and stays ~flat as the corpus grows because the index
is O(matching), not O(corpus).

**Verdict:** win that *grows* with data size. Below ~1 GB grep is fine; above
it, indexed drill-down is the difference between interactive and not.

---

## Scenario 5 — A relational question, with no JOIN

**Task:** "How many enterprise customers have an open ticket?" — spans two
sources.

**The trap:** answered relationally, the agent pulls one whole side of the
relation into context (966 ids × 16 bytes) and echoes it back — **15,914 bytes
for a 3-byte answer.** This is the entire "100× more than grep."

**The fix — denormalize at ingest, then one filtered aggregation:**
```bash
# put `plan` on the ticket once, at ingest; then:
curl -s "localhost:9200/tickets/_search?filter_path=aggregations.u.value" \
  -H 'Content-Type: application/json' -d '{"size":0,
    "query":{"bool":{"filter":[{"term":{"status":"open"}},{"term":{"plan":"enterprise"}}]}},
    "aggs":{"u":{"cardinality":{"field":"user_id"}}}}'
```
**Measured:** **36 bytes, identical answer (122) — 442× smaller.** When one side
already fits in a single document's array field, ES `terms` lookup does this
server-side (see `docs/TOKEN_USAGE.md`).

**Verdict:** the token cost is set by the *pattern*, not the engine. The
relational pattern is a trap; the native pattern is competitive. `autoindex`
already detects the foreign key — denormalizing on it is the next engine step.

---

## Scenario 6 — Durable memory across sessions

**Task:** remember a fact in one session, recall it by meaning in another.

**Without XERJ:** re-derive it, or the user re-supplies it. No mechanism.

**With XERJ:** `POST /_memory/{ns}` to store, `_recall` to retrieve by meaning.
**Measured:** a semantic recall returned in **328 bytes**, and recalled the
right memory from queries sharing *no vocabulary* with the stored text (in
`--embed-mode neural`; the default lexical embedder scores far lower on
paraphrase — P@1 0.33 vs 1.00, so turn neural on for memory).

**Verdict:** a capability the baseline does not have at all. Worth wiring in for
any multi-session agent, with the neural embedder active.

---

## When XERJ does *not* help — say so

- **One-integer questions on a small file.** `grep -c` prints `2` in one byte;
  no engine answering in JSON beats that. Ingest cost is never repaid.
- **`percentiles`/`date_histogram` without the fixes on this branch.** They
  fell to an O(N) scan; the fixes here bring them to columnar speed, but verify
  your build has them.
- **Byte-level forensics.** "What is the exact 0x80 byte at offset 4231" is a
  grep/xxd job, not a search job.
- **A corpus you will query once.** The break-even is ~30–70 questions
  (ingest amortization). One-shot use loses.

## Reproducing all of this

```bash
# analytics regime (XERJ wins correctness, loses tokens on record-heavy data)
python3 demo/agent-gate/make_corpus.py        /tmp/gate-corpus
xerj --insecure --data-dir /tmp/gate-data &
xerj autoindex /tmp/gate-corpus
python3 demo/agent-gate/gate.py               /tmp/gate-corpus

# retrieval regime (XERJ wins correctness and, at scale, tokens)
python3 demo/agent-gate/make_corpus_prose.py  /tmp/gate-prose
xerj autoindex /tmp/gate-prose
python3 demo/agent-gate/gate_retrieval.py     /tmp/gate-prose
```

Both gates print the corpus composition next to every ratio, and refuse to
collapse the two regimes into one headline number.
