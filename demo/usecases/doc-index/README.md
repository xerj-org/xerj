# Doc-index — search a recursive folder of mixed-format documents with XERJ

Point an AI agent at a real, messy shared drive — an HR handbook in **PDF**, an
on-call runbook in **Word**, a product FAQ in **HTML**, architecture notes in
**Markdown**, meeting notes in **plain text** — and let it answer questions with
*cited passages*, regardless of format.

This directory is a self-contained, reproducible use-case deliverable. One
`xerj-index` pass walks the folder, extracts text from **every** format, chunks +
auto-embeds each chunk, and bulk-indexes it into a single `docfolder` index. A
comparison harness then scores XERJ against the honest shell-only baseline (`rg`
over the raw files) and writes a measured scorecard.

**Everything here runs live and reports measured numbers.** The published recipe
page — <https://xerj.org/docs/recipes/document-folder-index.html> — cites only the
numbers this harness emits into `results.json` / `SCORECARD.md`.

---

## What gets compared

- **Baseline — "Claude Code with scripts only":** answer questions by running
  `rg`/`grep` over the raw files. Its ceiling is real and structural: (1) PDF/DOCX
  are compressed binary, so `rg` sees NUL bytes and matches nothing; (2) literal
  substring only — no meaning/semantic retrieval; (3) to *quote* an answer it must
  read whole files into context; (4) no ranking; (5) re-scans the whole tree every
  query.
- **XERJ-backed — "Claude + XERJ":** a one-time `xerj-index` pass extracts text
  from every format, chunks (~800 chars, ~100 overlap), auto-embeds with XERJ's
  built-in embedder, and bulk-indexes. The agent then queries XERJ (lexical +
  semantic + hybrid) and gets ranked, cited passages back.

The comparison is deliberately fair: same corpus, same 22-question ground-truth
set, and the baseline is given ripgrep (a good plain-shell tool). Per the
fair-baseline rule, the grep baseline searches the union of each query's curated
keywords AND the salient tokens of the question itself — it is never capped to a
keyword list that dodges the answer's own line. See `SPEC.md` for the full
contract (index mappings, chunking, query schema, scoring, and the gate).

---

## Measured result (from the live run)

| Metric | XERJ | Baseline (grep) |
|--------|------|-----------------|
| Overall coverage | **21/22 (95.5%)** | 14/22 (63.6%) |
| **binary_only** (PDF/DOCX-only answers) — **HEADLINE** | **6/7** | 0/7 |
| large_literal (answer buried in a ≥60 KB doc) | 4/4 | 3/4 |
| robustness (differently phrased) | 5/5 | 5/5 |
| literal (plaintext substring) | 6/6 | 6/6 |
| Query latency p50 / mean / max (ms) | 61.23 / 64.9 / 116.09 | — |
| Index build time | 29 files → 420 chunks in **1339 ms** | — |

**Gate: PASS** — the whole corpus indexed end-to-end with 0 crashes; XERJ's overall
coverage ≥ baseline (21 ≥ 14); XERJ is strictly higher on the decisive
`binary_only` set (6 > 0); and the `large_literal` context ratio is **17.24× > 1×**.
`robustness` is reported honestly as a **tie** and is not gated.

Honest notes carried in the scorecard:

- **The decisive win is format coverage (`binary_only`), not "meaning".** grep reads
  raw bytes, so a PDF/DOCX answer is invisible to it (0/7); XERJ extracted and
  indexed that text and answers 6/7. This is a capability grep structurally lacks.
