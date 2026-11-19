# Writing pack (`write.outline` and `write.section`)

`xerj_common::writing_pack` provides the deterministic, schema-validated Rust
primitives for `write.outline` (#2763) and `write.section` (#2764) in the Xerj
Packs milestone (#2769). It performs no network or filesystem I/O: callers pass
verified `PinnedCitation` records from Corpus Hub and receive RFC 8785 (JCS)
packets with SHA-256 receipts.

## `write.outline(topic, audience, length)` (#2763)

`write_outline` (or `WriteOutline::new`) validates each `WriteSourceItem`
against its `PinnedCitation` (`corpus`, 40-hex `commit`, relative `file`,
one-based `line_start..=line_end`, and `byte_start..byte_end`), sorts sources
deterministically by normalized theme tokens and provenance, and partitions
them across chapters:

| `OutlineLength` | Chapters | Minimum verified sources |
| --- | --- | --- |
| `Brief` | 1 | 1 |
| `Standard` | 3 | 3 |
| `Comprehensive` | 5 | 5 |

- **No padding on insufficient evidence**: when fewer verified sources are
  supplied than `length.chapter_count()`, `write_outline` returns
  `WritingPackError::InsufficientEvidence { required, available }` instead of
  repeating sources or fabricating filler chapters.
- **Deterministic chapter structure**: every `OutlineChapter` contains
  `chapter_id`, `title`, `thesis`, `evidence` (attributed bullet summaries with
  `file:line` and `[source:<id>]`), and `source_ids` where every ID resolves in
  `outline.sources`.
- **Content-addressed `outline_id`**: computed from the RFC 8785 canonical JSON
  of the outline payload (`outline-<16-hex>`), paired with `report_sha256`.

## `write.section(outline_id, chapter)` (#2764)

`write_section` verifies the `WriteOutline` receipt, checks `outline_id`, and
resolves `chapter` (by `chapter_id` or chapter `title`):

- **Loud errors**: unknown `outline_id` returns
  `WritingPackError::UnknownOutlineId`; unknown `chapter` returns
  `WritingPackError::UnknownChapter`.
- **Citable facts, quotes, and counterpoints**: populates `facts` and `quotes`
  with `CitedEvidence` items carrying `source_id`, `corpus`, `pin_sha`,
  `file:line`, `byte_start..byte_end`, and `PinnedCitation`. Whenever the
  chapter or outline evidence pool contains a `Stance::Counterpoint` source, at
  least one counterpoint is included in `counterpoints`.
- **Bounded token budget**: measures packet size deterministically with
  `estimated_token_count` (one token per four UTF-8 bytes, rounded up), drops
  lower-ranked supporting items to stay within `token_budget`, and returns
  `WritingPackError::TokenBudgetTooSmall { budget, minimum }` if the mandatory
  core (`thesis`, primary fact/quote, counterpoint, and `style_rules`) exceeds
  `token_budget`.
- **Verification**: both `WriteOutline` and `WriteSectionPacket` provide
  `to_jcs()` and `verify_json()` with `#[serde(deny_unknown_fields)]` and
  `report_sha256` integrity checks.
