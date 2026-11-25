//! Pack recipes served as MCP tools (#2762).
//!
//! A TOML recipe (`format_version = "xerj-recipe/v1"`, see
//! `xerj_common::pack_recipe`) is a whole tool definition: its `[[inputs]]` are
//! the tool's `inputSchema`, its `[corpora]`, filters, ranking, grading, output
//! schema and checks are what the tool does. [`RecipeRegistry`] is how a server
//! is told which recipes to serve. It projects each one into `tools/list` and
//! sends the matching `tools/call` to `RecipeRunner::execute`, so a recipe file
//! dropped into a recipe directory becomes a working MCP tool with no Rust
//! change.
//!
//! The registry is empty unless the operator asks for recipes
//! (`xerj mcp --recipe-dir <DIR>` or `XERJ_RECIPE_DIR`). An empty registry adds
//! nothing, so the default `tools/list` is exactly [`crate::tool_specs`] and the
//! published schema (`landing/docs/agents/schemas/mcp-tools.json`) is unchanged.
//!
//! What a recipe tool does and does not do:
//!
//! * It never calls the node. It reads recipe and corpus files from the
//!   operator's directory only, so it is served over `--http` with the other
//!   read-only tools.
//! * The runner does not fetch corpus data; it takes `hits` from its caller.
//!   Here the caller is a [`RecipeHitSource`]. [`CorpusDir`] reads the hits
//!   pinned in `<recipe dir>/corpora/<corpus>.jsonl`, one `RecipeCorpusHit` per
//!   line, for each corpus the recipe's `[corpora]` names. Fetching them live
//!   from hub packs is the runtime path of #2760 and is not here.
//! * The result is one text block holding the RFC 8785 (JCS) bytes of the sealed
//!   `RecipeExecutionReport` (`report_sha256` included), `isError: false`. A
//!   refusal (excluded licence, missing corpus, too little evidence, a failed
//!   check) is an `isError: true` result naming the reason.
//! * A recipe that cannot be served honestly is refused when it is registered,
//!   not when it is called: a `sample`, a recipe that `requires_scope` (an MCP
//!   argument is not an operator-declared work order, so every call would be
//!   refused), or a name that is not `xerj_*` or that is already a tool.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use serde_json::{json, Value};
use xerj_common::cyber_scope::InputOrigin;
use xerj_common::pack_recipe::{PackRecipe, RecipeCorpusHit, RecipeRunner};

use crate::tool_text;

/// Environment variable naming the recipe directory (`--recipe-dir` wins).
pub const RECIPE_DIR_ENV: &str = "XERJ_RECIPE_DIR";

/// Subdirectory of the recipe directory holding `<corpus>.jsonl` snapshots.
pub const CORPORA_SUBDIR: &str = "corpora";

/// Largest recipe file read. A recipe is a page of TOML.
const MAX_RECIPE_BYTES: u64 = 1024 * 1024;

/// Largest corpus snapshot read for one call.
const MAX_CORPUS_BYTES: u64 = 16 * 1024 * 1024;

/// Where a recipe's candidate hits come from when its tool is called. The
/// runner filters, grades and ranks them; a source only supplies them.
pub trait RecipeHitSource: Send + Sync {
    /// Every hit this source holds for the corpora `recipe` names.
    fn hits(&self, recipe: &PackRecipe) -> Result<Vec<RecipeCorpusHit>, String>;
}

/// Hits pinned in files: `<dir>/<corpus>.jsonl`, one `RecipeCorpusHit` JSON
/// object per line. A corpus with no file contributes no hits, so a recipe that
/// requires it is refused by the runner (`missing required corpus`), and an
/// optional one is simply absent.
#[derive(Debug, Clone)]
pub struct CorpusDir {
    dir: PathBuf,
}

