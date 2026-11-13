# Declarative pack recipe runner (`Xerj Packs` 3/9, #2762, #2769)

`xerj_common::pack_recipe` defines the versioned declarative TOML recipe format
(`format_version = "xerj-recipe/v1"`) and the deterministic Rust executor
(`RecipeRunner`) shared by the Writing, Bio, and Cyber packs.

## Recipe format (`xerj-recipe/v1`)

A pack tool is declared in TOML with no new Rust code:

- `[recipe]`: `format_version`, `pack`, `name` (`<pack>.<tool>`), optional
  `mcp_tool_name`, `description`, `requires_scope`, and optional `static_notice`.
- `[[inputs]]`: typed input parameters (`string`, `integer`, `number`,
  `boolean`, `string_array`) with `required`, `enum_values`, `min_len`, and
  `max_len`. `PackRecipe::to_mcp_tool_spec` projects these directly into an MCP
  `tools/list` descriptor (`name`, `description`, `inputSchema`).
- `[corpora]`: `required` and `optional` corpus slugs, `require_redistribution_licence`
  (refuses `LicenceClass::Excluded`), and `min_hits`.
- `[[filters]]`: deterministic predicates (`equals`, `not_equals`, `contains`,
  `in`, `prefix`) bound either to a literal `value` or dynamically to a validated
  input parameter via `from_input`.
- `[ranking]`: `score_desc`, `recency_desc`, or `study_grade_asc`, `top_k`, and
  optional `cluster_by` / `max_per_cluster` diversification.
- `[output_schema]`: required output properties, types, non-empty constraints,
  prefix patterns, and required per-item object members.
- `[[checks]]`: deterministic post-execution gates (`require_citations`,
  `forbid_phrases`, `max_token_budget`, `require_notice`).

## Execution and verification

`RecipeRunner::execute` validates the recipe and MCP arguments, enforces
`cyber_scope::ScopeGuard` when `requires_scope = true`, checks corpus licences,
runs filtering and ranking, validates the assembled JSON output against
`[output_schema]`, executes every `[[checks]]` gate, and seals the
`RecipeExecutionReport` with an RFC 8785 (JCS) `report_sha256` digest.
