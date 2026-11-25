# Declarative pack recipe runner (`Xerj Packs` 3/9, #2762, #2769)

`xerj_common::pack_recipe` defines the versioned declarative TOML recipe format
(`format_version = "xerj-recipe/v1"`) and the deterministic Rust executor
(`RecipeRunner`) shared by the Writing, Bio, and Cyber packs.

**Status.** The format, the runner and the four built-in recipes are here and
tested, and `xerj mcp --recipe-dir <DIR>` serves the recipes in a directory as
MCP tools ([Serving recipes as MCP tools](#serving-recipes-as-mcp-tools)). A
recipe's `[corpora]` block selects the data it runs on:
`RecipeRunner::execute_from_source` asks a `RecipeCorpusSource` for each
`required` corpus (a missing or unreadable one refuses the call) and each
`optional` one (a missing one is skipped), and nothing from a corpus the recipe
does not name reaches the report ([Where the hits come from](#where-the-hits-come-from)).
`RecipeRunner::execute` still takes hits a caller already holds and applies the
same `[corpora]` rules to them. `xerj mcp` reads pinned `<corpus>.jsonl` files
and signed `.hubpack.tar.zst` packs from a directory; fetching packs from the
hub over the network is the runtime path of #2760 (#3297) and is not here.

## Built-in recipes

`builtin_recipes()` returns the four tools the Writing and Bio packs define as
recipes, with no Rust beyond the runner (#2763, #2764, #2766):

| Recipe | MCP tool | What it does |
| --- | --- | --- |
| `write.outline` | `xerj_write_outline` | Groups cited evidence by theme into 1, 3 or 5 chapters (`length`); refuses when there are fewer sources than chapters |
| `write.section` | `xerj_write_section` | One chapter's cited `facts` and `counterpoints` within `token_budget`, measured on the serialized output |
| `bio.outline` | `xerj_bio_outline` | `bio.evidence` grading, grouped into the chapters the records name, each with its own clinical / non-clinical split and abstention |
| `bio.evidence` | `xerj_bio_evidence` | Study-type grading with a receipt per item, clinical / non-clinical split, contradictory findings, not-medical-advice notice |

`cyber.target_surface` is a **sample** (`[recipe] sample = true`): the Cyber pack
(#2767) is not on main, so the tool does not exist. `sample_recipes()` returns
it, `builtin_recipes()` does not, and its MCP description starts with
`[SAMPLE recipe, not a served tool]`.

## Recipe format (`xerj-recipe/v1`)

A pack tool is declared in TOML with no new Rust code:

- `[recipe]`: `format_version`, `pack`, `name` (`<pack>.<tool>`), optional
  `mcp_tool_name`, `description`, `requires_scope`, `static_notice` and `sample`.
- `[[inputs]]`: typed input parameters (`string`, `integer`, `number`,
  `boolean`, `string_array`) with `required`, `enum_values`, `min_len`, and
  `max_len`. `PackRecipe::to_mcp_tool_spec` projects these directly into an MCP
  `tools/list` descriptor (`name`, `description`, `inputSchema`).
- `[corpora]`: `required` and `optional` corpus slugs, `require_redistribution_licence`
  (refuses `LicenceClass::Excluded`), `min_hits` (candidates that pass the
  filters), and `min_evidence_hits` / `min_evidence_hits_by_input` (below).
- `[[filters]]`: deterministic predicates (`equals`, `not_equals`, `contains`,
  `in`, `prefix`) bound either to a literal `value` or dynamically to a validated
  input parameter via `from_input`.
- `[ranking]`: `score_desc`, `recency_desc`, or `study_grade_asc`, `top_k`, and
  optional `cluster_by` / `max_per_cluster` diversification.
- `[grouping]`, `[grading]`, `[[partitions]]`: chapter grouping, study-type
  grading and output partitions (below).
- `[output_schema]`: required output properties, types, non-empty constraints,
  prefix patterns, and required per-item object members.
- `[[checks]]`: deterministic post-execution gates (`require_citations`,
  `forbid_phrases`, `max_token_budget`, `require_notice`).

## Runner features for the pack recipes

- **`max_token_budget` on the real bytes.** The ceiling applies to the RFC 8785
  (JCS) bytes of the target, divided by four and rounded up. `target_field = "$"`
  is the whole output, which is what a consumer receives. The ceiling is
  `max_tokens`, or `max_tokens_from_input` (an integer input the output echoes).
  `fit_by_dropping = true` drops the lowest-ranked items outside the mandatory
  core, until the output fits; still over with only the core left is a failure.
  The core is the first match of each `[[partitions]]` entry with
  `protect_first = true`, or the top-ranked item when none is declared.
- **`min_evidence_hits` refusal.** `[corpora].min_evidence_hits` and
  `[corpora.min_evidence_hits_by_input]` (an enum input, one row per value)
  set the number of evidence items the output must carry after ranking, the
  per-cluster cap and `top_k`. Fewer is `RecipeError::InsufficientEvidence`,
  never padding. The larger of the two floors applies.
- **`cluster_by` grouping.** `[grouping]` groups the selected items by
  `ranking.cluster_by` (a hit field, or the derived `theme_key`: the first four
  alphanumeric tokens of the excerpt then the title, short tokens and digits
  kept) into `output_field` chapters (`chapter_id`, `title`, `group_keys`,
  `record_ids`), and gives each item a `chapter_id`. Items sharing a key stay in
  one chapter. With a count (`chapters`, or `chapters_from_input`), boundaries
  fall between keys as near an even split as they allow, and only fewer keys
  than chapters splits one (the largest first), so no chapter is empty or
  padded. With no count, each distinct key is a chapter.
- **Study-type grading with a receipt.** `[grading]` maps each item's
  `study_type` (`meta_analysis`, `systematic_review`,
  `randomized_controlled_trial`, `cohort_study`, `case_control`, `case_report`,
  `preclinical`, `in_vitro`, `expert_opinion`; each its own design, no aliases)
  and its flags (`multicenter`, `prospective`, `well_powered`) to an
  `EvidenceTier` (`level_1a` .. `level_5`) through an ordered rule table. Each
  item carries a `grade_rule` receipt (`rule_id`, `study_type`, `tier`,
  `matched_signals`, `metadata`, `rationale`). A study whose `human_clinical` is
  not `"true"` is capped at `level_5` under `non_human_rule_id`, whatever its
  design, and the receipt names the rule it would otherwise have met. Human
  clinical evidence (a human study above `level_5`) fills `clinical_field`;
  everything else fills `non_clinical_field`; with no clinical evidence the
  `abstention` text is written. `study_grade_asc` ranks by the computed tier.
- **`[[partitions]]`**: an output array of the items whose member satisfies a
  predicate (for example `stance = counterpoint`). The items stay in `items`.

## Execution and verification

`RecipeRunner::execute` validates the recipe and MCP arguments, enforces
`cyber_scope::ScopeGuard` when `requires_scope = true`, checks corpus licences,
runs filtering, grading and ranking, checks the evidence floor, assembles and
fits the output, validates it against `[output_schema]`, executes every
`[[checks]]` gate, and seals the `RecipeExecutionReport` with an RFC 8785 (JCS)
`report_sha256` digest. Whoever supplies the hits, `[corpora]` governs them: a
hit from a corpus the recipe does not list under `required` or `optional` is
ignored (it cannot satisfy a required corpus, and its licence is not examined),
and a `required` corpus with no hit among the rest is
`RecipeError::MissingRequiredCorpus`.

### Where the hits come from

`RecipeRunner::execute_from_source(recipe, args, source, guard, origin, now)`
takes no hits. After checking the arguments and the `ScopeGuard` gate it asks
`source` (a `RecipeCorpusSource`) for the corpora the recipe names, `required`
first, then `optional`, and runs `execute` over what they hold:

```rust
pub trait RecipeCorpusSource: Send + Sync {
    fn fetch_corpus_hits(&self, corpus: &str, query: &str, max_hits: usize)
        -> Result<Vec<RecipeCorpusHit>, RecipeError>;
}
```

- `query` is the call's free-text inputs (`PackRecipe::corpus_query`): the
  string and `string_array` inputs that are not enums, in declaration order,
  separated by a space. `max_hits` is `MAX_SOURCE_HITS_PER_CORPUS` (5,000).
- A **required** corpus the source does not hold, or holds with no hit, refuses
  the call with `MissingRequiredCorpus` naming it. One the source holds but
  cannot read (a malformed snapshot, a pack that does not verify) refuses it with
  `CorpusUnreadable`, naming the corpus and why.
- An **optional** corpus the source does not hold is skipped, and the report is
  built from the others. One it holds but cannot read refuses the call the same
  way: a corpus that is present but broken is not "not installed", and a report
  built without evidence it was meant to carry would not say so.
- A hit whose `corpus` is not the corpus that was asked for is dropped. A corpus
  named in both lists is asked for once, as required.

Sources:

| Source | Holds |
| --- | --- |
| `BTreeMap<String, Vec<RecipeCorpusHit>>` | hits in memory (tests, or a caller that already has them) |
| `JsonlCorpusDir` | `<dir>/<corpus>.jsonl`, one `RecipeCorpusHit` per line, returned whole (the recipe's `[[filters]]` select from it); at most 16 MiB |
| `xerj_hub::recipe_source::HubPackDir` | signed `<dir>/<corpus>-<pin8>.hubpack.tar.zst` pack archives, below |
| `ChainedCorpusSource` | several sources tried in order; one that does not hold the corpus passes the question on, any other error stops the chain |

**Hub packs.** `HubPackDir` finds the one `<corpus>-<pin8>.hubpack.tar.zst` in
its directory (two pins of a corpus is an error; it does not choose) and reads
it through `xerj_hub::pack::read_verified_pack_records`, which checks, before
anything is used, the ed25519 signature of the detached `<archive>.sig` against
the hub key of the build (`XERJ_HUB_PUBKEY` overrides it for tests, `none`
disables it), the decompression bound, `format_version`, and every `SHA256SUMS`
line. An archive with no `.sig`, a build with no hub key, a pack whose
`pack.json` names another slug, and an archive named for a pin its `pack.json`
does not carry are all refused. Nothing is extracted. Each `defs.jsonl` line is a
hit (`record_id` `<file>:<line>:<symbol>`, `title` the symbol, `excerpt` the
declaration, `fields` `kind`, `lang`, `file`, `line`, optional `value`,
`source_member = "defs.jsonl"`) and so is each `files.jsonl` line (`record_id`
the path, `fields` `kind = "file"`, `lang`, `bytes`, `licence`,
`source_member = "files.jsonl"`); all carry the pack's `pin_sha`, `pin_date`
(`published_date`), `built_at` (`fetched_at`) and licence class. Records are
ranked by the query: the score is the share of its words (letters and digits,
two or more) found in the record, records sharing none are left out, and the
best `max_hits` come back. An archive over 50 MiB, or one that decompresses past
256 MiB, is refused. The archive is verified again at each call.

## Serving recipes as MCP tools

`xerj mcp --recipe-dir <DIR>` (or `XERJ_RECIPE_DIR=<DIR>`; the flag wins) turns
the recipes in `DIR` into MCP tools. A recipe file is the whole tool definition:
dropping one in is a new tool, with no Rust change.

```text
DIR/
  advisories_by_cwe.toml        one recipe, one tool: xerj_advisories_by_cwe
  corpora/
    demo-advisories.jsonl       pinned hits for [corpora] required = ["demo-advisories"]
    other-corpus-0774d055.hubpack.tar.zst       a signed hub pack, and its
    other-corpus-0774d055.hubpack.tar.zst.sig   detached signature
```

- **Off by default.** Without the flag, `tools/list` is the built-in tool list
  and the published schema is unchanged.
- **Registered at startup.** Each `*.toml` in `DIR` (not recursive, in file-name
  order) is parsed and validated, and its tool follows the built-in tools in
  `tools/list`, named by `mcp_tool_name` or `xerj_<name>` (`.` becomes `_`). An
  invalid recipe stops the server and names the file.
- **Refused when registered, not when called.** A recipe with `sample = true`
  (a sample documents a tool that is not served), one with `requires_scope = true`
  (an MCP argument is not an operator-declared work order, so every call would be
  refused), a tool name that does not start with `xerj_`, one that is already a
  built-in tool or a `xerj_hub_*` name, or one another recipe already uses.
- **`tools/call` runs `RecipeRunner::execute_from_source`.** The arguments are
  validated against `[[inputs]]`, the recipe's `[corpora]` are read from
  `DIR/corpora`, and the result is one text block holding the RFC 8785 (JCS)
  bytes of the sealed `RecipeExecutionReport` (`report_sha256` included),
  `isError: false`. A refusal (bad arguments, a missing or unreadable corpus, an
  excluded licence, too little evidence, a failed check) is an `isError: true`
  block that names the reason.
- **`[corpora]` selects the data.** For each corpus the recipe names, `CorpusDir`
  reads `DIR/corpora/<corpus>.jsonl` (one `RecipeCorpusHit` JSON object per line:
  `corpus`, `licence_class`, `record_id`, `title`, `excerpt`, `score`,
  `published_date`, `pin_sha`, `fetched_at`, and `fields` for what filters and
  grading read), or else the signed `DIR/corpora/<corpus>-<pin8>.hubpack.tar.zst`
  and its `.sig` ([Hub packs](#where-the-hits-come-from)). Other files in the
  directory are never opened. The files are read at each call, so an edit needs
  no restart. A snapshot line whose `corpus` is not the file's name is an error,
  a corpus with neither form is not installed (so a required one is refused as
  missing and an optional one is skipped), and a snapshot over 16 MiB is
  refused.
- **No node, read-only.** A recipe tool reads only the operator's recipe and
  corpus files and never calls the node, so `xerj mcp --http` serves it with the
  other read-only tools, without `--allow-writes`.

From Rust, `RecipeRegistry::register` adds a `PackRecipe` (for example the ones
`builtin_recipes()` returns), `RecipeRegistry::with_corpus_source` sets where
the corpora come from, `tool_specs_with_recipes(include_hub, &registry)` is the
list a server with that registry answers `tools/list` with, and
`dispatch_recipe_tool_call(recipe, args, &source)` runs one call over a
`RecipeCorpusSource` you hold.

## What a recipe cannot express yet

Named here so the runner, not one pack, gains them:

- An unknown `outline_id` or chapter is reported as insufficient evidence (no
  matching hits), not as a distinct typed error.
- A content-addressed `outline_id` computed from the output; the report's
  `report_sha256` is the receipt.
- Generated thesis and style-rule prose: `write.outline` returns structure and
  `write.section` carries `static_notice`, not per-chapter prose.
