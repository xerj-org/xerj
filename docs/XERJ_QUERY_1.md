# XERJ Query-1: the capabilities

Retrieval finds documents. XERJ Query-1 writes a small program over the facts
you stored, and the engine executes it. Lookup and comparison answers carry the
program, the facts they used and the character span each one quotes; counting
answers carry a program hash, not the counted records. It is off by default;
everything on this page applies to a node started with `xerj --facts-model-dir <DIR>`
(without the flag every `/_facts` path answers `404 xerj_query_not_enabled`, and
rc.101 still upgrades the data directory to format 4 on first write).
Every request and response below was run on rc.101 with the shipped bundle
before it was written down. Setup: [MODEL_QUICKSTART.md](./MODEL_QUICKSTART.md);
the full contract: [FACTS_API.md](./FACTS_API.md).

## 1. Prompt to program, not prompt to text

The user writes a plain question ("What is Bo Chen's height?", "Which river is
longer, X or Y?", "How many people have office Berlin?"). The model reads the
question plus a numbered listing of the facts in the namespace and emits a few
program steps: `LOOKUP #12`, `CHAIN`, `LT/GT/EQ/NOT`, `PICK_LT`, or for counts
`SET → FIND → FILTER → AGG COUNT`. It never writes the answer itself.

```sh
curl -s localhost:9200/_facts/hr/_ask -H 'content-type: application/json' \
  -d '{"question": "Who is taller, Ana Bel or Bo Chen?"}' | jq '{final_status, final_answer, program}'
# -> {"final_status": "ANSWER", "final_answer": "Bo Chen",
#     "program": [["LOOKUP",0,null],["LOOKUP",1,null],["PICK_GT",null,null],["RETURN",null,null]]}
```

Compared with: keyword search returns the two height records and leaves the
comparison to you; an LLM writes "Bo Chen" as text, and checking it means rereading
the records it was given. Here the answer is the output of `PICK_GT` over two stored
facts, and both are named in the response.

## 2. The engine executes the program over its own store

Each step points at a stored fact (subject, relation, typed value, source
document, character span). The executor runs it in microseconds and produces
the answer plus the trace: which record, which version, which span.

```sh
curl -s localhost:9200/_facts/hr/_ask -H 'content-type: application/json' \
  -d '{"question": "How tall is Ana Bel?"}' | jq '{final_answer, program, final_evidence_ids, ev: .evidence[0]}'
# -> {"final_answer": "170 cm", "program": [["LOOKUP",1,null],["RETURN",null,null]],
#     "final_evidence_ids": ["f1"],
#     "ev": {"fact_id": "f1", "subject": "Ana Bel", "relation": "height", "kind": "number", "value": "170 cm",
#            "quote": "170", "source": {"doc_id": "d1", "version": "v1",
#            "locator": {"unit": "char", "start": 11, "end": 14}, "content_sha256": "d773c58a…"}}}
```

`handler_ms` on that ask: 36.5 ms at the median over ten warm asks on this 2-fact
namespace (x86 laptop, 4 model threads). On the 160-question head-to-head the same
model answered in 220 ms at the median on an x86 laptop (1,039 ms on four slow
efficiency cores of a shared box that was training a model,
and an earlier, unshipped int8-guard build measured 120 ms on one 40,000-fact synthetic
namespace with a single templated question; the shipped bundle serves w8 and is not
that build).

Compared with: on the same questions, Opus 5.5 took about 1.6 s per question to
write a RAG answer (Messages API time; our command-line client's start-up not counted),
after retrieval that took under 2 ms, with 2.8k to 4.6k input tokens; writing the
answer is the step that gives RAG its answer quality. GraphRAG took about 0.1 s to
build its context and the same order of API time to write the answer, with 15.7k
input tokens.

## 3. Verification and refusal are built in

A verifier checks the program against the store before serving. Pointer
resolves to nothing → `UNKNOWN`. Two stored facts disagree → `CONFLICT`,
propagated through comparisons. A count is served only when the executor can
close the set and a second, independently seeded model writes the same
program. Measured: On our 160-question test: 1 wrong for Query-1 and 5 for Opus 5.5, both given each question's own facts; 6 for Opus reading the whole store; 9 to 15 for RAG and GraphRAG pipelines that had to search for the facts.

```sh
# a fact that was never written
curl -s localhost:9200/_facts/hr/_ask -H 'content-type: application/json' \
  -d '{"question": "What is Ana Bel'"'"'s salary?"}' | jq '{final_status, final_answer, reason: .decline.reason_code}'
# -> {"final_status": "UNKNOWN", "final_answer": null, "reason": "RAW_UNKNOWN"}

# two heights for Bo Chen (182 cm and 175 cm), neither superseding the other
curl -s localhost:9200/_facts/hr/_ask -H 'content-type: application/json' \
  -d '{"question": "How tall is Bo Chen?"}' | jq '{final_status, final_answer, final_evidence_ids}'
# -> {"final_status": "CONFLICT", "final_answer": null, "final_evidence_ids": []}
```

Both are HTTP 200: a factual outcome, not an error. Read `final_status` and
`final_answer`; the plain `status`/`answer` are the model's unchecked program.

Compared with, same 160 questions from our development evaluation, right / wrong /
declined: XERJ Query-1 124 / 1 / 35, answering from each question's own facts;
Opus 5.5 given the question's own collection 137 / 5 / 18; Opus 5.5 with the
whole store in context (5,000 records, 40 questions per call) 130 / 6 / 24; GraphRAG (our minimal Edge et al. implementation) on Opus 5.5 127 / 9 / 24; BM25
RAG top-10 on Opus 5.5 124 / 9 / 27 (most of its wrong answers came when retrieval
missed the evidence); embedding RAG top-10 (bge-small) 109 / 14 / 37. The wrong-answer
gap is significant against GraphRAG and every RAG arm, borderline against Opus reading
the whole store, and not separable from Opus given the right collection. The overall
score is statistically above embedding RAG and BM25 top-5 (117 / 12 / 31) and not
separable from the others at n=160.

## 4. Indexing instead of embeddings

No vector step on this path. The "index" the model uses is the facts store the
engine already keeps (typed records + inverted index + doc-values). Writing
the 9,496 facts of the head-to-head's 160 per-question namespaces took 8.8 s on a shared
aarch64 box (1.8 to 3.0 s on an x86 laptop) and 15.4 MB on disk; no encoder, no vector RAM, nothing to redo
when a model changes. The same binary still offers BM25 full-text, aggregations
and kNN for documents.

Compared with: embedding the RAG store (5,000 records, 4,305 distinct, in 660 chunks)
with bge-small took 19.6 s on eight threads at 1.28 GB peak RSS, and is repeated
whenever the embedding model changes; GraphRAG's index (our minimal implementation,
cheaper than the reference one) took 267 Opus calls, 72 minutes serial and $14.27 at
list price for the 5,000-record test store.

## 5. Facts are a versioned store you control

Facts go in through `/_facts/{ns}/_bulk` (text, number, date, boolean kinds; a
`subject_id` per entity; a source document and quote span). An `edits`
supersede changes the answer (170 → 172 cm) while the old answer stays readable
at the earlier revision; a second disagreeing write turns the answer into
`CONFLICT`; a withdraw restores. Every ask can be pinned to a revision.

```sh
# write (revision 1 → 4)
curl -s localhost:9200/_facts/hr/_bulk -H 'content-type: application/json' -d '{
  "documents": [{"doc_id": "d1", "version": "v1", "text": "Ana Bel is 170 cm tall."},
                {"doc_id": "d2", "version": "v1", "text": "Bo Chen is 182 cm tall."}],
  "facts": [{"fact_id": "f1", "subject": "Ana Bel", "subject_id": "emp:1", "relation": "height", "kind": "number",
             "value": "170", "unit": "cm", "doc_id": "d1", "version": "v1", "start": 11, "end": 14, "quote": "170"},
            {"fact_id": "f2", "subject": "Bo Chen", "subject_id": "emp:2", "relation": "height", "kind": "number",
             "value": "182", "unit": "cm", "doc_id": "d2", "version": "v1", "start": 11, "end": 14, "quote": "182"}]
}' | jq '.committed'
# -> {"from_rev": 1, "to_rev": 4}

# correct Ana Bel's height: the edit names the fact it replaces and carries the new source text
curl -s localhost:9200/_facts/hr/_bulk -H 'content-type: application/json' -d '{
  "edits": [{"op": "correct",
             "doc": {"doc_id": "d1", "version": "v2", "text": "Ana Bel is 172 cm tall."},
             "fact": {"fact_id": "f3", "subject": "Ana Bel", "subject_id": "emp:1", "relation": "height", "kind": "number",
                      "value": "172", "unit": "cm", "doc_id": "d1", "version": "v2", "start": 11, "end": 14, "quote": "172"},
             "supersedes": "f1"}]
}' | jq '.committed'
# -> {"from_rev": 5, "to_rev": 6}

curl -s localhost:9200/_facts/hr/_ask -H 'content-type: application/json' \
  -d '{"question": "How tall is Ana Bel?"}' | jq '{final_answer, final_evidence_ids, published_rev}'
# -> {"final_answer": "172 cm", "final_evidence_ids": ["f3"], "published_rev": 6}

curl -s localhost:9200/_facts/hr/_ask -H 'content-type: application/json' \
  -d '{"question": "How tall is Ana Bel?", "revision": 4}' | jq '{final_answer, final_evidence_ids}'
# -> {"final_answer": "170 cm", "final_evidence_ids": ["f1"]}

# withdraw a disagreeing fact (f4, a second height for Bo Chen) and the answer comes back
curl -s -X DELETE localhost:9200/_facts/hr/f4 | jq '.committed'
# -> {"from_rev": 9, "to_rev": 9}
```

The edit's `doc` must carry its `text`: without it the batch is refused
(`facts_contract_error: world: missing field text`). `kind` is exactly
`text`, `date`, `number` or `boolean`; `version` is a string; `quote` must
equal `text[start:end]`; the field table with every refusal text is in
[FACTS_API.md](./FACTS_API.md#writing-facts-post-_factsns_bulk).

Compared with: a typical RAG index keeps no revision history; correcting a record means
re-chunking and re-embedding it, and the old answer is no longer reproducible. An LLM
reading the data keeps no store at all.

## 6. Memory and brains sit beside it

Agent memory (store / recall / forget, lexical ranking today) and brains (link,
unlink, ego with evidence, overview) are separate surfaces over MCP; brain
names ending in `-facts` are reserved for facts namespaces. The model reads
facts namespaces only.

On rc.101, a node started with the flag lists 24 tools in `xerj mcp` and 23 without it
(the published schema on main lists 24 without the flag, because it already includes
`xerj_sources` from rc.102); the ones on this
page are `xerj_memory_store`, `xerj_memory_recall`, `xerj_brain_link`,
`xerj_brain_unlink`, `xerj_brain_ego`, `xerj_brain_overview` and
`xerj_facts_ask` (`{"namespace": "hr", "question": "..."}`, the same contract as
`POST /_facts/{ns}/_ask`). Without the flag the list is the 23-tool published
schema. Memory recall (`POST /_memory/{ns}/_recall`) runs no model; "ask" over
memory is recall, and the model answers only over `/_facts`. `POST /_memory/notes-facts`
answers `400` ("suffix '-facts' is reserved for facts indices").

## 7. Counting

Set counts ("how many X have Y") over facts that carry `subject_id`, executed
exactly and checked by a second model. Not sums, totals or thresholds in this
release, and a sum question can still be served as a member count. Distinct-value
counts did not compile in our tests.

```sh
curl -s localhost:9200/_facts/team/_bulk -H 'content-type: application/json' -d '{
 "documents":[{"doc_id":"d0","version":"v1","text":"Record Ana Bel.\noffice -> Berlin"},
              {"doc_id":"d1","version":"v1","text":"Record Bo Chen.\noffice -> Berlin"},
              {"doc_id":"d2","version":"v1","text":"Record Di Eve.\noffice -> Munich"}],
 "facts":[{"fact_id":"f0","subject":"Ana Bel","subject_id":"emp:0","relation":"office","kind":"text","value":"Berlin","doc_id":"d0","version":"v1","start":26,"end":32,"quote":"Berlin"},
          {"fact_id":"f1","subject":"Bo Chen","subject_id":"emp:1","relation":"office","kind":"text","value":"Berlin","doc_id":"d1","version":"v1","start":26,"end":32,"quote":"Berlin"},
          {"fact_id":"f2","subject":"Di Eve","subject_id":"emp:2","relation":"office","kind":"text","value":"Munich","doc_id":"d2","version":"v1","start":25,"end":31,"quote":"Munich"}]}'
curl -s localhost:9200/_facts/team/_ask -H 'content-type: application/json' \
  -d '{"question":"How many people have office Berlin?"}' | jq '{path, final_status, final_answer, agree: .counting.partner_agreement}'
# -> {"path": "counting", "final_status": "ANSWER", "final_answer": 2, "agree": true}
```

Without the three `subject_id` fields the same question answers
`{"path": "facts", "final_status": "UNKNOWN"}` with `counting.status:
"INCOMPLETE"`. A counting answer cites no records (`final_evidence_ids` is `[]`
by design) and names its program by `counting.program_sha256`.

Compared with: no LLM arm in the head-to-head was wrong fewer than 5 times in
160. XERJ Query-1's counting answers do not show their program either; they
carry its hash.

## What it does not do

- No fact extraction from arbitrary documents: users or agents write facts
  (`xerj autoindex` yields records + a graph, not facts namespaces). The river
  example's records came from the evaluation pipeline.
- A count does not check the noun of the question: on the team namespace above,
  "How many devices in Berlin in total?" (no devices facts) returns the member
  count, 2, as verified. Disclosed; the real fix is planned for rc.102.
- Over-declines: 383 of 1,600 standard-eval items; 35 of 160 in the
  head-to-head. The price of not guessing.
- Off by default; without the flag there is no /_facts route (the rc.101 data-format upgrade still applies).

## Why it is cheap

Each question runs the facts model once over the namespace's fact listing (about
1,500 input tokens at the median in our test) after a counting stage, and counts add
two counting models; all on CPU, nothing leaves the machine: $0.042 per 1,000
questions of CPU time on the 160-question head-to-head (shared aarch64 box, AWS c7g price)
vs $13 to $694 per 1,000 questions for LLM pipelines at Opus 5.5 list prices on the
same 160-question test: BM25 RAG top-5 $13.83 (embedding top-5 $13.41), embedding
RAG top-10 $19.85, Opus 5.5 with perfect retrieval $28.84, GraphRAG $65.39 plus a
$14.27 index build, and Opus 5.5 with the whole 172,870-token store in context, one
question per call, no caching, $694.30 (20-question sample). Caveats: records were
pre-extracted for every arm; n=160; the questions come from the model's own development
evaluation; XERJ Query-1 and the Opus oracle got each question's facts while RAG had to
search a merged store of 4,305 distinct records; the data and scripts are not public.
