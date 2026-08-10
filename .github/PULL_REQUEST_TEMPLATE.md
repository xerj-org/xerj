<!--
  Thanks for the contribution. Delete any section that genuinely does not apply,
  but do not delete a section because you did not do it — say you did not do it.
  An unrun check that is silently removed is the one that costs a reviewer an
  afternoon.

  AI agents: read .github/AI_CONTRIBUTIONS.md and fill in the Provenance block.
-->

## What this changes, and why

<!-- Motivation and root cause: what made the old behaviour wrong, precisely.
     The diff already shows what you changed; it cannot show why it was wrong. -->

## Evidence

<!-- The output that proves it. For a bug fix: the test failing before and
     passing after. For performance: before/after from the same harness on the
     same machine, with the command. Every number here must come from a command
     you actually ran. -->

```
```

## Checks

- [ ] `cargo fmt --all` (CI runs `rustfmt --check` and `clippy -D warnings`)
- [ ] Scoped release build of the crates touched: `cargo build --release -j 32 -p <crate>`
- [ ] `cargo test -p <crate>` passes (`cargo check` is not sufficient — test code is interleaved with production code)
- [ ] ES-YAML conformance suite at **0 failed** (`cargo run --release -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml`), or: not applicable because this change is docs/landing-only
- [ ] New ES-compatible behaviour has a matching YAML case under `engine/tests/es-compat-yaml/yaml/`
- [ ] Docs updated if user-visible behaviour changed
- [ ] For non-trivial changes: the applicable audit in [`docs/CONTRIBUTION_REVIEW.md`](../docs/CONTRIBUTION_REVIEW.md)

**Not run:** <!-- name any check above you skipped, and why. This is expected and fine; hiding it is not. -->

## Provenance

<!-- Required if an AI coding agent wrote any part of this change. See
     .github/AI_CONTRIBUTIONS.md. Do NOT add Co-Authored-By trailers to this
     repository — the account opening this PR is the record of accountability. -->

- Written by: <!-- human / AI agent (model or tool), run by @username who has reviewed it -->
- **Verified** (commands run, output observed):
- **Assumed** (not tested, and what would falsify it):
