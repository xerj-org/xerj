# Passage retrieval: long documents that compete on any section

## The problem

Embeddings describe *one* piece of text. But real documents — a manual, a
contract, a long article, a support-ticket thread — are *many* topics stitched
together. If you embed the whole document into a single vector, you get the
**average** of everything it says. A query about one section then has to match
that blurred average, and it loses to a short document that is *only* about
that section. The relevant long document is there; your retriever just can't
see it.

The fix the field settled on is **passage (chunk) embeddings**: split the
document into overlapping passages, embed each one, and score the document by
its *best-matching* passage. Most stacks make you build that pipeline
yourself — a separate chunker, a vector column per chunk, a nested query — and
keep it in sync with your data.

## Why XERJ

XERJ does it on ingest, with no configuration. A `semantic_text` field splits
each value into overlapping passages, embeds **every passage**, and persists
the per-passage vectors alongside the document. A `semantic` query then scores
each document by its **best-matching passage** (max-sim) instead of a single
pooled vector — so a long document competes on *any one* of its sections.

Short values (a single passage) are unchanged: one vector, exactly as before.
The pipeline only kicks in when a value is long enough to span more than one
passage, so you pay nothing for short fields.

## The solution

Nothing to configure — map the field as `semantic_text` and index normally:

```bash
curl -sX PUT "$XERJ_URL/docs" -H 'content-type: application/json' -d '{
  "mappings": { "properties": { "body": { "type": "semantic_text" } } }
}'

# A long, multi-topic document — embedded per passage automatically.
curl -sX PUT "$XERJ_URL/docs/_doc/handbook?refresh=true" \
  -H 'content-type: application/json' \
  -d '{ "body": "...many paragraphs across many topics..." }'
```

Query it with `semantic` — the question is embedded server-side and scored
against the best passage:

```bash
curl -sX POST "$XERJ_URL/docs/_search" -H 'content-type: application/json' -d '{
  "query": { "semantic": { "field": "body",
                           "query": "how does chlorophyll drive photosynthesis",
                           "k": 10 } }
}'
```

The document is retrieved on the strength of the one passage that matches —
even if the rest of it is about something else entirely.

## Try it

`docs/examples/passage-retrieval/passage_demo.py` indexes the 40 real KB
articles as short docs **plus** one long "compendium" that concatenates all
40, then runs each article's title as a query and measures how often the long
compendium reaches the top 3 under each scoring mode. Both arms use XERJ's own
embedder — the pooled baseline reads XERJ's whole-document `<field>_vector`
back out of `_source` — so the only variable is pooled-vs-per-passage:

```
$ python3 docs/examples/passage-retrieval/passage_demo.py
40 short article docs + 1 long compendium of all 40

compendium embedded into 21 passage vectors (pooled into 1 whole-doc vector of 384 dims)

compendium reached the top-3:
  per-passage : 39/40  (98%)
  pooled      : 13/40  (32%)

OK: per-passage scoring let the long document compete on each of its sections
    (39/40 top-3); a single pooled vector managed only 13/40.
```

The compendium contains every topic, so with per-passage scoring it competes
for almost every query. Averaged into a single vector, it is a mediocre blur
that loses to the undiluted short docs two times out of three.

## Reproduce it yourself

Start XERJ on its default port (`9200`) and run the example — no keys, no
external services, stdlib Python only:

```bash
# 1. Start a throwaway XERJ (ES-compat wire on :9200 by default)
xerj --insecure --data-dir ./data

# 2. In another shell, run the demo (honors $XERJ_URL, default http://localhost:9200)
python3 docs/examples/passage-retrieval/passage_demo.py
```

Point it at a non-default host/port with `XERJ_URL=http://host:port`. The run
is fully deterministic (offline lexical embedder over a fixed 40-article
corpus), so every run prints the same numbers:

```
40 short article docs + 1 long compendium of all 40

compendium embedded into 21 passage vectors (pooled into 1 whole-doc vector of 384 dims)

compendium reached the top-3:
  per-passage : 39/40  (98%)
  pooled      : 13/40  (32%)
```

Both `39/40` and `13/40` are computed by the run itself over the 40 title
queries — nothing is hardcoded. If either arm regresses (per-passage top-3
rate < 75%, or per-passage failing to beat pooled) the script exits non-zero,
so it doubles as a CI gate.

## How it works

- **Chunker.** The built-in overlapping chunker (512-char passages, 64-char
  overlap) splits the field value; each passage is embedded with the same
  embedder used for the whole field (built-in lexical by default, or your
  configured OpenAI-compatible endpoint).
- **Storage.** The pooled whole-document vector stays in `<field>_vector` for
  back-compat and plain kNN; the per-passage vectors are persisted in
  `<field>_vector_chunks` **only** when the document spans more than one
  passage.
- **Scoring.** A `semantic` query scores each candidate by the maximum
  similarity over its passage vectors, falling back to the single pooled
  vector for short documents.

## Notes and limits

- **Short fields are byte-identical to before.** A single-passage value stores
  one vector and scores exactly as it did pre-pipeline; no existing behavior
  changes.
- **The built-in embedder is lexical**, not neural — great for offline demos
  and CI. Point XERJ at an OpenAI-compatible `/v1/embeddings` endpoint
  (`[embedding]` in `xerj.toml`) for production semantics; the pipeline is
  identical, just higher-quality per-passage vectors.
- **Scoring is exact (brute-force max-sim)** over the passage vectors, which is
  what makes the ranking deterministic on the corpus sizes these recipes use.
