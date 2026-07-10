# Use case: "Claude + XERJ — search a recursive folder of mixed-format documents"

**One-line:** Point an AI agent at a wide, recursive folder of real documents
(PDF, DOCX, HTML, Markdown, TXT). With XERJ it gets a format-agnostic,
meaning-aware, ranked, cited search index. With only shell scripts (grep/rg) it
is blind to binary formats, blind to meaning, and blows up its context window.

This directory is the production deliverable. Everything here must run **live**
against a real XERJ node and report **measured** numbers — no assertions, no
fabricated results, honest caveats.

## The two approaches being compared

- **Baseline — "Claude Code with scripts only":** the agent answers questions by
  running `rg`/`grep` over the raw files and reading matches into its context.
  Honest capability ceiling: (1) PDF/DOCX are binary → `rg` sees gibberish and
  matches nothing; (2) only literal substring match — no semantic/meaning
  retrieval; (3) to "read" a hit it must load whole files into context (tokens);
  (4) no ranking; (5) re-scans the whole tree every query.
- **XERJ-backed — "Claude + XERJ":** a one-time `xerj-index` pass walks the
  folder, extracts text from **every** format, chunks + embeds via XERJ, and bulk
  indexes. The agent then queries XERJ (lexical + semantic + hybrid) and gets
  ranked, cited passages back — format-agnostic, meaning-aware, no context blowup,
  incrementally re-indexable.

The comparison must be **fair**: same corpus, same query set, baseline given the
best plain-shell tools available (`rg`, `pdftotext` is NOT given to the baseline —
the whole point is that a naive script agent greps raw bytes; but we DO note in
the writeup that a script agent *could* shell out to `pdftotext`, and explain why
that still loses on format coverage, ranking, and context cost).

### REVISION 2 — honesty fix (an adversarial audit found the v1 comparison unfair)

The v1 run rigged the `semantic_only` set: the baseline was handed a curated
keyword list that dodged the answer line, while the answer actually shared a line
with the **question's own words**. A fair shell agent greps the question's terms
and finds those answers. So the corrected, HONEST value proposition is:

1. **Format coverage — the decisive, unassailable win.** grep reads raw bytes, so
   PDF/DOCX/other binary formats are invisible (0 hits). XERJ extracts + indexes
   them. This is a capability grep structurally lacks. THIS is the headline.
2. **Context efficiency at document scale — a real win, but only on large docs.**
   To answer from a big document a shell agent must load the whole file into
   context; XERJ returns one ranked passage. On small files this INVERTS (passage
   overhead > tiny file), so the corpus MUST include realistically large documents
   to demonstrate it honestly.
3. **Ranked, single-query retrieval — convenience/robustness, NOT a capability
   gap.** One query ranks passages across the whole corpus; the shell agent must
   guess terms/synonyms. XERJ's built-in LEXICAL embedder adds modest sub-word
   robustness — but it is NOT neural, so on plaintext a diligent grep of the
   question's own terms can TIE it. Do NOT claim a decisive semantic-understanding
   win. Report the fair (≈tie) semantic result truthfully.

**Fair-baseline rule (mandatory):** `grep-baseline.mjs` must derive its search
terms from BOTH the curated `keywords` AND the salient content tokens of the
`question` itself, and search all of them. It must never be capped to a keyword
list that structurally excludes the answer's own line.

## Honesty bar (non-negotiable — this repo has a standing honesty standard)

- XERJ's built-in embedder is **lexical feature-hashing (384-dim cosine), not
  neural**. Semantic wins here are word/sub-word overlap, not deep meaning. Say so.
- kNN is **exact brute-force at query time**. Fine at this corpus size; disclose.
- Do NOT claim XERJ "understands" documents. Claim: format-agnostic extraction +
  ranked retrieval + context efficiency + a real (if shallow) semantic layer.
- Every number in the recipe/website MUST come from `results.json` produced by a
  live run. If the gate fails, report the real result, do not massage it.

## Directory layout (all under demo/usecases/doc-index/)

- `gen-corpus.mjs`   — generates the corpus + `queries.json` (runs pdftotext/soffice)
- `corpus/`          — generated: `<topic>/<name>.{pdf,docx,html,md,txt}`
- `queries.json`     — ground-truth query set (see schema below)
- `xerj-index.mjs`   — the indexer: walk → extract → chunk → bulk-index into XERJ
- `grep-baseline.mjs`— the baseline "scripts" retriever (rg over raw files)
- `compare.mjs`      — runs both over queries.json, scores, writes results.json + SCORECARD.md
- `results.json`     — measured comparison output
- `SCORECARD.md`     — human-readable measured scorecard
- `README.md`        — how to run it end-to-end (community-facing)

## XERJ index contract (indexer + compare MUST agree)

- Node listen: ES-compat REST at `http://localhost:${XERJ_PORT:-9209}` (`--insecure`).
- Index name: `docfolder`.
- Create with mappings:
  - `path`     : keyword   (absolute or repo-relative file path)
  - `dir`      : keyword   (parent folder, for folder-scoped search)
  - `format`   : keyword   (pdf|docx|html|md|txt)
  - `title`    : text      (derived from filename / first heading)
  - `chunk_id` : integer
  - `body`     : `semantic_text` (auto-embed; built-in lexical embedder) — for semantic/hybrid
  - `body_text`: text      (SAME chunk text; for BM25 lexical / match / match_phrase)
