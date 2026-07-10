# Doc-index scorecard — Claude + XERJ vs. shell-only (grep) baseline

_Measured live on 2026-07-09T05:12:07.563Z against `http://localhost:9209` (index `docfolder`, 420 chunks). Retrieval mode: **hybrid**. Every number below is from this live run._

> **Honest framing (REVISION 2).** The decisive, capability-based win is **binary_only** — a fair grep structurally cannot read PDF/DOCX bytes. **Context efficiency** is a real win, but only shows up on **large_literal** (big documents). **robustness** ≈ **TIE** under a fair baseline, because XERJ's built-in embedder is a LEXICAL feature-hashing model (384-dim cosine), NOT neural — a diligent grep of the question's own terms matches the same lines. **literal** is the honest control. No overstated semantic claim is made.

## Per-query results

| ID | Type | Fmt | XERJ | Base | XERJ ms | XERJ psg B | Best psg B | Base answer-file B | Base matched B (diag) | Question |
|----|------|-----|------|------|--------:|-----------:|-----------:|-------------------:|----------------------:|----------|
| q01 | binary_only | pdf | hit | — | 116.09 | 3433 | 737 | 0 | 197133 | How many weeks of paid parental leave do employees receive? |
| q02 | binary_only | pdf | hit | — | 71.74 | 3771 | 737 | 0 | 197808 | How many days of paid time off do full-time employees acc... |
| q03 | binary_only | docx | hit | — | 63.15 | 3621 | 693 | 0 | 197796 | How many days of bereavement leave are provided per event? |
| q04 | binary_only | docx | hit | — | 74.96 | 3661 | 733 | 0 | 198770 | After how long does an unacknowledged page escalate to th... |
| q05 | binary_only | docx | — | — | 61.92 | 3703 | 809 | 0 | 196317 | What is the acknowledgement SLA for a Sev-1 incident? |
| q06 | binary_only | pdf | hit | — | 70.71 | 3780 | 710 | 0 | 197649 | How long must customer transaction records be retained? |
| q07 | binary_only | pdf | hit | — | 62.48 | 3728 | 693 | 0 | 196826 | What is the domestic meal per diem rate? |
| ll01 | large_literal | html | hit | hit | 59.57 | 3702 | 786 | 62204 | 195057 | How much is the employee referral bonus for a successful ... |
| ll02 | large_literal | md | hit | hit | 60.41 | 3765 | 765 | 64837 | 195245 | What is the default value of the ingest max_batch_size pa... |
| ll03 | large_literal | txt | hit | hit | 60.08 | 3713 | 699 | 65741 | 195532 | How quickly must a public status-page update be posted af... |
| ll04 | large_literal | pdf | hit | — | 58.56 | 3425 | 673 | 0 | 196546 | Within how many days must critical vulnerabilities be rem... |
| s01 | robustness | md | hit | hit | 62.32 | 3834 | 751 | 770 | 204563 | What is Northwind's policy on remote work versus coming i... |
| s02 | robustness | html | hit | hit | 62.99 | 3699 | 661 | 842 | 196355 | How does a user recover access to a locked account? |
| s03 | robustness | md | hit | hit | 60.06 | 3781 | 738 | 832 | 200772 | What developer hardware is issued to a newly hired engine... |
| s04 | robustness | md | hit | hit | 60.04 | 3476 | 760 | 1105 | 195368 | Which data store backs the inventory and SKU records? |
| s05 | robustness | html | hit | hit | 60.88 | 3815 | 781 | 991 | 198030 | What can a customer do if they want to send a purchased u... |
| l01 | literal | md | hit | hit | 59.43 | 3788 | 743 | 783 | 196826 | What is the default API rate limit? |
| l02 | literal | md | hit | hit | 61.11 | 3867 | 760 | 1105 | 200764 | What port does the ingest service listen on? |
| l03 | literal | md | hit | hit | 60.09 | 2991 | 605 | 643 | 195610 | When is the weekly engineering sync held? |
| l04 | literal | html | hit | hit | 58.16 | 3623 | 781 | 991 | 68849 | What uptime does the platform SLA guarantee? |
| l05 | literal | html | hit | hit | 61.34 | 3280 | 548 | 722 | 198316 | Who must approve purchases over the standard threshold? |
| l06 | literal | txt | hit | hit | 61.68 | 3416 | 493 | 497 | 198614 | What is the Q3 target for warehouse pick accuracy? |

_"Base answer-file B" is the HONEST context charge: the size of the single answer-containing file the baseline must open, and **0 when the baseline cannot answer** (it reads nothing toward an answer it cannot find). "Base matched B (diag)" is a DIAGNOSTIC only — the size of every file the baseline's broad terms matched, false positives included — and is used in no ratio or claim._

## Aggregate coverage by match type

| Metric | XERJ | Baseline | Note |
|--------|------|----------|------|
| Overall coverage | 21/22 (95.5%) | 14/22 (63.6%) | XERJ ≥ baseline |
| **binary_only** (answer only in PDF/DOCX) | 6/7 | 0/7 | **HEADLINE — capability grep lacks** |
| large_literal (buried in a ≥60 KB doc) | 4/4 | 3/4 | both hit; see context win below |
| robustness (differently-phrased) | 5/5 | 5/5 | ≈ TIE under fair baseline (lexical embedder) |
| literal (plaintext substring) | 6/6 | 6/6 | honest control — both read plain text |

### Coverage by answer format

