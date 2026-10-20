# #1108 — the wrong-and-confident gate, through the real `_watcher` path

[Issue #1108](https://github.com/xerj-org/xerj/issues/1108) is the standing
tracker for the **unmeasured half** of the rc.80 `_watcher` work (#1062). #1062
closed when its *evaluate-or-501* mechanism shipped (PR #1082), but its own
acceptance gate — **wrong-and-confident <= 5 % at confidence >= 0.8 on the
classify-email boundary tier** — was never run. This directory runs it and
publishes the verdict, pass or fail, per the loses-does-not-ship rule.

## Why a new benchmark and not `systemone-classify-email`

`systemone-classify-email` measures the same vote, but it calls `POST /_decide`
**directly**. #1108 explicitly rejects that: the measurement must go through the
real product path — *percolate prefilter -> `/_decide` -> fire into
`.xerj_alert_fires`* — so it tests what a deployed detection watch actually does
(its prefilter, paging and fire semantics), not a bare vote. This harness never
calls `/_decide` for the gate decision; the fire records the node's own watcher
evaluator writes are the sole input to the number.

It also fixes the other thing that made the old number unusable as a gate:
**n**. The hand-authored "hard" tier is 8 emails — one miss is 12.5 %, so it
cannot resolve a 5 % line. `gen_boundary.py` scales the *same four
boundary-crossing patterns* (see that file's header) to **120 original,
template-generated emails** (30 per label), labelled by construction.

## Method

- **History (the vote corpus):** the 200-email train split of
  `systemone-classify-email/corpus.json`, reused verbatim, indexed into
  `email-history` (`label` keyword, `text` text, default **lexical** embedder —
  no `--embed-mode neural`).
- **Watches:** one `PUT /_watcher/watch/{id}` per label (one-vs-rest). Each is a
  real detection watch: `trigger.schedule.interval 1s`, `input.search` over the
  empty `mail-incoming` index, `condition.xerj_decide` with that label as
  `positive_label`, `text_field "text"`, `p_min 0.5`, `index "email-history"`,
  `k 10`. A detection watch detects **one** positive label; four of them cover
  the four-label tier.
- **Candidates:** the 120 boundary emails, indexed into `mail-incoming`
  **after** the watches are PUT (the watch baselines at the index's max `_seq_no`
  at PUT time, so only later documents are judged). Each carries its truth label
  in `gold`.
- **The number:** read every record in `.xerj_alert_fires`, map `doc_id` to the
  candidate's `gold`, and compute **wrong-and-confident = fires whose true label
  is not the fired positive label, among fires with confidence >= 0.8**. A
  candidate fires at most once — under the watch matching its vote argmax, when
  the raw confidence clears `p_min`.
- **Which confidence is compared at >= 0.8:** the watcher fires on, and records,
  the **raw** `/_decide` confidence as `p` (`p_min` is a raw threshold by
  design — `es_compat.rs`). It also carries `p_cal` **when the node has a fitted
  calibration**. The run config sets `[decisions] calibration = isotonic`; the
  README reports whether `p_cal` was present and, if so, the rate on it too.

## Reproduce

```sh
# built release binary (this run used v1.0.0-rc.94, release profile)
xerj -d /tmp/gate-data --insecure --port 9471 -c gate.toml   # background; wait for green
cd benchmarks/systemone-watcher-gate
python3 gen_boundary.py
XERJ_URL=http://localhost:9471 python3 watcher_gate.py
```

`gate.toml` is the `[decisions]` block above (index `email-history`, k 10,
`calibration = isotonic`).

## Result

Run 2026-10-11, locally built `xerj v1.0.0-rc.94` (release profile), booted with
`--insecure --port 9471 -c gate.toml` on a throwaway data dir. Raw per-fire
output: [`results/run.json`](./results/run.json); full console transcript:
[`results/transcript.txt`](./results/transcript.txt). The 120 fire records were
written by the node's own watcher evaluator into `.xerj_alert_fires`; this
harness read them back and did nothing else.

| | value |
|---|---:|
| candidates evaluated (boundary tier) | 120 |
| fires (argmax cleared `p_min` 0.5) | 120 |
| fired correctly | 94 / 120 = **0.783** |
| calibration on fires | **none** — `p_cal` absent, number is on raw `p` |
| confident fires (raw `p` >= 0.8) | 98 |
| wrong **and** confident | 24 |
| **wrong-and-confident rate** | **24 / 98 = 0.2449** |

**The number, defined once:** wrong-and-confident = fires whose true label is
not the fired positive label, among fires with confidence >= 0.8 = **0.2449**.
The confidence compared at >= 0.8 is the **raw** `/_decide` confidence `p` — the
value the watcher fires on and records. `p_cal` is absent: `[decisions]
calibration = isotonic` was set, but the vote-history index carries no labelled
**outcome** pairs (a `p` served + a truth label, no `source`), so the node
reports `p_cal: null` with its reason (see `systemone_api.rs` `unfit_reason`) and
the fire records carry `calibrated: false`.

Where the mechanism is wrong, from the confusion matrix (`results/run.json`):

| gold | fired correctly | confidently mis-fired as |
|---|---:|---|
| personal | 30 / 30 | — |
| newsletter | 30 / 30 | — |
| needs-reply | 21 / 30 | 9 → newsletter |
| invoice | 13 / 30 | 11 → personal, 6 → needs-reply |

Every one of the 24 wrong-and-confident fires is at raw `p` between **0.871 and
1.000**, 18 of them at exactly **1.000**. The errors are the two boundary
patterns the hand-authored "hard" tier already flagged, now at scale: an invoice
with no invoice vocabulary ("did the transfer land", "wrapping up the offsite
numbers") voted `personal`/`needs-reply`, and a needs-reply buried in a
status-update rundown voted `newsletter`. The vote is a BM25-weighted k-NN over
labelled history; a boundary email that shares no vocabulary with its own label's
training examples is voted into whichever *other* label it most resembles — and
because it clearly resembles that other label, the vote share (the confidence) is
high. This is the discussion-#1012 caveat and the `systemone-classify-email`
hard-tier finding reproduced through the real watcher path.

## Verdict

**FAIL — the gate does not pass, and per the loses-does-not-ship rule the number
is published and the loss named.** Wrong-and-confident is **0.2449** at raw
`p >= 0.8` against the **<= 0.05** bar. The `_watcher` *mechanism* is real and
correct (evaluate-or-501, one real watch shape, fires into `.xerj_alert_fires`
carrying the raw `p` and `p_cal` when fitted — all true), but its default-vote
**quality** on a boundary tier is not good enough to ship a detection watch as a
"confidently correct" default.

**Would a fitted `p_cal` change the verdict? No, and here is why.** The #1063
calibration is a single monotonic (isotonic) map from raw `p` to `p_cal`. It
cannot separate the right fires from the wrong fires when both sit at raw
`p = 1.000`: it maps that one input to one output. 18 of the 24 wrong fires are
at raw `p = 1.000`, alongside many correct fires at `p = 1.000`; any monotonic
map that pushes `p = 1.000` below 0.8 to clear those 18 also pushes the correct
`p = 1.000` fires below 0.8, destroying the detector instead of fixing it.
Per-example overconfidence on out-of-distribution inputs is a property of the
*vote*, not of the confidence scale, so a global calibration — the kind #1063
fits — does not rescue this gate. (Where calibration *is* measured and does
help — pair-level reliability — is #1063's FiQA ECE 0.3109 → 0.0088, a different
quantity: how well a probability matches an empirical frequency *in aggregate*,
not whether a single confident fire is right.)

**What this does and does not close.** It closes #1108's ask — the gate is now
run through the real product path on the release binary and the verdict is
published with raw results. It does **not** make the detection watch a
ship-as-default "confidently correct" feature; the honest operator guidance is
unchanged from `systemone-classify-email`: tune `decisions.min_confidence` and
`p_min` against a held-out set shaped like the real traffic, and do not read the
raw vote confidence as a trustworthy abstain signal on inputs unlike the labelled
history. A better gate would need either an abstain signal that is not the vote
share (e.g. a distance-to-history or disagreement-among-neighbours score) or a
richer-than-lexical representation so boundary emails stop resembling the wrong
label — both are engine work beyond this measurement.

### Notes on faithfulness to #1108's acceptance criteria

- **Release binary, not a dev build:** rc.94, release profile. #1108 names rc.80
  because that is when it was filed; the `_watcher` + `p_cal` mechanism is
  unchanged since rc.80, and rc.94 is the current release line. Stated, not
  hidden.
- **Real product path, not a `/_decide`-direct harness:** satisfied — the fires
  are written by the node's own per-watch evaluator; the harness only reads
  `.xerj_alert_fires`.
- **Ingest-time labels:** the candidates are bulk-indexed carrying their
  **ground-truth** label in `gold` (the labelled-document shape PR #1082
  evaluates), *not* via `xerj autoindex --label`. `--label` attaches the model's
  own *predicted* label, which cannot be the truth a gate scores against — so for
  a gate the truth has to be stored independently. The watch decision itself
  still runs through the full watcher path.
- **Raw outputs in `benchmarks/`:** `results/run.json` + `results/transcript.txt`,
  reproducible from the commands above.
