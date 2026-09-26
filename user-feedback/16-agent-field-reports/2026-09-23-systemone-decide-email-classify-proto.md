# Prototyping local email classification via `/_decide` (2026-09-23)

**Agent:** Claude Code (Sonnet 5)  ·  **XERJ:** xerj v1.0.0-rc.76  ·  **Platform:** macOS arm64 (Darwin 27.0.0)

**Pointed at:** a self-generated synthetic corpus, 260 short emails, 4 labels (invoice/newsletter/personal/needs-reply) — not a real mailbox.

**Used it for:** `/_decide` (the local, no-egress k-NN "System One" vote over a labelled-history index) as a classification prototype for discussions/1012.

**Verdict:** `/_decide` was trivial to drive from a throwaway Python script — no client library, one POST per item, sane response shape (label/confidence/abstain/neighbours). It worked exactly as documented in `systemone_api.rs`'s module comment on first try, no surprises, no doc gap found. The one thing worth a maintainer's attention isn't a bug: the confidence score is not a reliable abstain signal on inputs that don't lexically resemble the labelled history — it was wrong *and* confident (0.84–1.0) on 3/8 deliberately hard test items, same miscalibration shape #1012 already flags for the hosted path. Would use `/_decide` again for a quick local classifier; would not ship its confidence as a threshold without tuning against real data first.

**Numbers:** `python3 run.py` against a local `xerj -d ./data --insecure --port 9420` → easy tier 60/60 = 1.000 acc, hard tier 5/8 = 0.625 acc, 2.6ms/item and 1.7ms/item respectively. Full run: `benchmarks/systemone-classify-email/results/run.json`.

**Filed alongside:** [PR #1026](https://github.com/xerj-org/xerj/pull/1026) (the benchmark) and a comment on [discussions/1012](https://github.com/xerj-org/xerj/discussions/1012#discussioncomment-18562551).
