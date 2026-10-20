# #1111 — Fair A/B: does corpus + XERJ give a security-review agent an edge?

One question, one codebase, two blinded arms, one manual validation gate. Full protocol:
[protocol.md](protocol.md). Deviations: [DEVIATIONS.md](DEVIATIONS.md). Verdict-level evidence:
[validation/VERDICTS.md](validation/VERDICTS.md). Raw runs: [runs/](runs/) (pinned SHA
`eb2f5689`, per-run cost/token/wall in `runs/*/usage.raw.json` + `runmeta.json`).

- **Arm P (plain agent):** full repo checkout, no corpus tooling. 2 independent runs (p1, p2).
- **Arm X (corpus + XERJ):** repo + `rust-vulns` corpus served by a XERJ node (`xerj code`
  retrieval available). 2 independent runs (x1, x2).
- Same task file, same turn budget, same pinned binary for dynamic PoCs, same launcher
  (stdin — see DEVIATIONS), same validator (me, manually, per the user's directive: every
  finding gets a dynamic PoC or is disqualified/downgraded).

## Headline for management

**The corpus did not win this comparison, and we are publishing that.**

|  | P — plain agent | X — corpus + XERJ |
|---|---|---|
| Findings filed | 20 | 18 |
| **Confirmed exploitable (dynamic PoC)** | **13** | **16** |
| Confirmed (mechanism/headers real, no exploit) | 2 | 0 |
| Confirmed but downgraded (real, smaller than claimed) | 3 | 0 |
| Confirmed by code inspection only | 1 | 2 |
| **False positives** | **1 — the arm's only CRITICAL-severity claim** | **0** |
| Precision (fully or partially confirmed) | 19/20 = 95% | 18/18 = 100% |
| Cost (2 runs) | $65.72 | $70.55 |
| Cost per filed finding | $3.29 | $3.92 |
| **Cost per confirmed-exploitable finding** | **$5.06** | **$4.41** |
| Wall clock (2 runs) | 99.7 min | 89.8 min |
| Tokens in (+cache read) | 13.11 M | 14.84 M |
| Tokens out | 66.6 k | 70.3 k |

Precision, per-finding exploit yield, and zero false positives go to **X**. But see
provenance before crediting the corpus.

## The one number that decides the claim

**No confirmed finding in either X run traces to corpus-assisted discovery.**

- x1 invoked `xerj code rust-vulns` exactly **once** — a 23-dependency sweep whose output
  produced no finding that appears in its report. Its own method note describes pure source
  reading + subagent fan-out + live re-verification.
- x2 invoked it **zero times**. Its method note: read the sources directly, five parallel
  subagents, re-verify everything live.

So the measured difference between the arms is **agent method variance** (X's runs chose a
verify-everything-dynamically discipline, which is what killed its false positives), not a
corpus effect. Under this protocol, **corpus + XERJ as deployed gave no measurable discovery
advantage on the project's own Rust code** — a tie on the central question, published as a
tie per the directive. What X's discipline did show: dynamic re-verification is worth
~1 false positive and ~3 overclaims per 20 findings, and that lesson is portable to any arm.

Why this is the expected failure mode (and what would fix it): the rust-vulns corpus indexes
*vulnerability patterns from other projects*. Both agents were reviewing code the model
already knows extremely well — XERJ's own idiom, in XERJ's own repo. Retrieval wins on code
the model has not memorised (the corpus-builder arc's own measurement: 1.65× fewer output
tokens on un-memorised code). #1110 Part A therefore re-tests on a second pack (CISA KEV,
different domain) before any "corpus helps/hurts" product claim is made.

## What the two arms found is startlingly disjoint

38 filed findings dedupe to **33 distinct confirmed defect classes**. Only **2** classes were
found by both arms (docvalue `format` char-boundary panic; .seg footer tamper). Neither arm
came close to exhausting the surface alone:

| | unique confirmed classes | solo recall vs union |
|---|---|---|
| P alone | 17 | 17/33 = 52% |
| X alone | 18 | 18/33 = 55% |
| union | 33 | — |

P exclusively found the dot-path recursion family (3 stack-overflow variants), the pipeline
DAG exponential expansion, the suggester CPU amplifier, and the supply-chain trio
(git-`rev` argv injection, pack-slug traversal, autoindex TLS). X exclusively found the
sort/painless char-boundary kills, `number_of_shards:0`, the .norms/_seq_no persistence pair,
and most of the quiet-authz set (ingest escalation, metrics-token leak, task-id reads,
share-rate kill switch, console headers/cookie/MCP interpolation).

The honest reading: **run two independent agents, expect ~50% overlap with the best single
run.** That is a statement about review depth per budget, not about which arm won.

## Severity of what was confirmed (both arms, post-gate)

- **Whole-process kills from one cheap request: 11** distinct entry points (panic=abort:
  every reachable panic is node death). Includes 3 persistent-after-restart kills
  (poisoned .seg/.norms files boot green, die on first query — 8-byte and 4-byte patches).
- **Silent permanent data loss:** tampered `_seq_no` + one merge = 51 docs → 1, no error
  logged, CRC recomputed and accepted.
- **Supply-chain RCE as the operator:** corpus recipe `rev` → `--upload-pack` (sentinel file
  created); checksum-VALID pack slug → arbitrary write + recursive delete.
- **Admin-key MITM:** autoindex accepts any cert — captured `Authorization: ApiKey …`
  over a self-signed TLS endpoint.
- **Cross-tenant authz:** RO key installs cluster pipelines; RO key reads admin tasks;
  `_resolve` and the metrics token leak foreign index/alias names; share links are
  denial-of-serviceable by id alone.
- The single most severe P claim (wildcard snapshot restore with zero grants, CRITICAL)
  was the run's **only false positive** — the guard fires on all four key shapes tried.

## Threats to validity (read before quoting numbers)

1. **n=2 per arm.** Every per-arm number above is 2 runs. The precision gap (1 FP + 3
   downgrades vs 0) is one method choice away from disappearing; do not present it as an
   arm property. It is reported because it was measured, not because it is settled.
2. **Corpus non-use is a protocol failure, not a product verdict.** Nothing forced arm X to
   query the corpus (the task made it available). A fair test of the *tool* needs tasks where
   retrieval is the natural move — that is #1110 Part A's job.
3. **Both arms ran the same model family with the same base knowledge.** Any corpus edge on
   memorised code was structurally suppressed (see above).
4. **x1 hit the turn cap** (stop reason `error_max_turns`, exit 1) after writing its
   deliverables; it is scored as delivered. Its wall clock (27.4 min) is therefore not
   budget-limited like the other three.
5. **Launcher transport:** P1's first two attempts died to an argv-propagation bug in the
   harness (documented in DEVIATIONS.md); all four *scored* runs used the identical
   stdin launcher, and the discarded attempts are preserved un-scored.
6. **The validator is one engineer (me) and wrote the harness**; PoC-shape misses were
   retried against the code path before any downgrade/refutation, and every verdict carries
   its evidence line in VERDICTS.md.
7. **Validation cost is not billed to either arm** (gate ran after both arms completed).

## Disposition

- **#1111 closes as measured: tie on corpus advantage** (no corpus-assisted discovery in the
  corpus arm), with a real, published method finding (dynamic re-verification cuts FPs and
  overclaims) and a 33-class confirmed-defect inventory handed to the security backlog.
- **#1110 Part A proceeds** (second pack, fresh-machine portability, negative-path tests) —
  the corpus question is re-tested where retrieval should actually pay.
- No public "agents find more vulns with XERJ" claim is supportable from this run; the
  llms.txt / marketing line stays exactly as honest-claims rules require.