- **Context efficiency is a real win — but scale-dependent, not flat.** The baseline is
  charged **only the single answer-containing file it must open** (`statSync(answer_path).size`,
  and **0 bytes on any query it cannot answer**) — never the false-positive files its broad
  terms also matched. Measured that way the ratio VARIES by document size. Over the 3
  `large_literal` queries the fair baseline actually answers, it must open **192,782 bytes**
  of whole answer files to quote/verify the line, while XERJ returns **11,180 bytes** of
  ranked passages — **17.24×** less context (85.68× counting only the single best passage).
  But on the 6 small-file `literal` queries the SAME metric INVERTS: XERJ returns **20,965
  bytes** of passages against the baseline's **4,741 bytes** of tiny answer files — just
  **0.23×** (≈ no win — on a tiny plaintext file you'd simply read the whole thing). Side
  by side — large_literal **17.24×** vs `literal` **0.23×** — IS the scale-dependence,
  measured not asserted. Across all 14 answerable queries the ratio is **3.98×**; the naive
  whole-corpus ratio (**2.53×**) FLATTERS the blind baseline (it opens 0 bytes on every query
  it cannot answer) and is **NOT** a claim. _(Diagnostic only, never used in any ratio or
  claim: had the baseline instead been charged every file its broad terms matched — false
  positives included — the total would be 4,218,746 bytes.)_
- **`robustness` ≈ TIE (honest).** On differently-phrased answers a fair grep of the
  question's own salient terms matches the same lines XERJ's lexical embedder finds
  — 5/5 vs 5/5. XERJ's built-in embedder is **lexical feature-hashing** (384-dim
  cosine), **not neural**, so its "semantic" matching is word/sub-word overlap, not
  comprehension. We do **not** claim a semantic-understanding win here.
- kNN is **exact brute-force** at query time (fine at this corpus size).
- **One miss (q05):** "What is the acknowledgement SLA for a Sev-1 incident?"
  (answer "within 5 minutes" in `oncall-runbook.docx`). The chunk *was* extracted
  and indexed — a direct `match_phrase` on the live index finds it — but for this
  phrasing the hybrid ranking pushes it out of the top-5 (the answer chunk lacks the
  word "SLA"; larger incident-heavy docs outrank it). Reported truthfully as
  `binary_only` 6/7, still strictly > the baseline's 0/7. Not massaged.

---

## Prerequisites

- **Node** 18+ (tested on v24) — no npm dependencies; all scripts are stdlib-only.
- **`pdftotext`** (poppler-utils) — PDF text extraction.
- **`soffice`** / LibreOffice — DOCX extraction (indexer falls back to `unzip`).
- **`rg`** (ripgrep) — the baseline retriever.
- A **XERJ** binary on your PATH (or adjust the `xerj` command below to its path).

The `corpus/` folder and `queries.json` are already checked in, so you can skip
`gen-corpus.mjs` unless you want to regenerate them (that step needs `pdftotext`
and `soffice`).

---

## Reproduce it end-to-end

All commands run from this directory (`demo/usecases/doc-index/`).

```bash
# 1. Start a throwaway XERJ (ES-compat wire; default port :9200).
#    --insecure disables TLS + auth for local dev. Use a scratch data dir.
xerj --insecure --data-dir ./docfolder-data

# 2. In another shell, point the scripts at your node's ES-compat port.
#    The scripts default to :9209; a fresh XERJ listens on :9200.
cd demo/usecases/doc-index
export XERJ_PORT=9200

# 3. (Optional) regenerate the corpus + ground-truth queries.json.
#    Needs pdftotext + soffice; skip it to use the checked-in corpus.
node gen-corpus.mjs

# 4. Extract, chunk, auto-embed and bulk-index the whole folder.
#    --recreate deletes + rebuilds the index so re-runs are clean.
node xerj-index.mjs --recreate

# 5. (Optional) record the index build time so the scorecard can cite it.
#    xerj-index prints "elapsed ms : <N>" — write that number here:
echo 1339 > .index_build_ms      # replace 1339 with YOUR run's elapsed ms

# 6. Score XERJ vs the grep baseline. Writes results.json + SCORECARD.md.
node compare.mjs

# 7. Read the measured verdict.
cat SCORECARD.md
```

When you're done, stop the node (Ctrl-C) and remove the scratch data dir
(`rm -rf ./docfolder-data`).

### Expected output

`xerj-index.mjs --recreate` prints a summary like:

```
===== xerj-index summary =====
files walked      : 29
chunks produced   : 420
chunks indexed    : 420
chunks errored    : 0
files skipped     : 0
elapsed ms        : 1339
```

Confirm the index is populated:

```bash
curl -s "localhost:${XERJ_PORT}/docfolder/_count"
# {"count":420, ...}
```

