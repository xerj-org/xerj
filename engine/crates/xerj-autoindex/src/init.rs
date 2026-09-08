//! `xerj init` — wire XERJ into the coding agents already installed here.
//!
//! The adoption lesson from the agent-tool field is one-command setup: the
//! tools that got used are the ones a user (or an agent) could wire in with a
//! single command that writes the right config files and gets out of the way.
//! This does exactly that, and nothing else: detect which agent surfaces
//! exist, write the smallest useful teaching file for each, register the MCP
//! server, and never touch a byte we did not write without a `.bak` backup.
//!
//! Deliberately NOT here: prompt rewriting, command interception, or anything
//! that changes what the agent was going to do. XERJ is a tool the agent
//! calls; init only makes the tool visible.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const USAGE: &str = "\
xerj init — wire XERJ into the coding agents installed in this project

USAGE:
    xerj init [OPTIONS]

Detects agent surfaces in the current directory and writes the minimal
config for each — with a .bak backup for any file it edits:

    .mcp.json                    register the `xerj mcp` server (Claude Code
                                 project scope; merged, other servers kept)
    .claude/skills/xerj/SKILL.md teach `xerj search` / `xerj def` (10 lines)
    .cursor/rules/xerj.mdc       same teaching file for Cursor (if .cursor/)
    AGENTS.md                    a short XERJ section (appended if the file
                                 exists, created otherwise)

OPTIONS:
    --url <U>       node endpoint baked into the MCP config
                    (default $XERJ_URL or http://localhost:9200)
    --dry-run       print what would be written, write nothing
    -h, --help      this help

After init: restart the agent session so it picks up the new tools, then
`xerj autoindex <folder>` anything you want searchable.
";

/// The ten-line teaching file. One idea per line, no filler: what the tools
/// are, when to reach for them, what comes back.
fn skill_md() -> String {
    "---\nname: xerj\ndescription: Definition-first code search over locally indexed repos (xerj search / xerj def)\n---\n\n\
     Use XERJ instead of grep when you need code you cannot see in the open files:\n\n\
     - `xerj def \"<symbol>\"` — where is this defined? Returns file:line + signature in one call.\n\
     - `xerj search \"<plain words>\"` — ranked passages, definitions first; cite the printed file:line.\n\
     - `xerj autoindex <folder>` — index a repo first (once; re-index is incremental).\n\
     - MCP: the `xerj_search` tool accepts a plain string query and returns the same passages.\n\n\
     Trust the passage over memory: it is the actual code at that path.\n"
        .to_string()
}

fn agents_md_section() -> String {
    "\n## XERJ (code search)\n\n\
     This project uses XERJ for reference coding. Before writing code that touches\n\
     unfamiliar territory: `xerj def \"<symbol>\"` for go-to-definition, or\n\
     `xerj search \"<plain words>\"` for ranked, definition-first passages.\n\
     Index a repo with `xerj autoindex <folder>`. Cite file:line for anything you rely on.\n"
        .to_string()
}

/// Back up `path` to `path.bak` before the first edit. Returns true if a
/// backup was made (false when the file did not exist).
fn backup(path: &Path, dry: bool) -> bool {
    if !path.exists() {
        return false;
    }
    if !dry {
        let bak = path.with_extension(match path.extension() {
            Some(e) => format!("{}.bak", e.to_string_lossy()),
            None => "bak".to_string(),
        });
        let _ = fs::copy(path, bak);
    }
    true
}

fn write_file(path: &Path, content: &str, dry: bool) -> std::io::Result<()> {
    if dry {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

/// Merge the xerj server into an `.mcp.json`, preserving everything else.
/// Absolute binary path — an MCP host launched from a desktop icon does not
/// inherit the shell PATH (same rule the `xerj mcp` help documents).
fn merged_mcp_json(existing: Option<&str>, url: &str) -> Result<String, String> {
    let bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "xerj".to_string());
    let entry = json!({
        "command": bin,
        "args": ["mcp"],
        "env": { "XERJ_URL": url }
    });
    let mut root: Value = match existing {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| format!("existing .mcp.json is not valid JSON ({e}); not touching it"))?,
        None => json!({}),
    };
    if !root.is_object() {
        return Err("existing .mcp.json is not a JSON object; not touching it".into());
    }
    let servers = root
        .as_object_mut()
        .expect("checked object above")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err("existing .mcp.json mcpServers is not an object; not touching it".into());
    }
    servers
        .as_object_mut()
        .expect("checked object above")
        .insert("xerj".into(), entry);
    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}

/// One detected surface, written. Returns the human line to print.
fn report(action: &str, path: &Path) -> String {
    format!("  {action:<8} {}", path.display())
}

/// Entry point for the `init` subcommand. Returns a process exit code.
pub fn run_init_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:9200".to_string());
    let mut dry = false;
    let mut it = std::env::args().skip(2);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return 0;
            }
            "--url" => url = it.next().unwrap_or(url),
            "--dry-run" => dry = true,
            other => {
                eprintln!("xerj init: unexpected argument '{other}'\n\n{USAGE}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match run_init_in(&cwd, &url, dry) {
        Ok(lines) => {
            println!(
                "xerj init{} — wiring XERJ into this project:\n",
                if dry { " (dry run)" } else { "" }
            );
            for l in &lines {
                println!("{l}");
            }
            println!(
                "\ndone. Restart the agent session to pick up the tools, then\n\
                 `xerj autoindex <folder>` anything you want searchable."
            );
            0
        }
        Err(e) => {
            eprintln!("xerj init: {e}");
            1
        }
    }
}