| Format | Queries | XERJ | Baseline |
|--------|--------:|------|----------|
| docx | 3 | 2/3 (66.7%) | 0/3 (0%) |
| html | 5 | 5/5 (100%) | 5/5 (100%) |
| md | 7 | 7/7 (100%) | 7/7 (100%) |
| pdf | 5 | 5/5 (100%) | 0/5 (0%) |
| txt | 2 | 2/2 (100%) | 2/2 (100%) |

### Context efficiency (measured honestly)

Context ratio = baseline bytes to open ÷ XERJ bytes returned. The baseline is charged ONLY the single answer-containing file it must open (`statSync(answer_path)`), **never** the false-positive files its broad terms also matched. `grep -n` yields a matching line, but to quote/verify an answer reliably an agent opens the whole answer file — so that single file is the charge (stated plainly as the assumption).

| View | Baseline bytes | XERJ bytes | Ratio | What it means |
|------|---------------:|-----------:|:-----:|---------------|
| **large_literal** (returned passages) | 192,782 | 11,180 | **17.24×** | THE REAL WIN — big files vs. ranked passages (over 3 query/queries the baseline answers) |
| large_literal (single best passage) | 192,782 | 2,250 | 85.68× | one passage the agent would actually quote |
| **literal** (returned passages) | 4,741 | 20,965 | **0.23×** | SCALE-DEPENDENCE: on tiny plaintext files the win INVERTS — 5 returned passages ≈/exceed the whole small file (over 6 query/queries) |
| literal (single best passage) | 4,741 | 3,930 | 1.21× | even a single passage ≈ a tiny file |
| answerable (all queries baseline answers) | 202,063 | 50,750 | 3.98× | fair whole-corpus view over 14 answerable queries |
| naive overall (all queries) | 202,063 | 79,872 | 2.53× | **flatters the blind baseline — it literally opens 0 bytes on every query it cannot answer** — NOT a claim |

**Scale-dependence, side by side:** large_literal **17.24×** (answers buried in ≥60 KB docs) vs. literal **0.23×** (tiny plaintext files). The context win is real on large documents and INVERTS on small ones — demonstrated on measured data, not asserted.

_Diagnostic (NOT a claim): had the baseline instead been charged every file its broad terms matched — false positives included — the total would be 4,218,746 bytes. That inflated charge is exactly what this scorecard does NOT use._

_Latency: XERJ query p50 / mean / max = 61.23 / 64.9 / 116.09 ms. Index build time: 1339 ms._

### Gate

- (#2) Overall coverage ≥ baseline: **PASS**
- (#2) binary_only strictly higher (the capability win): **PASS**
- (#3) large_literal context ratio > 1×: **PASS** (17.24×)
- (context, informational) literal ratio ≈1× or below — the small-file inversion that proves scale-dependence: 0.23× — **NOT gated**.
- robustness is reported honestly and may TIE — **NOT gated**.
- **GATE: PASS**

## Verdict

On this 22-query set, XERJ answered 21/22 (95.5%) versus the fair shell-only baseline's 14/22 (63.6%). **Headline (the one decisive, capability-based win): binary_only.** Answers that live only inside PDF/DOCX are invisible to ripgrep — it sees compressed/binary bytes and matches nothing — so the baseline gets 0/7, while XERJ, which extracted and indexed that text, gets 6/7 (+6). This is a capability grep structurally lacks, not a tuning artifact. **Context efficiency — real, but scale-dependent (shown, not asserted).** Over the 3 large_literal query/queries (answers buried in ≥60 KB docs that a fair grep DOES find), the baseline must open 192,782 bytes of whole answer files to quote/verify the line, while XERJ returns 11,180 bytes of ranked passages — **17.24×** less context (85.68× counting only the single best passage the agent would actually quote). On the 6 tiny-file **literal** query/queries the SAME metric INVERTS: XERJ returns 20,965 bytes of passages against the baseline's 4,741 bytes of tiny answer files — just **0.23×** (1.21× counting only the single best passage), because five returned passages meet or exceed a small whole file. Side by side — large_literal **17.24×** vs. literal **0.23×** — IS the scale-dependence, measured rather than claimed. Throughout, the baseline is charged ONLY the single answer-containing file it must open (`statSync(answer_path)`), never the false-positive files its broad terms also matched; `grep -n` yields a matching line, but to quote/verify an answer reliably an agent opens the whole file. The naive whole-corpus ratio is 2.53×, but it flatters the blind baseline — which now literally opens 0 bytes on every query it cannot answer. **robustness ≈ TIE (honest).** On differently-phrased answers the fair baseline — which greps the union of the curated keywords AND the salient tokens of the question itself — gets 5/5, and XERJ gets 5/5 (+0). XERJ's built-in embedder is a LEXICAL feature-hashing model (384-dim cosine), NOT neural, so its "semantic" matching is word/sub-word overlap, not deep understanding — a diligent grep of the question's own terms matches the same lines. We report this as a single-query convenience/robustness tie, NOT a semantic-understanding win. **literal — honest control.** On plaintext substring cases both approaches read plain text (XERJ 6/6, baseline 6/6); this confirms XERJ is not inflating the easy cases. **Caveats.** kNN is exact brute-force at query time (fine at this corpus size). The baseline could be upgraded to shell out to `pdftotext`/`soffice` to close the binary gap — but it would still lack ranking, per-chunk retrieval, and the (shallow, lexical) semantic layer, and would keep paying the whole-file context cost on large documents. **Gate: PASS.**
