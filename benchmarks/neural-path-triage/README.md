# Neural-path triage: BEIR probes for the `--embed-mode neural` path

Small, stdlib-only probes that turn "the neural path feels slow / odd" into
numbers with a reproduction. They were written to investigate observations from
a BEIR run; four ended as filed issues
([#937](https://github.com/xerj-org/xerj/issues/937) analyzers at flush,
[#938](https://github.com/xerj-org/xerj/issues/938) ingest throughput,
[#939](https://github.com/xerj-org/xerj/issues/939) exact-scan latency,
[#940](https://github.com/xerj-org/xerj/issues/940) RRF tie order) and one
(zero-hit BM25 queries) turned out to be legitimate. Everything a
public page or an issue quotes from here is in `results/<date>/` verbatim.

Scope, stated once: this is the **opt-in neural embedder** (`--embed-mode
neural`, all-MiniLM-L6-v2, Candle, CPU). XERJ's default embedder is lexical
feature hashing and is not measured here. One process on one host — XERJ is
single-node.

## Files

| File | What it answers | Writes to the node? |
|---|---|---|
| `load.py` | index one BEIR corpus (`title`,`text` as `text`; `body` as `semantic_text`) | **yes** — DELETEs the index first |
| `eval.py` | nDCG@10 for BM25, vectors only, hybrid RRF, BM25→bi-encoder re-order | no |
| `zero_hits.py` | which queries get 0 BM25 hits, and is it the corpus or the engine | no |
| `latency.py` | per-arm latency on queries the process has not seen | no |
| `phases.py` | splits `semantic` requests into embed / collect / score / top-k from a `XERJ_TRACE_SEMANTIC_PHASES=1` log | — |
| `rrf_stability.py` | is the hybrid order the same after a restart on unchanged data | no |
| `ingest_bench.py` | `_bulk` documents/s and the server's own CPU-seconds, by number of concurrent clients | **yes** — throwaway `tp_*` indices |
| `encoder_scaling.rs` | encoder-only passages/s and cores busy, N callers on one shared model | — (no server) |

## Run it

Use a **private port and a throwaway data directory**. `load.py` and
`ingest_bench.py` delete indices.

```sh
# BEIR data (public): https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/{scifact,nfcorpus}.zip
XERJ_TRACE_SEMANTIC_PHASES=1 NO_COLOR=1 xerj -c node.toml -d ./data --insecure --embed-mode neural > server.log 2>&1 &

U=http://localhost:9560; D=/path/to/beir
python3 load.py          $U scifact  $D/scifact
python3 load.py          $U nfcorpus $D/nfcorpus
python3 eval.py          $U scifact  $D/scifact
python3 zero_hits.py     $U nfcorpus $D/nfcorpus
python3 latency.py       $U scifact  $D/scifact 60 500      # 60 queries from offset 500
python3 phases.py server.log "semantic|<ARM_START>|<ARM_END>"   # stamps printed by latency.py
python3 ingest_bench.py  $U <server_pid> $D/scifact 400 1 4 8
python3 rrf_stability.py save $U scifact $D/scifact 40 before.json   # restart the node, save again, then `compare`
```

`encoder_scaling.rs` is not part of any crate; its header says how to build it
next to `xerj-ai`'s own `neural_throughput` example.

## Results — 2026-09-18

`v1.0.0-rc.74` release binary (and `xerj-ai` from `main` @ 4d8dadbf for the
encoder probe). AMD Ryzen AI Max+ 395, 16 cores / 32 threads, **shared with
other jobs at load average 16–31 throughout**. Absolute rates and milliseconds
are therefore conservative and noisy; ratios, CPU-seconds and cores-busy are
the robust columns. Not re-run on a quiet machine.

### Quality — nDCG@10, BEIR test splits (`ndcg-*.log`)

| Arm | SciFact (300 queries, 5,183 docs) | NFCorpus (323 queries, 3,633 docs) |
|---|---|---|
| BM25 — `multi_match` over `title`,`text` | 0.6572 | 0.3016 |
| MiniLM vectors only — `semantic` | 0.6764 | 0.3291 |
| BM25 top-30 re-ordered by the same bi-encoder | 0.6855 | 0.3323 |
| **`hybrid`, RRF, server-side** | **0.7044** (other runs: 0.6993, 0.7023) | **0.3450** (other runs: 0.3448, 0.3446) |

Hybrid RRF is the best arm XERJ has on both corpora, and re-ordering a BM25
shortlist with the same bi-encoder is worse than hybrid on both. The hybrid
figure is **not stable run to run** (see RRF below): two earlier runs of the
same four arms against the same indices are kept in `ndcg-earlier-runs.log`.
The other three arms reproduce to four decimals in all three runs. Quote hybrid
as a range (0.699-0.704 / 0.345), and treat a change smaller than that spread
as no change.

Latency columns in the `ndcg-*.log` files are not to be quoted: the rerank arm
re-issues queries earlier arms already sent, and an identical request is
answered from the result cache in about a millisecond. Use `latency-*.log`.

### Zero-hit BM25 queries (`zero_hits-*.log`)

NFCorpus: 25 of 323 test queries return no BM25 hit. **All 25 are legitimate**:
no query token occurs as a whole token in any of the 3,633 documents. 18 are
absent terms (`Zoloft`, `Czechoslovakia`, `eggnog`, …); 7 are plurals whose
singular is present (`bagels`, `turnips`, `leeks`, `pineapples`, `scanners`,
`sores`, `antinutrients`), which a stemming analyzer would match and `standard`
does not. SciFact: 0 of 300. Engine bugs: 0.

What this led to is a different defect: asking for a stemming analyzer does
nothing. A per-field `analyzer` is accepted and ignored, and an index-default
analyzer stops applying at flush.

### Neural ingest throughput (`ingest-scifact*.log`, `encoder-scaling-scifact.log`)

| | documents/s | passages/s | server cores busy |
|---|---|---|---|
| one `_bulk` stream, SciFact titles only (96 chars, 1 passage/doc) | 88.1 | 88.1 | 2.7 of 32 |
| 8 concurrent clients, titles only | 294.7 | 294.7 | 9.2 |
| one `_bulk` stream, SciFact (1,470 chars ≈ 3.9 passages/doc) | 6.1 | 23.5 | 3.4 of 32 |
| 4 concurrent clients | 18.9 | 72.9 | 10.8 |
| 8 concurrent clients | 29.5 | 113.7 | 15.7 |
| encoder only, 1 caller, Candle default threads | — | 31.1 | 3.5 |
| encoder only, 1 caller, 1 thread | — | 18.0 | 1.0 |
| encoder only, 16 callers × 1 thread, one shared model | — | 160.1 | 14.7 |
| encoder only, 32 callers × 1 thread | — | 169.1 | 18.4 |

Total server CPU for the 400 documents is the same at 1, 4 and 8 clients (226,
229, 213 CPU-seconds): the work is fixed and one stream does not occupy the
machine. One embedding call runs its forward passes one after another, and a
`_bulk` request awaits its embedding windows in sequence, so the only
parallelism is what Candle finds inside a single pass — about 3.5 cores, bought
at 2.03× the CPU of a single thread for 1.73× the speed.

### `semantic` / `hybrid` latency (`latency-scifact.log`, `phases.log`)

| p50, ms | BM25 | semantic | semantic + `ids` filter (30 docs) | hybrid RRF |
|---|---|---|---|---|
| client-side | 11.0 | 427.7 | 322.1 | 567.9 |
| embed the query (forward pass) | — | 14 | 12 | 19 |
| collect (load every stored document) | — | 216 | 197 | 331 |
| score | — | 107 | 105 | 106 |
| top-k + hits | — | 67 | 0 | 75 |

5,183 documents, every one of them multi-passage, so every request takes the
exact scan. On a 10,003-document index of one-sentence documents (HNSW
eligible) the same `semantic` query is 16 ms p50 client-side: 12 ms embed, 14 ms
total server-side, 0 of 37 requests on the exact scan. The forward pass is not
the problem; the scan deep-copies every stored `_source` per query, before the
filter is applied.

### RRF order across a restart (`rrf-stability-scifact.log`)

Same data directory, node stopped and started, no writes: 32 of 40 queries
returned a different order and 21 of 40 a different top 10, with identical hit
sets. Every differing pair has bit-identical fused scores. Within one process,
two passes returned identical orders.