/// The writable core, separated from argv/cwd so tests drive it directly.
pub(crate) fn run_init_in(cwd: &Path, url: &str, dry: bool) -> Result<Vec<String>, String> {
    let mut out = Vec::new();

    // 1) MCP registration (.mcp.json, Claude Code project scope; other hosts
    //    read the same shape). Merge, never clobber.
    let mcp_path = cwd.join(".mcp.json");
    let existing = fs::read_to_string(&mcp_path).ok();
    let already = existing
        .as_deref()
        .map(|t| t.contains("\"xerj\""))
        .unwrap_or(false);
    if already {
        out.push(report("kept", &mcp_path));
    } else {
        let merged = merged_mcp_json(existing.as_deref(), url)?;
        let had = backup(&mcp_path, dry);
        write_file(&mcp_path, &merged, dry).map_err(|e| e.to_string())?;
        out.push(report(if had { "merged" } else { "wrote" }, &mcp_path));
    }

    // 2) Claude Code skill — written whenever the project (or user) uses
    //    Claude Code; the directory existing is the signal.
    let claude_dir_present = cwd.join(".claude").is_dir()
        || dirs_home()
            .map(|h| h.join(".claude").is_dir())
            .unwrap_or(false);
    if claude_dir_present {
        let skill = cwd.join(".claude/skills/xerj/SKILL.md");
        if skill.exists() {
            out.push(report("kept", &skill));
        } else {
            write_file(&skill, &skill_md(), dry).map_err(|e| e.to_string())?;
            out.push(report("wrote", &skill));
        }
    }

    // 3) Cursor rule — only when the project already has a .cursor directory.
    if cwd.join(".cursor").is_dir() {
        let rule = cwd.join(".cursor/rules/xerj.mdc");
        if rule.exists() {
            out.push(report("kept", &rule));
        } else {
            write_file(&rule, &skill_md(), dry).map_err(|e| e.to_string())?;
            out.push(report("wrote", &rule));
        }
    }

    // 4) AGENTS.md — the cross-agent convention. Append a short section if the
    //    file exists without one; create the file if absent.
    let agents = cwd.join("AGENTS.md");
    match fs::read_to_string(&agents) {
        Ok(text) if text.contains("xerj search") || text.contains("xerj def") => {
            out.push(report("kept", &agents));
        }
        Ok(text) => {
            backup(&agents, dry);
            write_file(&agents, &format!("{text}{}", agents_md_section()), dry)
                .map_err(|e| e.to_string())?;
            out.push(report("appended", &agents));
        }
        Err(_) => {
            write_file(
                &agents,
                &format!("# Agent notes\n{}", agents_md_section()),
                dry,
            )
            .map_err(|e| e.to_string())?;
            out.push(report("wrote", &agents));
        }
    }

    Ok(out)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn fresh_project_gets_mcp_and_agents_md() {
        let d = tmp();
        let lines = run_init_in(d.path(), "http://localhost:9200", false).unwrap();
        assert!(lines.iter().any(|l| l.contains(".mcp.json")));
        let mcp: Value =
            serde_json::from_str(&fs::read_to_string(d.path().join(".mcp.json")).unwrap()).unwrap();
        assert!(mcp["mcpServers"]["xerj"]["args"][0] == "mcp");
        assert_eq!(
            mcp["mcpServers"]["xerj"]["env"]["XERJ_URL"],
            "http://localhost:9200"
        );
        let agents = fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert!(agents.contains("xerj def"));
    }

    #[test]
    fn existing_mcp_json_is_merged_not_clobbered_and_backed_up() {
        let d = tmp();
        fs::write(
            d.path().join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"other-bin"}}}"#,
        )
        .unwrap();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        let mcp: Value =
            serde_json::from_str(&fs::read_to_string(d.path().join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"]["other"]["command"], "other-bin");
        assert!(mcp["mcpServers"]["xerj"].is_object());
        assert!(d.path().join(".mcp.json.bak").exists());
    }

    #[test]
    fn second_run_is_idempotent() {
        let d = tmp();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        let first = fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        let second = fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert_eq!(first, second, "AGENTS.md must not grow on re-run");
        assert!(lines.iter().all(|l| l.contains("kept")), "{lines:?}");
    }

    #[test]
    fn invalid_mcp_json_is_left_alone() {
        let d = tmp();
        fs::write(d.path().join(".mcp.json"), "{not json").unwrap();
        let err = run_init_in(d.path(), "http://x:1", false).unwrap_err();
        assert!(err.contains("not valid JSON"));
        assert_eq!(
            fs::read_to_string(d.path().join(".mcp.json")).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let d = tmp();
        run_init_in(d.path(), "http://x:1", true).unwrap();
        assert!(!d.path().join(".mcp.json").exists());
        assert!(!d.path().join("AGENTS.md").exists());
    }

    #[test]
    fn cursor_rule_written_only_when_cursor_dir_exists() {
        let d = tmp();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        assert!(!d.path().join(".cursor/rules/xerj.mdc").exists());
        fs::create_dir_all(d.path().join(".cursor")).unwrap();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        assert!(d.path().join(".cursor/rules/xerj.mdc").exists());
    }
}
