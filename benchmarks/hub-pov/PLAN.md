# Corpus Hub v2 — proof-of-value pre-registration

**Status: pre-registration (frozen before any run).** This document fixes the
hypotheses, arms, task classes, metrics, repeats and the publication/stopping
rule for the Corpus Hub v2 proof-of-value measurement — step 10 of
`docs/CORPUS_HUB_V2_IMPLEMENTATION.md` ("## 10. Measurement (before anything is
published)"). Nothing is measured, and nothing is published, until this document
is agreed with the user (constraint 4 below). It gates a real spend and encodes
our honesty commitments, so it is written to be conservative about what will be
measured and strict about what may be shown.

- Plan / discussion: https://github.com/xerj-org/xerj/discussions/1374
- Sandbox every arm runs in: [`sandbox.sh`](./sandbox.sh)
- Registered control suites (reused, not reinvented):
  `benchmarks/corpus-tasks/tasks/<slug>.json` on branch `bench/corpus-tasks`
- Prior informal run this is designed to supersede: see **§8**.

---

## 0. The standing rules this run must obey

These are user directives, not preferences. They are restated here so the run
cannot be reframed after the fact.

1. **Real value through the real flow.** An arm that uses XERJ must use the real
   consumer flow (`xerj "<question>"`, `xerj hub pull`, `xerj code … --def`), not
   grep, not a hand-built slice, not a contrived keyword lookup. The thing being
   tested is the hub giving an agent knowledge it does not already have.
2. **A tool-equipped baseline is mandatory.** The baseline is a *normal agent
   with its tools* — at minimum **normal agent + git** (clone + ripgrep/read)
   **and** **normal agent + web**. A memory-only arm is kept as a floor but is
   **not** a baseline for value. A prior run that reported "positive" off a
   tool-less baseline was rejected for exactly this reason (§8).
3. **Never publish a non-positive result.** The public site shows numbers **only
   if** XERJ v2 beats **both** normal-agent arms (git **and** web) on **cost**
   **and** **output tokens** at **equal-or-better accuracy** (§7). Any other
   outcome — tie, loss, or mixed — goes to discussion #1374, not the website.
   We do not re-roll a test until it passes and present it as typical.
4. **Pre-registration means fixed, then shown.** Hypotheses, arms, task classes,
   metrics, repeats and the decision rule are frozen in *this file* before the
   first run, and the file is shown to the user before anything executes. Changes
   after freeze are logged as dated amendments in §10 **before** the affected
   cell runs — never silently.

---

## 1. What is under test (so the task classes make sense)

From `docs/CORPUS_HUB_V2_IMPLEMENTATION.md` §0 and §5:

- **Corpus Hub v2** answers `xerj "<question>"` from a hub corpus in **one turn,
  with no clone and no indexing on the user's machine.** Packs are pre-indexed by
  CI, signed, and pulled on demand. The corpus is never required in the question
  and is always named in the answer.
- The router (§5) picks the corpus from the question + the project's dependency
  files; `ask` answers locally if the pack is cached, otherwise answers through a
  hosted node and pulls the pack in the background (first use), so the *second*
  question onward is a single local tool call (steady state).
- There are therefore **two distinct cost regimes**, and they are the crux of
  this run:
  - **first use** — cold machine: a hosted answer and/or a one-time signed pack
    pull (bytes downloaded, pull time) before or around the first answer;
  - **steady state** — pack cached: one local tool call per question, no clone,
    no re-index, no per-question download.

The v1 path (`xerj corpus add --from <manifest>` → `xerj corpus index` →
`xerj code`) pays a full clone+index **per machine per corpus, every session**,
and nothing amortises it (§8). v2's claim is that the pre-indexed pulled pack
removes that cost. This run measures whether that claim holds **against a normal
tool-equipped agent**, not just against memory.

---

## 2. Hypotheses (what "wins" means, quantified)

The prior run (§8) already established the **null** we expect to reproduce on
easy content, so we state it first and do not treat reproducing it as a win.