impl CorpusDir {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn read_corpus(&self, corpus: &str) -> Result<Vec<RecipeCorpusHit>, String> {
        // A recipe slug may hold `:`, which is a stream separator on NTFS, and
        // a leading `.`; neither names a snapshot file.
        let file_name_ok = corpus
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            && corpus
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !file_name_ok {
            return Err(format!(
                "corpus `{corpus}` cannot be read from a snapshot file: use letters, digits, \
                 `.`, `_` and `-`, starting with a letter or digit"
            ));
        }
        let path = self.dir.join(format!("{corpus}.jsonl"));
        let len = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if len > MAX_CORPUS_BYTES {
            return Err(format!(
                "{} is {len} bytes; the limit is {MAX_CORPUS_BYTES}",
                path.display()
            ));
        }
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut hits = Vec::new();
        for (n, line) in raw
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let hit: RecipeCorpusHit = serde_json::from_str(line)
                .map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?;
            if hit.corpus != corpus {
                return Err(format!(
                    "{}:{}: the hit says corpus `{}`, but this file is `{corpus}`",
                    path.display(),
                    n + 1,
                    hit.corpus
                ));
            }
            hits.push(hit);
        }
        Ok(hits)
    }
}

impl RecipeHitSource for CorpusDir {
    fn hits(&self, recipe: &PackRecipe) -> Result<Vec<RecipeCorpusHit>, String> {
        let mut hits = Vec::new();
        for corpus in recipe
            .corpora
            .required
            .iter()
            .chain(&recipe.corpora.optional)
        {
            hits.extend(self.read_corpus(corpus)?);
        }
        Ok(hits)
    }
}

#[derive(Clone)]
struct Entry {
    tool: String,
    recipe: Arc<PackRecipe>,
}

/// The recipes a server serves as tools, each under its MCP tool name, and where
/// their hits come from. Empty by default.
#[derive(Clone, Default)]
pub struct RecipeRegistry {
    entries: Vec<Entry>,
    source: Option<Arc<dyn RecipeHitSource>>,
}

impl std::fmt::Debug for RecipeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeRegistry")
            .field("tools", &self.tool_names())
            .finish_non_exhaustive()
    }
}

impl RecipeRegistry {
    /// An empty registry: serves no recipe tool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every `*.toml` file in `dir` (not recursive, in file-name order) as
    /// a recipe, and read their hits from `dir/corpora/<corpus>.jsonl`. A file
    /// that is not a valid, servable recipe is an error naming the file: a
    /// server that quietly skipped it would answer without a tool the operator
    /// believes is there.
    pub fn load_dir(dir: &Path) -> anyhow::Result<Self> {
        let listing = std::fs::read_dir(dir)
            .with_context(|| format!("cannot read the recipe directory {}", dir.display()))?;
        let mut files = Vec::new();
        for entry in listing {
            let path = entry
                .with_context(|| format!("cannot list {}", dir.display()))?
                .path();
            if path.extension().is_some_and(|e| e == "toml") && path.is_file() {
                files.push(path);
            }
        }
        files.sort();

        let mut registry = Self::new();
        for path in files {
            let len = std::fs::metadata(&path)
                .with_context(|| format!("cannot read {}", path.display()))?
                .len();
            if len > MAX_RECIPE_BYTES {
                bail!(
                    "recipe file {} is {len} bytes; the limit is {MAX_RECIPE_BYTES}",
                    path.display()
                );
            }
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read recipe file {}", path.display()))?;
            let recipe = PackRecipe::from_toml_str(&raw)
                .with_context(|| format!("recipe file {}", path.display()))?;
            registry
                .register(recipe)
                .with_context(|| format!("recipe file {}", path.display()))?;
        }
        Ok(registry.with_hit_source(Arc::new(CorpusDir::new(dir.join(CORPORA_SUBDIR)))))
    }

    /// Where the hits for every recipe come from.
    pub fn with_hit_source(mut self, source: Arc<dyn RecipeHitSource>) -> Self {
        self.source = Some(source);
        self
    }

