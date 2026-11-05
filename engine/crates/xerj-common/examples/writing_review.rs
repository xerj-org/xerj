//! Local demonstration, not a registered MCP tool or production CLI.
use std::{collections::BTreeSet, env, fs};
use xerj_common::writing_review::{review, ReviewConfig};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2,
        "usage: writing_review <rules.toml> <draft.txt> [known-source-id ...]"
    );
    let config: ReviewConfig = toml::from_str(&fs::read_to_string(&args[0])?)?;
    let sources: BTreeSet<String> = args[2..].iter().cloned().collect();
    let result = review(&fs::read_to_string(&args[1])?, &config, &sources)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if !result.passed {
        std::process::exit(1);
    }
    Ok(())
}
