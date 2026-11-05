# Writing review primitive

`xerj_common::writing_review::review(draft, config, source_ids)` is a pure,
model-free plain-text checker toward #2765. It performs no file or network I/O.
It is not yet `write.review` over MCP: #2762's recipe runner must define that
integration. The configuration below is a standalone check policy, not a claim
to implement the future recipe schema.

Every rule has a unique nonempty `id` and a nested `check` with a `kind`.
The complete, tested example policy is
[`rules.toml`](../engine/crates/xerj-common/tests/fixtures/writing_review/rules.toml).
There are no hard-coded banned phrases, hedge vocabulary or style thresholds.
Missing policy fields and unknown fields/kinds are deserialization errors;
zero word/occurrence limits and empty or duplicate normalized phrases are
validation errors, even for an empty draft. An empty rule list disables checks.
Empty phrase lists disable phrase matching; a zero hedging limit prohibits
any configured hedge match.

| Kind | Configuration | Meaning |
| --- | --- | --- |
| `banned_phrases` | `phrases` | Whole normalized word sequences, case-insensitive |
| `sentence_length` | `max_words` | Flag sentences strictly above the word limit |
| `repetition` | `min_words`, `max_occurrences` | Flag normalized identical sentences after the allowed count; shorter sentences excluded |
| `hedging` | `phrases`, `max_per_100_words` | Per-sentence phrase matches per 100 words, using exact integer comparison; overlapping phrases count separately |
| `citations` | `claim_phrases`, `numeric_claims` | Flag configured phrase matches or numeric tokens without a recognized citation in the same sentence; report unknown IDs |

## Text and citation contract

- Sentences terminate at ASCII `.`, `!`, `?`, a blank line, or end of input.
  Soft line breaks become spaces; CRLF works. Abbreviations and decimals are
  not specially parsed. This is an English-oriented heuristic, not linguistic
  sentence segmentation. Markdown/code/URLs are not parsed or excluded: supply
  plain prose. Unicode alphanumerics and ASCII apostrophes form words; other
  punctuation separates words. Lowercasing is Unicode-aware but does not apply
  Unicode normalization or stemming.
- Citations must be `[source:<id>]` **before the sentence-ending punctuation**.
  IDs are case-sensitive exact matches against a caller-supplied trusted set.
  IDs should contain no line breaks or `]`. Well-formed markers are excluded
  from word counts and repetition; their internal punctuation does not split
  sentences. Incomplete markers remain prose. The draft cannot declare its own
  trusted source IDs. Known IDs establish attribution syntax only: this does
  not verify the source content, entailment, provenance or factual correctness.
- Numeric statements and configured claim phrases are only potential claims.
  Other uncited facts can pass; nonfactual numeric prose can be flagged. A
  citation on a different sentence does not satisfy the check.
- Notes contain a one-based **sentence-start** line, rule ID, and message.
  They are sorted by `(line, rule_id, message)` and identical notes are collapsed.
  `passed` means no configured check produced a note, not that the prose is true
  or publishable. Identical draft, policy and source IDs produce identical JSON;
  rule order does not affect output. Invalid configuration returns an error,
  never a successful review.

## Local demonstration

From `engine/`, this reads only the specified local files and prints JSON.
The good fixture exits 0. Substitute `bad.txt` to get five notes and exit 1.
A malformed policy returns an error without a review result. Nothing is
installed, published, or served. Run only with files you intend to review;
stdout includes configured phrase and source-ID text.

```sh
cargo run --locked -p xerj-common --example writing_review -- \
  crates/xerj-common/tests/fixtures/writing_review/rules.toml \
  crates/xerj-common/tests/fixtures/writing_review/good.txt study.1
```

Tests: `cargo test --locked -p xerj-common --test writing_review`.
The golden good/bad drafts, exact JSON notes, configuration refusal cases,
threshold boundaries, Unicode/CRLF, soft wraps, repetition and citation locality
are repository-visible fixtures/tests. No latency or accuracy benchmark is claimed.

Design reference: Vale's MIT-licensed `internal/check/existence.go`, lines 13–24,
47–59 and 69–79, separates configured tokens from checks and emits located alerts
([source](https://github.com/vale-cli/vale/blob/cafff9f93536168527650c4b3637b43e8d8c8a9e/internal/check/existence.go),
[license](https://github.com/vale-cli/vale/blob/cafff9f93536168527650c4b3637b43e8d8c8a9e/LICENSE)). This informed the
policy/mechanism split; no Vale implementation was copied. This primitive does
not use Vale's regex engine or claim feature parity.