- **H0 (expected null, not publishable).** On the **registered exact-lookup
  control** — single constant values in a cloneable public repo — a normal agent
  with git (`git clone --depth 1` + ripgrep) matches v2 accuracy at a fraction of
  the tokens and cost. We pre-register a **tie-or-loss** for v2 here as the
  *expected* outcome. Exact-symbol lookup in a public repo is grep's best case;
  publishing a win there would be dishonest even if one appeared.

- **H1 (the claim this run tests).** v2 **steady state** changes the result on
  the task classes that are **not** grep's best case:
  - *(a) how-it-works cross-file questions* — a git agent must open and read many
    files to answer; v2 returns a ranked passage set in one call;
  - *(b) non-git multi-source data* — "clone + grep" does not apply, so the git
    arm degrades toward the web arm;
  - *(c) 10-question sessions* — v2's one-time pull amortises across 10 answers.

  **Quantified prediction (H1):** on classes (a)–(c), v2 steady-state mean
  **output tokens ≤ 50%** and mean **cost ≤ 70%** of **both** the git arm and the
  web arm, at accuracy **≥** both. (These are the predicted magnitudes; the
  *publication bar* in §7 is the separate, binding gate.)

- **H2 (routing is not free).** v2 must route the bare question to the correct
  corpus. We pre-register a **routing-accuracy floor of 90%** on the control
  questions (which name a symbol that maps cleanly to one corpus) and **80%** on
  the how-it-works questions; a cell whose question mis-routes is counted as an
  *incorrect v2 answer*, not silently re-asked with `--corpus`. Routing accuracy
  is reported per class.

"Wins" for publication is **not** H1 — it is the stricter rule in §7. H1 is our
honest prediction; §7 is the contract for what may go on the website.

---

## 3. Arms

Nine arms. **All arms share one agent harness and differ only in the tools on
PATH and the prompt's tool instructions** — same `claude -p` CLI, same model
(pinned at freeze, §10), same `--max-turns`, same scratch cwd with no project
`CLAUDE.md`/memory, same questions, same corpora/pins. Each arm runs inside
[`sandbox.sh`](./sandbox.sh). ≥3 repeats per cell (§6).

| # | Arm | Role | Network | Exactly what it may use |
|---|---|---|---|---|
| **A0** | memory only | floor (not a value baseline) | offline | No tools. One reply from parameters. Prompt states the source is unavailable and to answer `unknown` rather than fabricate (PROTOCOL.md §4 bare prompt). `--max-turns 1`. |
| **A1** | normal agent **+ git** | **primary baseline** | on | Bash + network; `git clone --depth 1 <public url>`, `git fetch`, `ripgrep`/`grep`, file read, coreutils. **No XERJ on PATH, not mentioned.** |
| **A2** | normal agent **+ web only** | **primary baseline** | on | Bash + network **but may not `git clone` the corpus source**; `curl` / web fetch / web search allowed. A fetch returning a bot-protection page, captcha, 403/429, or empty body is recorded as a **failed fetch** (counted; the agent proceeds without it). |
| **A3** | hosted regex code search | reference | on | One hosted regex/code-search endpoint over public code (no clone, no local index), via `curl`. Exact endpoint locked at freeze (§10). Requests and failed fetches counted. |
| **A4** | web-search API | reference | on | One general web-search API, via `curl`; agent reads returned snippets/pages (failed fetches counted). Exact API locked at freeze (§10). |
| **A5** | XERJ **today** (`corpus add` path) | v1 incumbent | on | Full v1 flow on a clean machine: install `xerj`, start a node **in the background** (foreground boot dies exit 144 — never foreground), `xerj corpus add --from <manifest>`, `xerj corpus index`, `xerj code <slug> "<q>"`. This is the prior "hub" arm (§8). |
| **A6** | XERJ **v2 first use** | v2 cold | on | `xerj "<question>"` on a **cold** machine (empty `HOME`, no pack cached). Router + §5 answer flow only: hosted answer and/or one signed pack pull. **No hand-run `corpus add`/`corpus index`.** Pack bytes + pull time recorded; answer must name the corpus. |
| **A7** | XERJ **v2 steady state** | **v2 under test** | on* | `xerj "<question>"` with the pack **already pulled**: one local tool call per question. `--offline` set where possible to prove no per-question network. *Network is present but the answer must not require it. |

