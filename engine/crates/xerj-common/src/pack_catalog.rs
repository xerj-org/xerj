//! Deterministic catalog metadata for the pack-grouped MCP tool surface.
//!
//! Packs are a documentation and CLI grouping only. The MCP server remains
//! the source of truth for the tools and their schemas; this module never
//! manufactures or omits a tool.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub const PACK_ORDER: [&str; 5] = ["core", "hub", "writing", "bio", "cyber"];
pub const TOKENIZER: &str = "word-runs-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogTool {
    pub name: String,
    pub catalog_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogPack {
    pub name: String,
    pub tool_count: usize,
    pub catalog_tokens: usize,
    pub tools: Vec<CatalogTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogTotal {
    pub tool_count: usize,
    pub catalog_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackCatalog {
    pub tokenizer: &'static str,
    pub total: CatalogTotal,
    pub packs: Vec<CatalogPack>,
}

/// Group an MCP `tools/list` array. A malformed, duplicate, or unnamed spec is
/// an error rather than a catalogue that silently disagrees with the server.
pub fn build(specs: &Value) -> Result<PackCatalog, String> {
    let specs = specs
        .as_array()
        .ok_or_else(|| "tool specifications must be a JSON array".to_string())?;
    let mut names = BTreeSet::new();
    let mut grouped: Vec<Vec<CatalogTool>> = PACK_ORDER.iter().map(|_| Vec::new()).collect();
    let mut total_tokens = 0;

    for spec in specs {
        let name = spec
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "every tool specification needs a string `name`".to_string())?;
        if !names.insert(name.to_string()) {
            return Err(format!("duplicate tool specification `{name}`"));
        }
        let compact = serde_json::to_string(spec)
            .map_err(|e| format!("cannot serialize tool `{name}`: {e}"))?;
        let catalog_tokens = count_word_runs(&compact);
        total_tokens += catalog_tokens;
        let pack = PACK_ORDER
            .iter()
            .position(|candidate| *candidate == pack_for_tool(name))
            .expect("pack_for_tool returns an entry in PACK_ORDER");
        grouped[pack].push(CatalogTool {
            name: name.to_string(),
            catalog_tokens,
        });
    }

    let packs = PACK_ORDER
        .iter()
        .zip(grouped)
        .map(|(name, tools)| CatalogPack {
            name: (*name).to_string(),
            tool_count: tools.len(),
            catalog_tokens: tools.iter().map(|tool| tool.catalog_tokens).sum(),
            tools,
        })
        .collect::<Vec<_>>();
    Ok(PackCatalog {
        tokenizer: TOKENIZER,
        total: CatalogTotal {
            tool_count: specs.len(),
            catalog_tokens: total_tokens,
        },
        packs,
    })
}

pub fn pack_for_tool(name: &str) -> &'static str {
    if name.starts_with("xerj_hub_") || name.starts_with("hub.") || name.starts_with("hub_") {
        "hub"
    } else if name.starts_with("xerj_write_")
        || name.starts_with("xerj_writing_")
        || name.starts_with("write.")
        || name.starts_with("write_")
    {
        "writing"
    } else if name.starts_with("xerj_bio_") || name.starts_with("bio.") || name.starts_with("bio_")
    {
        "bio"
    } else if name.starts_with("xerj_cyber_")
        || name.starts_with("cyber.")
        || name.starts_with("cyber_")
    {
        "cyber"
    } else {
        "core"
    }
}

/// Render the stable human-readable form. Tool names are already in
/// `tools/list` order because each pack filters that source array.
pub fn format_table(catalog: &PackCatalog) -> String {
    let mut out = String::from(
        "PACK     TOOLS  CATALOG TOKENS  TOOLS IN MCP ORDER\n\
         -------  -----  --------------  ------------------\n",
    );
    for pack in &catalog.packs {
        let names = pack
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "{:<7}  {:>5}  {:>14}  {}\n",
            pack.name, pack.tool_count, pack.catalog_tokens, names
        ));
    }
    out.push_str(&format!(
        "TOTAL    {:>5}  {:>14}\n",
        catalog.total.tool_count, catalog.total.catalog_tokens
    ));
    out.push_str("\nCatalog tokens use word-runs-v1 over each complete, compact JSON tool spec.\n");
    out
}

fn count_word_runs(text: &str) -> usize {
    let mut runs = 0;
    let mut in_run = false;
    for character in text.chars() {
        let current = character.is_alphanumeric() || character == '_';
        if current && !in_run {
            runs += 1;
        }
        in_run = current;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn groups_every_pack_in_fixed_order_and_counts_deterministically() {
        let specs = json!([
            {"name":"xerj_search","description":"local search","inputSchema":{"type":"object"}},
            {"name":"xerj_hub_ask","description":"hub","inputSchema":{}},
            {"name":"xerj_write_outline","description":"write","inputSchema":{}},
            {"name":"xerj_bio_evidence","description":"bio","inputSchema":{}},
            {"name":"xerj_cyber_review","description":"cyber","inputSchema":{}}
        ]);
        let first = build(&specs).unwrap();
        let second = build(&specs).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .packs
                .iter()
                .map(|pack| pack.name.as_str())
                .collect::<Vec<_>>(),
            PACK_ORDER
        );
        assert!(first
            .packs
            .iter()
            .all(|pack| pack.tool_count == 1 && pack.catalog_tokens > 0));
        assert_eq!(first.total.tool_count, 5);
        assert_eq!(
            first.total.catalog_tokens,
            first
                .packs
                .iter()
                .map(|pack| pack.catalog_tokens)
                .sum::<usize>()
        );
    }

    #[test]
    fn refuses_duplicate_or_unnamed_tools() {
        assert!(build(&json!([{"inputSchema":{}}])).is_err());
        assert!(build(&json!([{"name":"same"},{"name":"same"}])).is_err());
    }

    #[test]
    fn table_keeps_empty_packs_visible() {
        let rendered = format_table(&build(&json!([{"name":"xerj_search"}])).unwrap());
        assert!(rendered.contains("core"));
        assert!(rendered.contains("hub"));
        assert!(rendered.contains("writing"));
        assert!(rendered.contains("bio"));
        assert!(rendered.contains("cyber"));
        assert!(rendered.contains("TOTAL"));
    }
}