`compare.mjs` writes `results.json` (machine-readable rows + aggregates + gate) and
`SCORECARD.md` (per-query table + honest verdict), then prints where it wrote them.
It exits `0` whether or not the gate passes — a gate failure is reported honestly,
never massaged. It exits `1` only if it *couldn't run* (node unreachable,
`docfolder` missing, `queries.json`/`grep-baseline.mjs` missing).

---

## Poke at it directly

The three retrieval shapes the harness uses (substitute your own question):

```bash
# Lexical — BM25 over the extracted text.
curl -s "localhost:${XERJ_PORT}/docfolder/_search" -H 'Content-Type: application/json' -d '{
  "size": 5, "query": { "match": { "body_text": "default API rate limit" } }
}'

# Semantic — meaning-ish retrieval over the auto-embedded body.
curl -s "localhost:${XERJ_PORT}/docfolder/_search" -H 'Content-Type: application/json' -d '{
  "size": 5, "query": { "semantic": { "field": "body",
             "query": "policy on remote work versus coming into the office", "k": 5 } }
}'

# Hybrid — fuse lexical + semantic with RRF (what compare.mjs scores).
curl -s "localhost:${XERJ_PORT}/docfolder/_search" -H 'Content-Type: application/json' -d '{
  "size": 5, "query": { "hybrid": { "fusion": "rrf", "queries": [
    { "query": { "match":    { "body_text": "weeks of paid parental leave" } } },
    { "query": { "semantic": { "field": "body", "query": "weeks of paid parental leave", "k": 5 } } }
  ] } }
}'
```

And the honest baseline, for contrast — note it finds **nothing** for the
parental-leave question because the answer lives only in a binary PDF:

```bash
node grep-baseline.mjs q01     # binary_only → miss (answer locked in a binary PDF)
node grep-baseline.mjs l01     # literal     → hit (finds the answer line in a .md)
```

---

## Files in this directory

| File | Role |
|------|------|
| `SPEC.md` | The contract: index mappings, chunking, query schema, scoring, gate. |
| `gen-corpus.mjs` | Generates `corpus/` + `queries.json` (needs pdftotext + soffice). |
| `corpus/` | The generated fictional-company KB (PDF/DOCX/HTML/MD/TXT). |
| `queries.json` | 22 ground-truth queries (7 binary_only, 4 large_literal, 5 robustness, 6 literal). |
| `xerj-index.mjs` | The indexer: walk → extract → chunk → bulk-index into XERJ. |
| `grep-baseline.mjs` | The shell-only baseline retriever (ripgrep over raw files). |
| `compare.mjs` | Runs both over `queries.json`, scores, writes `results.json` + `SCORECARD.md`. |
| `results.json` | Measured comparison output (machine-readable). |
| `SCORECARD.md` | Measured, human-readable scorecard + verdict. |

### Script options worth knowing

- `xerj-index.mjs`: `--dir <folder>` (default `./corpus`), `--recreate`,
  `--batch <n>` (docs per `_bulk`, default 200), `--dry-run` (extract + chunk +
  print stats, don't touch XERJ). Env: `XERJ_PORT` (default 9209).
- `compare.mjs`: env `XERJ_PORT` / `XERJ_URL`, `INDEX_BUILD_MS` (or
  `--index-build-ms=<N>`, or the `.index_build_ms` file) to cite build time.
- `grep-baseline.mjs`: env `DOC_INDEX_CORPUS`, `DOC_INDEX_QUERIES`, `RG_BINARY`.

---

## Notes on fairness and honesty

A script agent *could* shell out to `pdftotext`/`soffice` to defeat the binary
blindness — this baseline deliberately does not, because the point is the naive
shell-only agent. But even a `pdftotext`-augmented baseline would still lack
ranking, per-chunk retrieval, and the semantic layer, and would keep paying the
whole-file context cost. XERJ's win is not "it can read PDFs" alone; it is
format-agnostic extraction **plus** chunked, ranked, context-aware retrieval with a
real (if shallow, lexical) semantic layer. Every claim above is reproducible from
the run this README describes.