- Chunking: ~800 chars, ~100 char overlap, split on paragraph/sentence boundaries
  where possible. Store both `body` and `body_text` = the chunk text.
- Ingest via `POST /docfolder/_bulk` (NDJSON), batched (e.g. 500 docs/batch).
- Extraction per format (in the INDEXER — it is allowed the good tools):
  - `.pdf`  → `pdftotext -layout <f> -`
  - `.docx` → `soffice --headless --convert-to txt:Text --outdir <tmp> <f>` then read .txt (fallback: unzip word/document.xml + strip tags)
  - `.html` → strip tags/script/style to text (stdlib parser or a small tokenizer)
  - `.md`/`.txt` → read as-is (strip md syntax lightly for .md)
- Be robust: skip unreadable files with a logged warning; never crash the run.

## Query set contract (`queries.json`)

Array of objects; **~18 queries** spanning the trap cases:
```
{
  "id": "q01",
  "question": "How many weeks of parental leave do employees get?",
  "keywords": ["parental leave", "weeks"],   // salient terms the baseline would grep
  "expect_substring": "16 weeks",             // ground-truth answer text (case-insensitive)
  "answer_path": "corpus/hr/handbook.pdf",    // file the answer lives in
  "answer_format": "pdf",
  "match_type": "binary_only"                 // binary_only | semantic_only | literal
}
```
Distribution (REVISION 2 — reflects the honest value prop above):
- **binary_only** (answer lives ONLY in a `.pdf` or `.docx`): ~7 queries. XERJ
  finds them; the grep baseline structurally cannot. **This is the headline win.**
- **large_literal** (answer is a literal string buried deep inside a REALISTICALLY
  LARGE document, ≥60 KB, any format): ~4 queries. A FAIR baseline finds the line
  but must load the whole large file into context to use/verify it; XERJ returns
  one ranked passage. This is where the **context-efficiency** win is demonstrated
  honestly — so both approaches should HIT, but XERJ's returned bytes ≪ the file
  the baseline must open.
- **robustness** (formerly "semantic_only"; answer phrased differently than the
  query, in html/md/txt): ~4 queries. Report the FAIR result: a diligent grep of
  the question's own terms will likely TIE XERJ here — that is the honest finding
  (lexical embedder ≈ fuzzy word-overlap). Keep them to show XERJ's single-query
  convenience, NOT to claim a semantic-understanding win.
- **literal** (answer is a literal string in a small plaintext html/md/txt file):
  ~5 queries. BOTH find these (honest control — XERJ isn't cheating on easy cases).

`match_type` values: `binary_only | large_literal | robustness | literal`.

Corpus domain: a realistic fictional company internal KB — "Northwind Robotics".
HR handbook (PDF), engineering/on-call runbook (DOCX), product FAQ (HTML),
architecture notes (MD), meeting/decision notes (TXT). Content must be coherent and
plausible, NOT lorem ipsum. NO real personal names / usernames.

## Scoring (in `compare.mjs`)

For each query, per approach, obtain the top passages/hits and check:
- **hit** = any returned passage/line contains `expect_substring` (case-insensitive).
- XERJ: run the best supported retrieval (probe the live node — prefer `hybrid`
  {match on body_text + semantic on body, rrf}; fall back to semantic-only merged
  with match if hybrid errors). size=5.
- Baseline (FAIR): `rg -i -n` for the union of the query `keywords` AND salient
  content tokens extracted from the `question` itself, across `corpus/`; a hit
  requires a matched line containing `expect_substring`. Binary files → no match
  (that is the real capability gap, not a rigged one).
- **Context cost (measured HONESTLY):** compute the token ratio ONLY over the
  queries the BASELINE actually answers (it is meaningless on queries it can't
  answer at all). For each such query: baseline cost = bytes of the file(s) it must
  open to read/verify the answer (≈ chars/4 tokens); XERJ cost = bytes of the
  ranked passage(s) it returns for that query. Report `context_ratio =
  baseline_bytes / xerj_bytes` over answerable queries, AND break it out for the
  `large_literal` set (where it should be ≫ 1×). Also report the naive overall
  ratio but LABEL it as flattering the blind baseline.
- Emit per-query rows + aggregates: coverage% (each approach), per-format coverage,
  per-match_type coverage (binary_only / large_literal / robustness / literal),
  context_ratio (answerable + large_literal), index build time, query latency.
  SCORECARD verdict must state the honest framing: headline = binary_only; context
  win shown on large_literal; robustness ≈ tie under fair baseline (lexical embedder).

## GATE (must all hold on the live run, or report the honest failure)

1. `xerj-index` indexes the whole corpus end-to-end against a live node, 0 crashes.
2. XERJ overall coverage **≥** baseline, AND **strictly higher on `binary_only`**
   (the decisive, capability-based differentiator). `robustness` is reported
   honestly and may TIE — do NOT rig it to win.
3. `context_ratio` on the `large_literal` set is **> 1×** (XERJ returns far fewer
   tokens than the whole large files the baseline must load).
4. Every number in SCORECARD.md / results.json is from the live run (reproducible),
   and the SCORECARD verdict carries the honest framing (no overstated semantic win).
5. Recipe + website + README carry the honesty caveats and cite only measured
   numbers, and `README.md` lets a community user reproduce the whole thing.