Notes:

- A1 and A2 are the two arms the §7 gate is measured against ("both normal-agent
  arms"). A3/A4 are context, not the gate.
- A5 vs A7 is the v1→v2 comparison (does pre-indexed-pull remove v1's per-session
  setup cost). A6 vs A7 is first-use vs steady-state.
- A0 reproduces the prior floor; it is reported but never used to claim value.
- The XERJ arms invoke `xerj` through a logging PATH shim that counts invocations
  (as PROTOCOL.md §4 does), so a passing v2 cell with **0** corpus calls is
  reported as a model recall, not a hub win.

---

## 4. Task classes

Four classes, from step 10. For each class the **same question set** is put to
every arm.

### C1 — registered exact-lookup (control; tie with git expected)

- **Reuse the existing registered suites — do not invent new tasks for the
  control.** Source: `benchmarks/corpus-tasks/tasks/<slug>.json` on branch
  `bench/corpus-tasks` (constant-value `state`/`drift` tasks, pin-anchored).
- Grading is **mechanical**: the suite's `benchmarks/corpus-tasks/harness/
  check.py` runs the task's regex alternates over the final `ANSWER:` line
  (`hits >= need`). No LLM judge, no partial credit.
- Purpose: the null control. A tie between A1 (git) and A7 (v2) here is the
  pre-registered expected result (H0), published to the discussion as such.

### C2 — how-does-it-work, across files (retrieval's best case)

- Conceptual questions whose answer spans **multiple files** of a pinned corpus
  ("how does X recover after a crash", "what is the ordering guarantee between A
  and B"). These are **new** and must be authored from the pinned source **before
  any run** (see §5), each with a grading rubric and a gold answer.
- Grading is by **LLM judge** (§6) because a regex cannot score prose. The judge
  model is named and its key must be set, else the cell is **unscored** (§6) — a
  known prior gap we are closing, not papering over.

### C3 — non-git multi-source data (clone+grep does not apply)

- A hub corpus whose content is **not one `git clone` away** (multi-source
  intake, non-repo data). This is the class the prior run explicitly lacked.
  Questions are authored from the pinned pack content; judged like C2.
- Purpose: the git arm (A1) degrades toward the web arm (A2) here; this is where
  H1 predicts the clearest separation.

### C4 — 10-question sessions per corpus (amortisation)

- Ten questions of one corpus asked **in a single agent session**, so each arm's
  one-time setup (A5's clone+index; A6's pull; A2/A4's first fetches) is spread
  across ten answers. Mix is fixed at freeze: e.g. 4×C1-style + 4×C2-style +
  2×C3-style per corpus.
- Purpose: the regime where v2 steady state can legitimately win if it wins at
  all; also where A5's per-session setup is most penalised.

**Corpora (pinned at freeze, §10).** C1/C2 draw from the `measured` registered
code corpora (e.g. `lmdb`, `quinn`, `leveldb`, `boringtun`, `badger`, `s2n-tls`,
`lz4`); C3 draws from a hub corpus with multi-source intake. The exact per-class
corpus+pin list is locked in §10 with the user before the first run; pins are the
hub pack pins so every arm is graded against the same ground truth.

---

## 5. Authoring the non-mechanical tasks (C2, C3)

Pre-registration requires these be fixed before runs, from the pinned source,
with no knowledge of how any arm will answer:

1. Written against the **pinned** corpus content only (the pack pin for XERJ
   arms; the same commit for git/web arms), by a person, not generated from an
   arm's output.
2. Each task carries: `q`, a `gold` answer, a `rubric` (the specific facts a
   correct answer must contain), and the `files`/locators the gold was drawn from
   (for audit).
3. Committed to `benchmarks/hub-pov/tasks/<class>/<slug>.json` **before** the
   first C2/C3 run. The commit that freezes them is referenced in §10.
4. The judge (C2/C3) grades answer-vs-rubric, never answer-vs-arm; the gold and
   rubric are the authority.

---

## 6. Metrics, repeats, judging

**Metrics per run** (step 10 list, verbatim coverage):

| Metric | Source |
|---|---|
| correct (pass/fail) | `check.py` for C1; LLM judge for C2/C3 |
| cost ($) | transcript `total_cost_usd`, summed over all `result` events in the session |
| output tokens | transcript `usage.output_tokens`, summed over all `result` events |
| turns | transcript `num_turns` |
| bytes downloaded | counted in-sandbox (pack pull for A6; clone bytes for A1/A5; fetch bytes for A2/A3/A4) |
| wall time | measured on a **quiet machine**; the 1/5/15-min load average is recorded next to every timing (a machine that slept or was loaded invalidates the timing — prior lesson, §8) |
| failed fetches | A2/A3/A4: a bot-protection page / captcha / 403 / 429 / empty body (step 10) |
| routing accuracy | A6/A7: did the router pick the correct corpus (Confident, or Ambiguous containing the correct slug); a mis-route counts the answer wrong (H2) |

**Summing lesson (§8):** a session with a background task emits **several**
`result` events; cost and output tokens are the **sum** over all of them, not the
last one. The harness asserts it summed ≥1 event per run.

**Repeats:** **≥3 per cell** (arm × class × corpus). A cell whose metric of
record (output tokens or cost) has across-repeat coefficient of variation > 0.5
gets **+2** repeats before it is used in the §7 comparison.

**Judging (C2/C3):**

- Judge model: **`claude-opus-4-8`**, temperature 0, given only `q`, the arm's
  answer, the `gold`, and the `rubric`; it returns pass/fail + which rubric points
  were met. The judge is a **separate inference** from every arm (never an arm
  judging itself).
- **The judge's API key (`ANTHROPIC_API_KEY`) must be set in the harness env.**
  If it is unset or the judge call errors, the cell is recorded `unscored` — not
  pass, not fail — and an unscored cell **cannot** satisfy the §7 gate. This is
  the explicit closure of a prior run's silent judge-key gap.
- C1 needs no judge (mechanical); its cells are never `unscored` for key reasons.

---

## 7. Publication / stopping rule (the binding decision)

This is the pre-registered decision. It is checked **once**, after the fixed cell
matrix (§6 repeats) is complete. We do not peek-and-stop, and we do not re-roll.

> **Publish to the website iff** — for **at least one** task class in {C2, C3,
> C4} — XERJ v2 **steady state (A7)** beats **both** the normal-agent-with-git
> arm (A1) **and** the normal-agent-with-web arm (A2) on **both** mean **cost**
> **and** mean **output tokens**, at accuracy (mean correct) **≥** both, where
> **every** winning margin exceeds the larger across-repeat standard deviation of
> the two compared cells (≥3 repeats).
>
> **And, if published, the first-use regime is disclosed alongside, never
> hidden:** A6's one-time pack bytes, pull time and cost, plus the **per-corpus
> amortisation crossover** — how many questions into a session v2's cumulative
> cost drops below the baseline's. A steady-state headline without its first-use
> cost is not a permitted claim.
>
> **The control class C1 is excluded from the gate** (a win there is grep's best
> case and would be dishonest — H0). It is reported for context only.

**Everything else → discussion #1374, not the website.** A tie, a loss, a
mixed result (wins tokens but not cost, or beats git but not web, or wins only on
C1), or any `unscored` cell in the deciding comparison: the numbers go to the
thread with their conditions stated, and nothing goes to the site. Win or lose,
the full matrix is posted to #1374 (step 10: "Results go to the discussion, win
or not").

**Stopping:** the run stops when the fixed matrix is complete. We do not add
corpora, arms or repeats to move a near-miss across the line; such a change would
be a new pre-registration, dated in §10, agreed with the user, and run fresh.

---

## 8. Prior run (why this run is designed the way it is)

An informal run on **2026-10-13** (5 hub corpora: s2n-tls, lmdb, badger, lz4,
quinn; 50 registered constant-value questions; model claude-opus-5-5) produced:

| arm | correct | all tokens | cost |
|---|---|---|---|
| memory (tool-less, one reply) | 21/50 | 31,715 | $0.30 |
| **tools, no hub** (shell + network: `git clone --depth 1` + grep) | **49/50** | **556,254** | **$0.91** |
| hub (v1: read llms.txt → install → node → `corpus add` → `corpus index` → `xerj code`) | 50/50 | 1,618,155 | $1.66 |

**The lesson it taught.** The hub beat the **tool-less memory** arm (50 vs 21)
but **not a normal tool-equipped agent**: the git arm matched its accuracy at
**~1/3 the tokens and ~half the cost**. Reporting "positive" off the memory
baseline was the mistake that got the result rejected; the user asked "are you
sure the agent with no hub was able to retry?" — it had not been in that framing.
The hub's extra cost was its **per-session, per-machine setup** (~10 steps, each
turn re-reading ~28k tokens of context) against the git arm's ~4 (clone, grep),
with nothing amortising the setup. Its one "miss" was a **stale pin** (lz4
`LZ4_VERSION_RELEASE`, upstream moved 0→1 after the 2026-10-02 pin).

**What this run changes to find whether v2 is different:**

- it measures **v2's pre-indexed, cached, one-tool-call path** (A6/A7), not v1's
  clone+index path (A5) — the per-session setup v1 paid is exactly what v2 claims
  to remove;
- it adds the **two normal-agent arms as the gate** (A1 git, A2 web), per the
  standing rule — never memory as the baseline;
- it tests **classes that are not grep's best case** (C2 cross-file, C3 non-git
  multi-source, C4 amortised sessions), because exact-symbol lookup in a cloneable
  repo (C1) is where the prior run correctly tied;
- it records **load next to every timing** (prior timings were inflated by a
  sleeping/loaded machine) and **sums all `result` events** per session.

Artifacts of the prior run (not committed): `/home/claude/pov-run/`.

---

## 9. Sandbox and operational safety

Every arm — every `claude -p` invocation — runs through
[`sandbox.sh`](./sandbox.sh): bubblewrap, **own PID namespace**
(`--unshare-pid`), **read-only filesystem outside the arm's HOME**
(`--ro-bind / /` + a fresh writable HOME), `--die-with-parent`, `proc`/`dev`,
network allowed. The PID namespace is the load-bearing protection: a prior
**unsandboxed** test agent cleaning up its node ran `pkill -x xerj` and stopped
the user's long-running reference-index node (ports 9297–9299). Inside the
sandbox, `pgrep`/`pkill` see only the sandbox's own processes.

Operational rules for the run (not in step 10; added from that incident and the
quiet-machine requirement):

- Run arms **one at a time** — the XERJ arms bind host port **:9200**, so
  concurrent arms collide; serialise them and record load.
- **Never touch the reference node** (`xerj-refindex`, :9297–9299) or its data.
- Quiet machine; 1/5/15 load average recorded next to every timing.

---

## 10. Freeze checklist (lock with the user before the first run)

Pre-registration is complete only when these are filled in and the user has
agreed. Each is a value that must not change after its first dependent run;
post-freeze changes are logged as dated amendments **in this section** before the
affected cell runs.

- [ ] **Agent model** pinned (prior run used `claude-opus-5-5`): `__________`
- [ ] **Judge model** = `claude-opus-4-8`, temp 0, `ANTHROPIC_API_KEY` present.
- [ ] **C1 corpora+pins** (registered suites): `__________`
- [ ] **C2 corpora+pins** and authored tasks committed at: `__________`
- [ ] **C3 corpus+pin** (multi-source, non-git) and tasks committed at: `_____`
- [ ] **C4 question mix** per corpus fixed: `__________`
- [ ] **A3 endpoint** (hosted regex code search): `__________`
- [ ] **A4 endpoint** (web-search API): `__________`
- [ ] **Repeats** = 3 (floor), +2 on CV > 0.5.
- [ ] Cost envelope estimated and approved (arms × classes × corpora × repeats).
- [ ] User has seen this document and approved the run (constraint 4).

**Amendments log:** _(none yet — append dated entries here before the affected
run; e.g. "2026-10-xx: A4 endpoint set to … — agreed with user".)_