    /// Serve `recipe` as a tool. Refused when it is invalid, a `sample`, a
    /// `requires_scope` recipe, or when its tool name is not `xerj_*`, is
    /// already a built-in tool (or a `xerj_hub_*` name), or is already
    /// registered.
    pub fn register(&mut self, recipe: PackRecipe) -> anyhow::Result<()> {
        recipe.validate()?;
        let name = recipe.recipe.name.clone();
        let tool = recipe.mcp_name();
        if recipe.recipe.sample {
            bail!(
                "recipe `{name}` is marked `sample = true`: a sample shows the format for a tool \
                 that is not served, so it is not registered. Remove `sample` when the tool is real"
            );
        }
        if recipe.recipe.requires_scope {
            bail!(
                "recipe `{name}` has `requires_scope = true`: an MCP argument is not an \
                 operator-declared work order, so every call would be refused. Serve it from a \
                 caller that holds a ScopeGuard"
            );
        }
        if !tool.starts_with("xerj_") {
            bail!("recipe `{name}`: the MCP tool name `{tool}` must start with `xerj_`");
        }
        if is_builtin_tool(&tool) {
            bail!("recipe `{name}`: the MCP tool name `{tool}` is already a built-in tool");
        }
        if self.contains(&tool) {
            bail!(
                "recipe `{name}`: the MCP tool name `{tool}` is already served by another recipe"
            );
        }
        self.entries.push(Entry {
            tool,
            recipe: Arc::new(recipe),
        });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `tool` is the MCP name of a registered recipe.
    pub fn contains(&self, tool: &str) -> bool {
        self.entries.iter().any(|e| e.tool == tool)
    }

    /// The MCP tool names, in registration order.
    pub fn tool_names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.tool.as_str()).collect()
    }

    /// The `tools/list` descriptors, in registration order.
    pub fn tool_specs(&self) -> Vec<Value> {
        self.entries
            .iter()
            .map(|e| e.recipe.to_mcp_tool_spec())
            .collect()
    }

    /// Run the recipe tool `tool` over its hits. `None` when no registered
    /// recipe has that name, so the caller can try the built-in tools.
    pub(crate) async fn call(&self, tool: &str, args: &Value) -> Option<Value> {
        let entry = self.entries.iter().find(|e| e.tool == tool)?;
        let recipe = Arc::clone(&entry.recipe);
        let source = self.source.clone();
        let args = args.clone();
        // File reads and the runner are blocking work; keep them off the
        // reactor so one recipe call does not hold up `ping` and `tools/list`.
        let result = tokio::task::spawn_blocking(move || {
            let hits = match &source {
                Some(source) => source.hits(&recipe),
                None => Ok(Vec::new()),
            };
            match hits {
                Ok(hits) => dispatch_recipe_tool_call(&recipe, &args, &hits),
                Err(why) => tool_text(
                    format!(
                        "recipe `{}` could not read its corpora: {why}",
                        recipe.recipe.name
                    ),
                    true,
                ),
            }
        })
        .await
        .unwrap_or_else(|e| tool_text(format!("the recipe task failed: {e}"), true));
        Some(result)
    }
}

/// A built-in tool name, or one in the `xerj_hub_*` family, which the dispatcher
/// routes by prefix.
fn is_builtin_tool(name: &str) -> bool {
    name.starts_with("xerj_hub_")
        || crate::tool_specs_with(true)
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["name"].as_str() == Some(name)))
}

/// Run `recipe` over `args` and `hits` and return the MCP `tools/call` result.
///
/// Success is one text block holding the RFC 8785 (JCS) bytes of the sealed
/// `RecipeExecutionReport`, `isError: false`. Any `RecipeError` (bad arguments,
/// an excluded licence, a missing corpus, too little evidence, an output that
/// fails its schema or a check) is an `isError: true` text block with the
/// error, as for every other tool. The call carries no `ScopeGuard` and its
/// origin is untrusted, so a `requires_scope` recipe is refused here.
pub fn dispatch_recipe_tool_call(
    recipe: &PackRecipe,
    args: &Value,
    hits: &[RecipeCorpusHit],
) -> Value {
    let sealed = RecipeRunner::execute(
        recipe,
        args,
        hits,
        None,
        InputOrigin::UntrustedRepository,
        chrono::Utc::now(),
    )
    .and_then(|report| report.to_jcs(recipe));
    match sealed {
        Ok(jcs) => match String::from_utf8(jcs) {
            Ok(text) => tool_text(text, false),
            Err(_) => tool_text("the sealed report is not UTF-8", true),
        },
        Err(e) => tool_text(e.to_string(), true),
    }
}

