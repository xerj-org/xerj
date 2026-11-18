//! Local `xerj def` adapter. The CLI owns the query and result ordering;
//! this module validates MCP arguments and uses the configured node/auth.
use serde_json::{json, Value};
use xerj_autoindex::def::{definition_query, finalize_definition_response};

use crate::{
    engine_request, fetch_mapping, opt_u64, search_index, token_budget, tool_text, Ctx, Method,
};

pub(crate) fn spec() -> Value {
    json!({
        "name": "xerj_def",
        "description": concat!(
            "Find a symbol's definition in local autoindexed data, like `xerj def --json`. ",
            "Lexical ranking, not neural: shares the CLI's mapping-aware query, preferred-kind ",
            "and public-declaration ranking, location deduplication and bounded candidate window. ",
            "Returns ES JSON with declarations in hits.hits[]._source.code and locations in ",
            "ax_path/path plus line/start_line (page/cell for documents); older file-level hits ",
            "may carry fields._passage instead. hits.total counts engine matches before deduplication. ",
            "No match is a successful empty hits list. Unlike xerj_hub_def, this queries the ",
            "configured node, not a Hub pack. Optional max_tokens caps the response; see its ",
            "tokenizer and locator-only rule. ",
            "If this prefix was autoindexed from a folder on this machine and a file there is ",
            "newer than the last run, or an indexed file has been removed, the response adds ",
            "`_xerj.hints` code `index_stale` with the last index time and a reindex command, ",
            "the same check as xerj_search: a definition from that index can be stale or missing."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "symbol": {"type":"string", "minLength":1,
                    "description":"Symbol name or capability phrase, as in `xerj def Cache`. Must not be blank."},
                "prefix": {"type":"string",
                    "description":"Autoindex prefix: project searches project-*. Trimmed, must not be empty. Cannot be combined with a non-empty index. Defaults to ax when neither scope is given."},
                "index": {"type":"string",
                    "description":"Explicit index name or pattern instead of prefix, as in xerj_search. A blank string is treated as absent."},
                "kind": {"type":"string",
                    "description":"Symbol kind, e.g. function, struct or class. Like the CLI, ignored when no index maps kind or any maps it as numeric."},
                "lang": {"type":"string",
                    "description":"Language, e.g. rust or python. Like the CLI, ignored when no index maps language or any maps it as numeric."},
                "k": {"type":"integer", "minimum":1, "maximum":50, "default":5,
                    "description":"Maximum unique definitions to return (1..=50, default 5)."},
                "max_tokens": crate::max_tokens_arg_schema()
            },
            "required":["symbol"]
        }
    })
}

fn optional_text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s)),
        _ => Err(format!("`{key}` must be a non-empty string")),
    }
}

pub(crate) async fn run(ctx: &Ctx, args: &Value) -> Value {
    // Validate all locally knowable inputs before the first mapping request.
    let validated = (|| {
        let symbol = optional_text(args, "symbol")?
            .ok_or("`symbol` is required, for example {\"symbol\":\"Cache\"}")?;
        if args
            .get("index")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err("`index` must be a string".to_string());
        }
        let index = search_index(args)?;
        let kind = optional_text(args, "kind")?;
        let lang = optional_text(args, "lang")?;
        let k = opt_u64(args, "k")?.unwrap_or(5);
        if !(1..=50).contains(&k) {
            return Err("`k` must be an integer from 1 to 50".into());
        }
        let budget = token_budget::opt_max_tokens(args)?;
        Ok((symbol, index, kind, lang, k as usize, budget))
    })();
    let (symbol, index, kind, lang, k, budget) = match validated {
        Ok(args) => args,
        Err(error) => return tool_text(error, true),
    };
    let mapping = fetch_mapping(ctx, &index).await;
    let body = definition_query(symbol, kind, lang, k, mapping.as_ref());
    let result = engine_request(
        ctx,
        Method::Post,
        &format!("/{index}/_search"),
        Some(body),
        None,
        None,
        None,
    )
    .await;
    if result["isError"] == true {
        return result;
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    let mut response: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return tool_text("XERJ returned invalid JSON for definition lookup", true),
    };
    if !response.pointer("/hits/hits").is_some_and(Value::is_array) {
        return tool_text(
            "XERJ returned a definition response without hits.hits",
            true,
        );
    }
    finalize_definition_response(&mut response, symbol, k);
    let text = response.to_string();
    tool_text(
        match budget {
            Some(max_tokens) => token_budget::budget_json_response(&text, max_tokens),
            None => text,
        },
        false,
    )
}
