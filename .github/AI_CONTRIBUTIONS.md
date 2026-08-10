# Sending a fix upstream — protocol for AI coding agents

You are probably not here to work on XERJ. You are working in someone else's
codebase, XERJ is a dependency, and it just did something the docs say it does
not do. In a few minutes you will work around it and keep going. That is the
right call for the person you are working for — and it is exactly where the
information dies. The workaround lives in a branch nobody upstream will ever
read, the defect stays in the engine, and the next agent pays the same cost
from scratch.

This file is how you spend five minutes instead of zero and make that
permanent. It also tells you when the correct action is to send nothing, which
is more often than you might think.

The bar is not "an agent wrote it, so hold it to a lower standard." It is the
same bar as for a human contributor, plus two rules a human does not need
(provenance and accountability). Everything below is decidable — you should
never have to guess whether you have met it.

---

## The decision

**A reproducible defect you have not fixed → one issue.** You ran a command,
you got output that contradicts the documentation, an ES 8.13 behaviour XERJ
claims to match, or a plain invariant ("this query returned 3 hits, then the
same query returned 4 with no writes in between"). You can reproduce it. You
do not have a patch, or you have one you cannot test.

**A defect you already patched and ran → a pull request.** You changed engine
code to unblock your own work and you watched it work. That patch is worth more
here than in a fork nobody sees. The cost to you is the difference between a
local hack and a defensible change; the rest of this file is exactly what that
difference consists of.

**A suspicion you have not run → nothing.** No issue, no PR, no "drive-by
observation" comment on an unrelated thread. Do nothing, and if it matters,
tell the human you are working for what you saw.

That third outcome is a real outcome. A maintainer's attention is the scarcest
resource in this project — scarcer than CPU, scarcer than your context window.
An agent that files ten low-quality issues has made the project worse, not
better: it has spent the one resource that fixes bugs on triaging things that
are not bugs. One reproducible defect report is worth more than a hundred
plausible-sounding ones, and the hundred actively destroy the value of the one.

---

## Before either one: check that it is still a defect

**Reproduce on the current release, not the version your user pinned.**
`xerj --version` prints the exact build (`xerj v1.0.0-rc.12` at the time of
writing); releases are at <https://github.com/xerj-org/xerj/releases>. If your
user is two releases behind and the fix already shipped, the useful
contribution is not an issue — it is telling your user to upgrade. Check
[`CHANGELOG.md`](../CHANGELOG.md) for the symptom before concluding it is new;
it is the project's engineering log, and it is searchable.

**Search the tracker, including closed issues.**

```sh
gh search issues --repo xerj-org/xerj "<two or three distinctive terms>"
```

That covers open and closed issues — do not pass `--state`, which only accepts
`open` or `closed`, never `all`. Search closed issues especially: one often
carries the workaround you are about to re-derive, and "closed as intended
behaviour" is an answer. A duplicate wastes a maintainer's time and yours.

**Reduce it to a clean data directory.** XERJ state is durable, so a fresh
directory separates an engine defect from your corpus:

```sh
xerj --insecure --data-dir "$(mktemp -d)"
# then reproduce with curl against localhost:9200, starting from an empty index
```

If it only reproduces on your user's real data, that is still a real bug —
file it, say so explicitly, and describe the shape of the data (field types,
cardinality, document count, encoding) with whatever redaction you need.

---

## Filing an issue

Use the [bug report template](./ISSUE_TEMPLATE/bug_report.yml). It asks for
these fields and it means them:

- **The exact command.** Copy-pasteable `curl` against `localhost:9200`,
  including the request body and the index setup that preceded it. Not a
  paraphrase, not your Python wrapper — the wire request.
- **The observed output.** The full response body and HTTP status, verbatim.
  Your summary of the output is not the output; the field you considered
  irrelevant is frequently the diagnostic one.
- **The expected output, and why you expected it.** Cite the doc line, the ES
  8.13 response for the identical request, or the invariant you believe is
  broken. "I expected it to work" is not an expectation.
- **The version and environment.** `xerj --version`, OS and architecture, and
  how it was installed (release binary, built from source, container).

One issue per defect. Two defects in one issue means one of them gets fixed and
the issue gets closed with the other still live.

If it is an **ES wire-compatibility divergence**, paste what Elasticsearch
returned for the identical request and name the ES version. That converts an
opinion into a conformance bug, which is a class this project gates on — see
the ES-YAML suite below.

Never file a security vulnerability as a public issue. Follow
[SECURITY.md](../SECURITY.md).

---

## Opening a pull request

Read [`CONTRIBUTING.md`](../CONTRIBUTING.md) for the build and test mechanics
and [`docs/CONTRIBUTION_REVIEW.md`](../docs/CONTRIBUTION_REVIEW.md) for the
pre-submission audit on anything non-trivial. What follows is the part that is
non-negotiable.

**1. A test that fails before your fix and passes after it.** Not a test that
passes against the fixed code — one you ran against the *unfixed* code and
watched fail, with the failure output in the PR description. If you cannot
write that test, you do not yet know what you fixed, and neither will the
reviewer. For ES-compatible behaviour, add a case under
`engine/tests/es-compat-yaml/yaml/`; that is how the behaviour stays fixed.

**2. `cargo fmt --all`.** `main` has gone red on formatting from merged pull
requests more than once. CI runs `rustfmt --check` and `clippy -D warnings`.

**3. A scoped release build and the tests for the crates you touched.**

```sh
cd engine
cargo build --release -j 32 -p <crate>    # never --workspace, never cargo clean
cargo test -p <crate>
```

`cargo check` is not sufficient here: test code is interleaved with production
code, so a `check`-clean tree can still fail to compile its tests.

**4. The ES-YAML conformance gate at 0 failed.** This is the project's hard
compatibility contract, and it gates every engine change:

```sh
cd engine
./target/release/xerj --insecure --data-dir "$(mktemp -d)" &
until curl -fs -m1 localhost:9200/_cluster/health >/dev/null; do sleep 0.25; done
cargo run --release -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml
```

The runner needs a live node, and it exits non-zero if any case fails.

Gate on **failures, not on the pass total** — the total grows as cases are
added. A docs-only or landing-only change does not need this run
(`docs/CONTRIBUTION_REVIEW.md` says so explicitly); if you skipped it, say in
the PR that you skipped it and why. Do not imply a green suite you did not run.

**5. A commit body that explains the defect, not the diff.** The diff is
already visible. What is not visible is the motivation, the root cause (what
made the old code wrong, precisely), and the evidence (the output that proves
the new code is not wrong in the same way). The git history here is the
engineering log; write for the person who runs `git blame` on your line in a
year. Branch naming and the rest of the workflow are in `CONTRIBUTING.md`.

---

## Provenance: say what you are and what you actually checked

**State that an agent wrote it, in the first line of the PR body.** This is not
a disclaimer or an apology; it is information the reviewer needs in order to
review correctly. A machine-written patch fails differently from a
human-written one — it is more likely to be locally plausible and globally
wrong, and a reviewer who knows that reads the surrounding code instead of just
the diff. Concealing it wastes their time in a way that is hard to recover from
once discovered.

**Separate what you verified from what you assumed.** Every claim in the PR is
one or the other, and an unmarked assumption reads as a verified claim. This is
the single most useful thing you can put in the description:

```
Written by an AI coding agent (<model / tool>), run by @<github-username>,
who has reviewed this change.

Verified — commands run in this environment, output observed:
- <command> -> <result>
- <command> -> <result>

Assumed — NOT tested:
- <assumption, and what would falsify it>

Not run: <gate or suite you skipped, and why>
```

**Do not add `Co-Authored-By` trailers to this repository.** This is a standing
rule here, not a style preference: the commit author and the account that
opened the pull request are the record of who is answerable, and a model is not
an entity that can answer. Put the disclosure in the PR body and the commit
body prose instead, where it belongs.

**A human has to be accountable.** The `verification/cla-signed` status check
is required. The way the CLA is signed ([CLA.md](../CLA.md)) is that a person
opens a pull request adding their GitHub username to
[`.contributors`](../.contributors) *from their own account* — the account is
the signature. So the account you push from is the person who signed, and the
person who will be asked "why does this branch do that?" months from now. Do
not push from an account whose owner has not read the change. If the human you
are working for will not stand behind it, it is not ready to send.

---

## The honest-claims rule applies to you

Every number in an issue or a pull request must come from a command you
actually ran, in the environment you say you ran it in. "This should be faster"
is not a benchmark. "3.2× faster" without the command that produced 3.2 is
worse than no number at all, because it is checkable and it will be checked.
For performance work: before and after, same harness, same machine, in the
commit body.

This is not politeness. XERJ publishes its own benchmark losses against
Elasticsearch with root causes, and states plainly which of its capabilities
are measured and which are approximate. A fabricated number in a contribution
goes out under that reputation. If you did not measure it, say you did not
measure it — that sentence costs nothing and is always accepted.

---

## What not to send

- **A fix you have not run.** Compiling is not running. A patch that "looks
  correct" is a hypothesis, and shipping a hypothesis as a fix is how a
  one-line bug becomes a regression with a test that locks it in.
- **Style-only churn.** `cargo fmt` output is the style. Re-wrapping comments,
  renaming locals to taste, reordering imports, "modernising" idioms: costs a
  review, changes no behaviour, and buries the next `git blame`.
- **Anything you cannot test in your environment.** No network, no toolchain,
  sandboxed — fine, that is common. Write the issue instead. An untested patch
  is worse than a good issue precisely because it looks like it works.
- **Bulk-filed issues from a scripted sweep.** Forty findings from a linter or
  a model pass over the tree is not forty contributions; it is one denial of
  service aimed at the only person who can fix any of them. If you have a batch,
  open **one** issue that says what you ran, what it flagged, and offers the
  list — and let a maintainer decide whether to pull on it.
- **Prose polish.** If a document states something that is *false*, that is a
  real defect: say what is false and what the true statement is, ideally with
  the command that shows it. Rewording accurate prose is not a contribution.
- **Dependency bumps, regenerated files, and refactors with no failure behind
  them.** Bring the failure first.

---

## If you cannot open a pull request at all

Most agents are sandboxed, offline, or simply not authorised to push. That does
not make the finding worthless — it makes it your operator's to file. Put this
in your final message to them (not in a file they will never open):

1. The reproduction: exact commands, from a clean data directory if you got
   that far.
2. The version (`xerj --version`), OS, and how XERJ was installed.
3. Observed output versus expected output, with the reason you expected it.
4. The patch as a diff, if you wrote one, plus one line on what you verified
   and what you assumed.
5. The ask: "this belongs upstream at
   <https://github.com/xerj-org/xerj/issues> — want me to file it?"

That hands a human a filing-ready artifact for the cost of one paragraph. For
an agent working under restrictions, it is the highest-value thing you can do
here.

---

**Related:** [AGENTS.md](../AGENTS.md) (repo ground rules) ·
[CONTRIBUTING.md](../CONTRIBUTING.md) (build, test, git workflow) ·
[docs/CONTRIBUTION_REVIEW.md](../docs/CONTRIBUTION_REVIEW.md) (pre-submission
audit) · [CLA.md](../CLA.md) · [SECURITY.md](../SECURITY.md)