/// The `tools/call` arguments of `msg`, `{}` when absent.
pub(crate) fn call_arguments(msg: &Value) -> Value {
    msg.pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe_toml(name: &str, extra_header: &str) -> String {
        format!(
            r#"
[recipe]
format_version = "xerj-recipe/v1"
pack = "demo"
name = "{name}"
description = "A recipe used by the registry tests."
{extra_header}

[corpora]
required = ["demo-corpus"]

[ranking]
strategy = "score_desc"
top_k = 3

[output_schema]
required_fields = ["items"]

[output_schema.properties.recipe]
kind = "string"

[output_schema.properties.pack]
kind = "string"

[output_schema.properties.items]
kind = "array"
non_empty = true
"#
        )
    }

    fn recipe(name: &str, extra_header: &str) -> PackRecipe {
        PackRecipe::from_toml_str(&recipe_toml(name, extra_header)).unwrap()
    }

    fn hit_line(corpus: &str, record_id: &str) -> String {
        json!({
            "corpus": corpus,
            "licence_class": "adapt-with-attribution",
            "record_id": record_id,
            "title": "T",
            "excerpt": "E",
            "score": 0.5,
            "published_date": "2026-11-01",
            "pin_sha": "a".repeat(40),
            "fetched_at": "2026-11-05",
        })
        .to_string()
    }

    fn refusal(r: PackRecipe) -> String {
        format!("{:#}", RecipeRegistry::new().register(r).unwrap_err())
    }

    #[test]
    fn a_registry_starts_empty_and_adds_nothing_to_the_default_list() {
        let none = RecipeRegistry::new();
        assert!(none.is_empty() && none.tool_specs().is_empty());
        assert_eq!(
            crate::tool_specs_with_recipes(true, &none),
            crate::tool_specs_with(true)
        );
        assert_eq!(
            crate::tool_specs_with_recipes(false, &none),
            crate::tool_specs_with(false)
        );
    }

    #[test]
    fn registered_recipes_follow_the_built_in_tools_in_order() {
        let mut registry = RecipeRegistry::new();
        registry.register(recipe("demo.first", "")).unwrap();
        registry.register(recipe("demo.second", "")).unwrap();
        assert_eq!(
            registry.tool_names(),
            ["xerj_demo_first", "xerj_demo_second"]
        );

        let all = crate::tool_specs_with_recipes(true, &registry);
        let all = all.as_array().unwrap();
        let built_in = crate::tool_specs_with(true);
        let built_in = built_in.as_array().unwrap();
        assert_eq!(&all[..built_in.len()], built_in.as_slice());
        assert_eq!(all[built_in.len()]["name"], "xerj_demo_first");
        assert_eq!(all[built_in.len() + 1]["name"], "xerj_demo_second");
        assert_eq!(all.len(), built_in.len() + 2);
    }

    #[test]
    fn a_recipe_that_cannot_be_served_honestly_is_refused_at_registration() {
        let why = refusal(recipe("demo.sampled", "sample = true"));
        assert!(why.contains("sample = true"), "{why}");

        let why = refusal(recipe("demo.scoped", "requires_scope = true"));
        assert!(why.contains("requires_scope = true"), "{why}");

        let why = refusal(recipe("demo.named", r#"mcp_tool_name = "demo_named""#));
        assert!(why.contains("must start with `xerj_`"), "{why}");

        let why = refusal(recipe("demo.search", r#"mcp_tool_name = "xerj_search""#));
        assert!(why.contains("already a built-in tool"), "{why}");

        let why = refusal(recipe("demo.hub", r#"mcp_tool_name = "xerj_hub_demo""#));
        assert!(why.contains("already a built-in tool"), "{why}");

        let mut registry = RecipeRegistry::new();
        registry.register(recipe("demo.once", "")).unwrap();
        let why = format!(
            "{:#}",
            registry.register(recipe("demo.once", "")).unwrap_err()
        );
        assert!(why.contains("already served by another recipe"), "{why}");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn the_builtin_recipes_register_and_the_cyber_sample_does_not() {
        let mut registry = RecipeRegistry::new();
        for recipe in xerj_common::pack_recipe::builtin_recipes().unwrap() {
            registry.register(recipe).unwrap();
        }
        assert_eq!(
            registry.tool_names(),
            [
                "xerj_write_outline",
                "xerj_write_section",
                "xerj_bio_outline",
                "xerj_bio_evidence"
            ]
        );
        for sample in xerj_common::pack_recipe::sample_recipes().unwrap() {
            let why = refusal(sample);
            assert!(why.contains("sample = true"), "{why}");
        }
    }

    #[test]
    fn load_dir_reads_toml_files_in_name_order_and_names_a_bad_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.toml"), recipe_toml("demo.b", "")).unwrap();
        std::fs::write(dir.path().join("a.toml"), recipe_toml("demo.a", "")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a recipe").unwrap();
        let registry = RecipeRegistry::load_dir(dir.path()).unwrap();
        assert_eq!(registry.tool_names(), ["xerj_demo_a", "xerj_demo_b"]);

        std::fs::write(dir.path().join("c.toml"), "[recipe]\nname = ").unwrap();
        let why = format!("{:#}", RecipeRegistry::load_dir(dir.path()).unwrap_err());
        assert!(why.contains("c.toml"), "{why}");

        let missing = dir.path().join("absent");
        let why = format!("{:#}", RecipeRegistry::load_dir(&missing).unwrap_err());
        assert!(why.contains("cannot read the recipe directory"), "{why}");
    }

    #[test]
    fn a_corpus_dir_serves_pinned_hits_and_refuses_a_file_that_names_another_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let source = CorpusDir::new(dir.path());
        let r = recipe("demo.hits", "");
        assert!(source.hits(&r).unwrap().is_empty(), "no file, no hits");

        let path = dir.path().join("demo-corpus.jsonl");
        std::fs::write(&path, format!("{}\n\n", hit_line("demo-corpus", "r1"))).unwrap();
        assert_eq!(source.hits(&r).unwrap().len(), 1);

        std::fs::write(&path, hit_line("other-corpus", "r1")).unwrap();
        let why = source.hits(&r).unwrap_err();
        assert!(
            why.contains("other-corpus") && why.contains("demo-corpus"),
            "{why}"
        );

        std::fs::write(&path, "{not json").unwrap();
        assert!(source.hits(&r).unwrap_err().contains("demo-corpus.jsonl:1"));
    }

    #[test]
    fn a_corpus_slug_that_is_not_a_file_name_is_refused_not_opened() {
        let source = CorpusDir::new("unused");
        for slug in ["ns:corpus", ".hidden", "-lead"] {
            let why = source.read_corpus(slug).unwrap_err();
            assert!(
                why.contains("cannot be read from a snapshot file"),
                "{slug}: {why}"
            );
        }
    }

    /// The real read loop, with a registry: `tools/list` carries the recipe's
    /// tool and `tools/call` answers with the sealed report, with the node it
    /// would proxy to unreachable (a recipe tool never calls it).
    #[tokio::test]
    async fn the_stdio_loop_lists_and_runs_a_recipe_tool_without_a_node() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hits.toml"), recipe_toml("demo.hits", "")).unwrap();
        std::fs::create_dir(dir.path().join(CORPORA_SUBDIR)).unwrap();
        std::fs::write(
            dir.path().join(CORPORA_SUBDIR).join("demo-corpus.jsonl"),
            hit_line("demo-corpus", "r1"),
        )
        .unwrap();
        let registry = Arc::new(RecipeRegistry::load_dir(dir.path()).unwrap());

        let (mut client_in, server_in) = tokio::io::duplex(64 * 1024);
        let (server_out, client_out) = tokio::io::duplex(64 * 1024);
        let ctx = crate::Ctx {
            client: reqwest::Client::builder().build().unwrap(),
            base_url: "http://127.0.0.1:9".to_string(),
            auth: None,
        };
        let server = tokio::spawn(crate::serve(
            ctx,
            registry,
            BufReader::new(server_in),
            server_out,
        ));
        for msg in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "xerj_demo_hits", "arguments": {}}}),
        ] {
            client_in
                .write_all(format!("{msg}\n").as_bytes())
                .await
                .unwrap();
        }
        // End of input: the loop answers the calls still running, then stops.
        drop(client_in);

        let mut replies = Vec::new();
        let mut lines = BufReader::new(client_out).lines();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Ok(Some(line)) = lines.next_line().await {
                replies.push(serde_json::from_str::<Value>(&line).unwrap());
            }
        })
        .await
        .expect("stdout was not closed after the last reply");
        server.await.unwrap().unwrap();

        let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap();
        let tools = by_id(1)["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.last().unwrap()["name"], "xerj_demo_hits");
        let call = &by_id(2)["result"];
        assert_eq!(call["isError"], false, "{call}");
        let report: Value =
            serde_json::from_str(call["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(report["mcp_tool_name"], "xerj_demo_hits");
        assert_eq!(report["output"]["items"][0]["record_id"], "r1");
    }
}
