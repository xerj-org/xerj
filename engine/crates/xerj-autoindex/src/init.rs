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

/// `--help` for `xerj init`. `hub` is this build's [`xerj_common::hub::hub_key_configured`]:
/// a build with no signing key does not write the hub teaching files and must
/// not tell the reader that `xerj "<question>"` asks the hub (that command
/// exits 5 there).
fn usage(hub: bool) -> String {
    let hub_skill = if hub {
        "    .claude/skills/xerj-hub/SKILL.md\n                                 teach `xerj \"<question>\"`: when to ask the\n                                 hub, `--def`, the MCP registration per client\n"
    } else {
        ""
    };
    let hub_rule = if hub {
        "    .cursor/rules/xerj-hub.mdc   the hub teaching file for Cursor (if .cursor/)\n"
    } else {
        ""
    };
    let hub_next = if hub {
        "Public libraries,\nstandards and advisories need no indexing: `xerj \"<question>\"` asks the hub.\n"
    } else {
        "This build has no hub signing key, so `xerj \"<question>\"` is not available\n(it exits 5). For public libraries, use `xerj corpus` and `xerj code`.\n"
    };
    format!(
        "\
xerj init — wire XERJ into the coding agents installed in this project

USAGE:
    xerj init [OPTIONS]

Detects agent surfaces in the current directory and writes the minimal
config for each — with a .bak backup for any file it edits:

    .mcp.json                    register the `xerj mcp` server (Claude Code
                                 project scope; merged, other servers kept)
    .cursor/mcp.json             the same registration for Cursor (if .cursor/)
    .claude/skills/xerj/SKILL.md teach `xerj search` / `xerj def` (10 lines)
{hub_skill}    .cursor/rules/xerj.mdc       same teaching file for Cursor (if .cursor/)
{hub_rule}    AGENTS.md                    a short XERJ section (appended if the file
                                 exists, created otherwise)

The teaching files carry an `xerj-init-version` marker. A re-run upgrades one
an older init wrote and nobody edited (with a .bak); an edited file, or one
without the marker, is yours and is kept.

OPTIONS:
    --url <U>       node endpoint baked into the MCP config
                    (default $XERJ_URL or http://localhost:9200)
    --dry-run       print what would be written, write nothing
    -h, --help      this help

After init: restart the agent session so it picks up the new tools, then
`xerj autoindex <folder> --prefix <name>` anything you want searchable.
Pass that same `--prefix <name>` to `xerj search` and `xerj def`; without it
they search the default `ax` prefix. {hub_next}init also prints the MCP registration line for each client.

Auth: nothing secret is written (`.mcp.json` is meant to be committed). For a
loopback node the MCP server reads <data-dir>/admin.key by itself and says so
on stderr; for any other node add \"XERJ_AUTH\": \"ApiKey <key>\" to the env
block — init prints which case you are in.
"
    )
}

/// The ten-line teaching file. One idea per line, no filler: what the tools
/// are, when to reach for them, what comes back.
fn skill_md() -> String {
    "---\nname: xerj\ndescription: Definition-first code search over locally indexed repos (xerj search / xerj def)\n---\n\n\
     Use XERJ instead of grep when you need code you cannot see in the open files:\n\n\
     - `xerj def \"<symbol>\"` — where is this defined? Returns file:line + signature in one call.\n\
     - `xerj search \"<plain words>\"` — ranked passages, definitions first; cite the printed file:line.\n\
     - `xerj autoindex <folder> --prefix <name>` — index a repo first (once; re-index is incremental).\n\
     - Pass the same `--prefix <name>` to `xerj search` / `xerj def`; without it they search the default `ax` prefix.\n\
     - MCP: the `xerj_search` tool accepts a plain string query and returns the same passages.\n\n\
     Trust the passage over memory: it is the actual code at that path.\n"
        .to_string()
}

fn agents_md_section() -> String {
    "\n## XERJ (code search)\n\n\
     This project uses XERJ for reference coding. Before writing code that touches\n\
     unfamiliar territory: `xerj def \"<symbol>\"` for go-to-definition, or\n\
     `xerj search \"<plain words>\"` for ranked, definition-first passages.\n\
     Index each repo under its own prefix: `xerj autoindex <folder> --prefix <name>`, then\n\
     pass the same `--prefix <name>` to `xerj search` and `xerj def` (without it they search\n\
     the default `ax` prefix). Cite file:line for anything you rely on.\n"
        .to_string()
}

/// The second teaching file (Corpus Hub v2, plan section 9): when to ask the
/// hub instead of searching local files, the one line that does it, the exact
/// form, how to stop a node safely, and the MCP registration per client.
///
/// Kept as a raw string rather than a `.md` asset: the repository ignores
/// `*.md` by default, and a `include_str!` of an ignored file compiles on a
/// developer machine and fails in CI.
///
/// Rules this text follows (docs/research/llms-txt-2026-09/README.md section
/// 6): no obligation language; every step ends with what success looks like;
/// the registration is printed per client with the root key that client reads
/// (`mcpServers` vs VS Code's `servers`); no line over 300 characters.
const HUB_SKILL_MD: &str = r##"---
name: xerj-hub
description: Answer a question about public source code, a standard, a regulation or an advisory feed from a pre-indexed hub corpus with one command, xerj "your question". Use it for exact public material you cannot quote from memory. Not for this project's own files.
---

# XERJ hub: ask, do not clone

Pick the source before you search:

- **HUB**: public material you cannot quote exactly. A library's source at a pinned commit, a
  standard, a regulation, an advisory feed.
- **LOCAL**: files only this machine has. This project, private documents, exports.
  `xerj autoindex <folder> --prefix <name>` once, then pass that `--prefix <name>` to
  `xerj search` and `xerj def`. Without `--prefix` they search the default `ax` prefix.
- **NEITHER**: a fact you can already state exactly. Retrieval on memorised content only adds cost.

## Ask the hub

```sh
xerj "<question>"
```

One line. No corpus name, no clone, no index. XERJ picks the corpus (your project's dependencies
first), fetches its signed, pre-indexed pack on first use, and answers. Retrieval is lexical, not
semantic: use the words and identifiers the source uses. The bare form needs a space in the
question; for a single word write `xerj ask "<word>"`.

A good answer is three lines, then any `warning:` lines:

```text
<corpus>  pin <date> (<N> days old)  <licence>  <file>:<line>
<the passage, or the declaration>
answered from: local pack
```

`pin` is the source revision the corpus was built from: check its age when you ask about the current
version. `answered from:` can also say `hosted hub`. Tell the user which corpus, pin date and licence
your answer rests on. `warning: approach-only licence` means read the source, do not copy it.

## Exact questions

```sh
xerj ask --def <SYMBOL>
```

Use `--def` for a value, a signature or a default. It prints the definition (`file:line`, the
declaration, and a `value:` line for a constant or macro) and no passages. Add `--corpus <slug>` when
more than one corpus defines the name.

## When it does not answer

- Exit 4: more than one corpus fits. The candidates print, one per line. Run again with
  `--corpus <slug>`.
- Exit 1, `no hub corpus matches`: the hub does not hold it. Say so. Do not answer from memory.
- Exit 3: a downloaded pack failed its checksum or signature check and was discarded. Do not use it.
  Tell the user.
- Exit 5: this build has no hub public key, so no pack can be verified and nothing was downloaded.
  Tell the user; a retry will not help.
- `run: xerj hub pull <slug> (<MB> MB)`: the pack is above the automatic download limit. Run that
  line, then ask again. Success: `pulled <slug> <pin8> <bytes> → <n> indices, <defs> definitions`.

## The node, and where the question goes

If a node answers on `--url`, `$XERJ_URL` or `localhost:9200` (first one set), XERJ uses it as it
is. If none does, XERJ starts its own on a free loopback port and records its PID.
Stop only the node you started: `xerj hub stop` for XERJ's own (success: `stopped the hub node
(pid <N>)` or `no hub node was started by xerj; nothing to stop`); for a node you started by hand,
kill only the PID you recorded when you started it. Never `pkill xerj` or `killall xerj`: other XERJ nodes may be running
on this machine.

By default, inside a git repository the question is answered on this machine. Outside one it may be
sent to the hosted hub. `xerj hub config hosted off` forbids that.

## MCP

`xerj mcp` serves three hub tools. They run the same flow as `xerj ask`:
- `xerj_hub_ask` `{question, corpus?, mode?, max_tokens?}`: the shell line above, as one call.
- `xerj_hub_def` `{slug, symbol, k?}`: `xerj ask --corpus <slug> --def <SYMBOL>`.
- `xerj_hub_find` `{question, k?}`: which corpora could answer, best first. No passages.

A hub tool answers only from packs already on this machine; it never downloads one. When a corpus
is not here, the answer is the `xerj hub pull <slug>` line: run it in a shell, then ask again.
Nor does a hub tool fetch the catalogue: it reads the cached copy. If it says no catalogue is
cached, run `xerj hub sync` in a shell once, then call the tool again.

Register `xerj mcp` once, then restart the session. Success: `xerj_search` is in the tool list.
Use the absolute path to the binary: an editor started from a desktop icon does not inherit your
shell PATH. `command -v xerj` prints it (PowerShell: `(Get-Command xerj).Source`).

- Claude Code: `claude mcp add --scope user xerj -- "$(command -v xerj)" mcp`
  (PowerShell or cmd: put the full path of `xerj.exe` in place of `"$(command -v xerj)"`)
- `.mcp.json` (Claude Code, project scope; `xerj init` writes it), Cursor, Windsurf, Claude Desktop,
  Cline: root key `mcpServers`.
  `{"mcpServers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"]}}}`
- VS Code (`.vscode/mcp.json`): root key `servers`, not `mcpServers`. A block under the wrong key is
  silently ignored.
  `{"servers": {"xerj": {"type": "stdio", "command": "/abs/path/to/xerj", "args": ["mcp"]}}}`
"##;

/// The MCP tool whose presence proves `xerj mcp` is registered. It must be a
/// tool every `xerj mcp` serves. The hub tools (`xerj_hub_*`) are served too,
/// but `xerj_search` is the proof the skill and the printed text have always
/// used, and it stays the one check.
const MCP_SUCCESS_TOOL: &str = "xerj_search";

/// The default node URL; the printed Claude Code line carries `-e XERJ_URL=`
/// only when init was pointed somewhere else.
const DEFAULT_URL: &str = "http://localhost:9200";

/// Quote `s` as one argument for the shell the user is most likely to paste
/// into: POSIX single quotes on Unix, double quotes on Windows (PowerShell and
/// cmd both accept them; a Windows path cannot contain `"`).
fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// What `xerj init` prints for the clients `.mcp.json` does not cover: the
/// Claude Code user-scope line with THIS binary's absolute path and the node
/// URL `.mcp.json` got (no `$(command -v …)`, which only a POSIX shell
/// expands), then the root key each other client reads. A unit test pins it to
/// the hub skill so the two cannot drift apart.
fn mcp_registration(bin: &str, url: &str) -> String {
    let env = if url == DEFAULT_URL {
        String::new()
    } else {
        format!(" -e XERJ_URL={}", shell_quote(url))
    };
    format!(
        "MCP registration for other clients (restart the session after; success: `{MCP_SUCCESS_TOOL}` is in the tool list):\n\
         \x20 Claude Code, every project:  claude mcp add --scope user xerj{env} -- {} mcp\n\
         \x20 Cursor:  `.cursor/mcp.json` when that directory exists (init writes it; root key \"mcpServers\")\n\
         \x20 Windsurf, Claude Desktop, Cline:  copy the \"xerj\" entry from .mcp.json; the root key is \"mcpServers\"\n\
         \x20 VS Code (.vscode/mcp.json):  root key \"servers\", not \"mcpServers\"; the entry needs \"type\": \"stdio\"",
        shell_quote(bin)
    )
}

/// What init says last: where to go next, the hub's one line (only in a build
/// that can verify hub packs — `hub`), and the per-client MCP registration.
fn closing_message(bin: &str, url: &str, hub: bool) -> String {
    let hub_line = if hub {
        "Public libraries, standards and advisories need no indexing: ask the hub with\n\
         `xerj \"<question>\"` (the xerj-hub skill says when).\n\n"
    } else {
        ""
    };
    format!(
        "\ndone. Restart the agent session to pick up the tools, then\n\
         `xerj autoindex <folder> --prefix <name>` anything you want searchable.\n\
         Pass that same `--prefix <name>` to `xerj search` and `xerj def`\n\
         (without it they search the default `ax` prefix).\n\n\
         {hub_line}{}",
        mcp_registration(bin, url)
    )
}

/// This binary's absolute path, the same string `.mcp.json` gets.
fn self_bin() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "xerj".to_string())
}

/// The generation of the teaching files init writes. Bump it whenever
/// `skill_md()` or `HUB_SKILL_MD` changes, so a file an older init wrote (and
/// the user never edited) is upgraded on the next run. A unit test pins the
/// texts' hashes to this number, so a text change without a bump fails CI.
const INIT_FILES_VERSION: u32 = 3;

/// The marker's prefix. The full line is
/// `<!-- xerj-init-version: <N> sha256:<16 hex> -->`: the generation, and the
/// hash of the file as written minus this line, so a re-run can tell a file
/// init wrote and nobody touched from one the user edited.
const MARKER_PREFIX: &str = "<!-- xerj-init-version: ";

/// The first 8 bytes of SHA-256, hex. Line endings are normalised first, so a
/// checkout or editor that turned `\n` into `\r\n` is not mistaken for an edit.
fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.replace("\r\n", "\n").as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `content` with the version marker inserted: after the YAML frontmatter
/// when there is one (a skill loader reads `---` on line 1), at the top
/// otherwise. Removing the marker line gives back `content` exactly.
fn stamped(content: &str) -> String {
    let marker = format!(
        "{MARKER_PREFIX}{INIT_FILES_VERSION} sha256:{} -->\n",
        content_hash(content)
    );
    let at = content
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n").map(|i| 4 + i + "\n---\n".len()))
        .unwrap_or(0);
    format!("{}{marker}{}", &content[..at], &content[at..])
}

/// What a re-run found in an existing teaching file.
#[derive(Debug, PartialEq)]
enum Stamp {
    /// No marker: written by hand or by an init that predates markers. The
    /// user owns it.
    None,
    /// A marker, and the text still hashes to what init wrote.
    Pristine(u32),
    /// A marker, but the text changed since init wrote it.
    Edited(u32),
}

fn read_stamp(text: &str) -> Stamp {
    let mut found = None;
    let mut rest = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if found.is_none() {
            if let Some(m) = line.strip_prefix(MARKER_PREFIX) {
                let mut parts = m.split_whitespace();
                let version = parts.next().and_then(|v| v.parse::<u32>().ok());
                let hash = parts.next().and_then(|h| h.strip_prefix("sha256:"));
                if let (Some(v), Some(h)) = (version, hash) {
                    found = Some((v, h.to_string()));
                    continue;
                }
            }
        }
        rest.push_str(line);
    }
    match found {
        None => Stamp::None,
        Some((v, h)) if h == content_hash(&rest) => Stamp::Pristine(v),
        Some((v, _)) => Stamp::Edited(v),
    }
}

/// Write the teaching file `content` (stamped with the version marker) to
/// `path`. An existing file is upgraded only when an older init wrote it and
/// nobody edited it since, after a `.bak` backup (fail-closed, as for
/// `.mcp.json`); an edited or unmarked file is the user's and is kept.
/// Returns the report line for it.
fn write_teaching_file(path: &Path, content: &str, dry: bool) -> Result<String, String> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_file(path, &stamped(content), dry).map_err(|e| e.to_string())?;
            return Ok(report("wrote", path, dry));
        }
        // Present but unreadable (not UTF-8, no permission): the user's.
        Err(_) => return Ok(report("kept", path, dry)),
    };
    match read_stamp(&existing) {
        Stamp::Pristine(v) if v < INIT_FILES_VERSION => {
            backup(path, dry)?;
            write_file(path, &stamped(content), dry).map_err(|e| e.to_string())?;
            Ok(report("upgraded", path, dry))
        }
        Stamp::Edited(v) if v < INIT_FILES_VERSION => Ok(format!(
            "{} (edited since xerj init wrote it; not upgraded to v{INIT_FILES_VERSION})",
            report("kept", path, dry)
        )),
        _ => Ok(report("kept", path, dry)),
    }
}

/// Back up `path` to `path.bak` before the first edit. Returns true if a
/// backup was made (false when the file did not exist). A backup that cannot
/// be written is an error, so the caller never edits a file it could not save
/// a copy of first.
fn backup(path: &Path, dry: bool) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    if !dry {
        let bak = path.with_extension(match path.extension() {
            Some(e) => format!("{}.bak", e.to_string_lossy()),
            None => "bak".to_string(),
        });
        fs::copy(path, &bak).map_err(|e| {
            format!(
                "could not back up {} to {} ({e}); not touching it",
                path.display(),
                bak.display()
            )
        })?;
    }
    Ok(true)
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
    let bin = self_bin();
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
///
/// A dry run used to say `wrote` for a file it did not create. The verb
/// becomes `would write` / `would merge` / `would append` / `would upgrade`
/// so the line matches `--dry-run`'s "write nothing".
fn report(action: &str, path: &Path, dry: bool) -> String {
    let action = match (dry, action) {
        (true, "wrote") => "would write",
        (true, "merged") => "would merge",
        (true, "appended") => "would append",
        (true, "upgraded") => "would upgrade",
        _ => action,
    };
    format!("  {action:<8} {}", path.display())
}

/// Merge the xerj server into one MCP config, or report `kept` when the
/// file already names it. Shared by Claude Code's `.mcp.json` and Cursor's
/// `.cursor/mcp.json` (same `mcpServers` shape).
fn register_mcp(path: &Path, url: &str, dry: bool) -> Result<String, String> {
    let existing = fs::read_to_string(path).ok();
    let already = existing
        .as_deref()
        .map(|t| t.contains("\"xerj\""))
        .unwrap_or(false);
    if already {
        return Ok(report("kept", path, dry));
    }
    let merged = merged_mcp_json(existing.as_deref(), url)?;
    let had = backup(path, dry)?;
    write_file(path, &merged, dry).map_err(|e| e.to_string())?;
    Ok(report(if had { "merged" } else { "wrote" }, path, dry))
}

/// Entry point for the `init` subcommand. Returns a process exit code.
pub fn run_init_cli() -> i32 {
    let mut url = std::env::var("XERJ_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let mut dry = false;
    let mut it = xerj_common::argv::args_after_subcommand().into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{}", usage(xerj_common::hub::hub_key_configured()));
                return 0;
            }
            "--url" => url = it.next().unwrap_or(url),
            "--dry-run" => dry = true,
            other => {
                eprintln!(
                    "xerj init: unexpected argument '{other}'\n\n{}",
                    usage(xerj_common::hub::hub_key_configured())
                );
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let hub = xerj_common::hub::hub_key_configured();
    match run_init_with(&cwd, &url, dry, hub) {
        Ok(lines) => {
            println!(
                "xerj init{} — wiring XERJ into this project:\n",
                if dry { " (dry run)" } else { "" }
            );
            for l in &lines {
                println!("{l}");
            }
            println!("{}", closing_message(&self_bin(), &url, hub));
            0
        }
        Err(e) => {
            eprintln!("xerj init: {e}");
            1
        }
    }
}

/// [`run_init_with`] in a build that can use the hub (the hub teaching files
/// are written). Tests drive this; the CLI passes the build's real answer.
#[cfg(test)]
pub(crate) fn run_init_in(cwd: &Path, url: &str, dry: bool) -> Result<Vec<String>, String> {
    run_init_with(cwd, url, dry, true)
}

/// The writable core, separated from argv/cwd so tests drive it directly.
/// `hub` is false in a build with no hub signing key: no pack can be verified
/// there, so the hub teaching files (which teach `xerj "<question>"`) are not
/// written — an agent taught that line would only ever get exit 5.
pub(crate) fn run_init_with(
    cwd: &Path,
    url: &str,
    dry: bool,
    hub: bool,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();

    // 1) MCP registration (.mcp.json, Claude Code project scope; other hosts
    //    read the same shape). Merge, never clobber. Cursor does not read
    //    that file: when the project already has `.cursor/`, write the same
    //    entry to `.cursor/mcp.json`.
    out.push(register_mcp(&cwd.join(".mcp.json"), url, dry)?);
    if cwd.join(".cursor").is_dir() {
        out.push(register_mcp(&cwd.join(".cursor/mcp.json"), url, dry)?);
    }

    // 2) Claude Code skills — written whenever the project (or user) uses
    //    Claude Code; the directory existing is the signal. Two files: the
    //    local-code skill, and the hub skill (when to ask the hub instead).
    let claude_dir_present = cwd.join(".claude").is_dir()
        || dirs_home()
            .map(|h| h.join(".claude").is_dir())
            .unwrap_or(false);
    if claude_dir_present {
        out.push(write_teaching_file(
            &cwd.join(".claude/skills/xerj/SKILL.md"),
            &skill_md(),
            dry,
        )?);
        if hub {
            out.push(write_teaching_file(
                &cwd.join(".claude/skills/xerj-hub/SKILL.md"),
                HUB_SKILL_MD,
                dry,
            )?);
        }
    }

    // 3) Cursor rules — only when the project already has a .cursor directory.
    //    The same two teaching files, one rule each.
    if cwd.join(".cursor").is_dir() {
        out.push(write_teaching_file(
            &cwd.join(".cursor/rules/xerj.mdc"),
            &skill_md(),
            dry,
        )?);
        if hub {
            out.push(write_teaching_file(
                &cwd.join(".cursor/rules/xerj-hub.mdc"),
                HUB_SKILL_MD,
                dry,
            )?);
        }
    }

    // 4) AGENTS.md — the cross-agent convention. Append a short section if the
    //    file exists without one; create the file if absent.
    let agents = cwd.join("AGENTS.md");
    match fs::read_to_string(&agents) {
        Ok(text) if text.contains("xerj search") || text.contains("xerj def") => {
            out.push(report("kept", &agents, dry));
        }
        Ok(text) => {
            backup(&agents, dry)?;
            write_file(&agents, &format!("{text}{}", agents_md_section()), dry)
                .map_err(|e| e.to_string())?;
            out.push(report("appended", &agents, dry));
        }
        // Unreadable but present (not UTF-8, no permission): never replace it.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!(
                "could not read {} ({e}); not touching it",
                agents.display()
            ));
        }
        Err(_) => {
            write_file(
                &agents,
                &format!("# Agent notes\n{}", agents_md_section()),
                dry,
            )
            .map_err(|e| e.to_string())?;
            out.push(report("wrote", &agents, dry));
        }
    }

    // 5) Auth note — say plainly what will happen at tool-call time (#961).
    //
    // `.mcp.json` is Claude Code *project* scope, i.e. a file meant to be
    // committed, so init deliberately writes no credential into it; auth is
    // ON by default, and the gap between "ran xerj init" and "tools work"
    // used to be a silent 401 on the first tool call. The MCP server now
    // falls back to the local admin key for loopback nodes by itself; here
    // we only tell the user which of the two worlds they are in — the key
    // itself is never printed or written.
    if xerj_common::localauth::url_is_loopback(url) {
        match xerj_common::localauth::discover_local_admin_key_for_url(url) {
            Some((_, path)) => out.push(format!(
                "  auth     MCP authenticates with the admin key at {} (local node)",
                path.display()
            )),
            None => out.push(format!(
                "  auth     no --auth given and no local admin key found; if the node at \
                 {url} runs with auth on, add \"XERJ_AUTH\": \"ApiKey <key>\" to the \
                 .mcp.json env — the key is the node's <data-dir>/admin.key"
            )),
        }
    } else {
        out.push(format!(
            "  auth     {url} is not local, so no admin key is auto-discovered for it; \
                 if the node runs with auth on, add \"XERJ_AUTH\": \"ApiKey <key>\" to \
                 the .mcp.json env — the key is the node's <data-dir>/admin.key"
        ));
    }

    Ok(out)
}

/// `HOME`, else `USERPROFILE` (native Windows usually has no `HOME`).
fn dirs_home() -> Option<PathBuf> {
    xerj_common::home::home_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// When the `.bak` cannot be written, the original is not edited: the
    /// promise is "never touch a byte we did not write without a backup".
    #[test]
    fn failed_backup_leaves_mcp_json_untouched() {
        let d = tmp();
        let original = r#"{"mcpServers":{"other":{"command":"other-bin"}}}"#;
        fs::write(d.path().join(".mcp.json"), original).unwrap();
        // A directory where the backup file must go makes the copy fail.
        fs::create_dir(d.path().join(".mcp.json.bak")).unwrap();
        let err = run_init_in(d.path(), "http://x:1", false).unwrap_err();
        assert!(err.contains("could not back up"), "{err}");
        assert_eq!(
            fs::read_to_string(d.path().join(".mcp.json")).unwrap(),
            original
        );
    }

    /// An AGENTS.md that exists but is not valid UTF-8 is never replaced by a
    /// fresh file (it used to be: any read error took the "absent" branch).
    #[test]
    fn unreadable_agents_md_is_not_replaced() {
        let d = tmp();
        let bytes = b"# notes\n\xff\xfe not utf-8\n";
        fs::write(d.path().join("AGENTS.md"), bytes).unwrap();
        let err = run_init_in(d.path(), "http://x:1", false).unwrap_err();
        assert!(err.contains("AGENTS.md"), "{err}");
        assert_eq!(fs::read(d.path().join("AGENTS.md")).unwrap(), bytes);
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
        // The pair must see ONE `HOME`: `run_init_in` reads it through
        // `dirs_home()` to decide whether the Claude skill is in scope, and
        // the auth-note `Sandbox` tests below swap `HOME` under the same
        // `ENV_LOCK`. Without the lock, a sandboxed test landing between the
        // two runs makes the first see no `~/.claude` (skill out of scope)
        // and the second see one (skill "wrote") — a scheduling-dependent
        // failure, not an idempotency failure.
        let _env = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let d = tmp();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        let first = fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        let second = fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert_eq!(first, second, "AGENTS.md must not grow on re-run");
        // "kept" for every surface; the only other line init emits is the
        // auth note (section 5), which rewrites nothing.
        assert!(
            lines
                .iter()
                .all(|l| l.contains("kept") || l.contains("auth")),
            "{lines:?}"
        );
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
        let lines = run_init_in(d.path(), "http://x:1", true).unwrap();
        assert!(!d.path().join(".mcp.json").exists());
        assert!(!d.path().join("AGENTS.md").exists());
        assert!(
            lines
                .iter()
                .any(|l| l.contains("would write") && l.contains(".mcp.json")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("would write") && l.contains("AGENTS.md")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("  wrote")),
            "a dry run must not claim it wrote: {lines:?}"
        );
    }

    #[test]
    fn cursor_rule_written_only_when_cursor_dir_exists() {
        let d = tmp();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        assert!(!d.path().join(".cursor/rules/xerj.mdc").exists());
        assert!(!d.path().join(".cursor/mcp.json").exists());
        fs::create_dir_all(d.path().join(".cursor")).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        assert!(d.path().join(".cursor/rules/xerj.mdc").exists());
        let cursor: Value =
            serde_json::from_str(&fs::read_to_string(d.path().join(".cursor/mcp.json")).unwrap())
                .unwrap();
        assert!(cursor["mcpServers"]["xerj"]["args"][0] == "mcp");
        assert!(
            lines.iter().any(|l| {
                let l = l.replace('\\', "/");
                l.contains("wrote") && l.ends_with(".cursor/mcp.json")
            }),
            "{lines:?}"
        );
    }

    // ── the hub skill (Corpus Hub v2, plan section 9) ───────────────────

    /// True when `lines` holds a report line for `action` whose path ends in
    /// `tail`. Paths are compared with `/` so the test reads the same on
    /// Windows, where `Path::display` prints backslashes.
    fn has_report(lines: &[String], action: &str, tail: &str) -> bool {
        lines.iter().any(|l| {
            let l = l.replace('\\', "/");
            l.contains(action) && l.ends_with(tail)
        })
    }

    /// `.claude/` is the signal for both skills: the hub skill lands beside the
    /// local-code skill and is a different file with different content.
    #[test]
    fn claude_projects_get_the_hub_skill_beside_the_xerj_skill() {
        let d = tmp();
        fs::create_dir_all(d.path().join(".claude")).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        let local = fs::read_to_string(d.path().join(".claude/skills/xerj/SKILL.md")).unwrap();
        let hub = fs::read_to_string(d.path().join(".claude/skills/xerj-hub/SKILL.md")).unwrap();
        assert_eq!(local, stamped(&skill_md()));
        assert_eq!(hub, stamped(HUB_SKILL_MD));
        assert_ne!(local, hub);
        assert!(
            has_report(&lines, "wrote", "skills/xerj-hub/SKILL.md"),
            "{lines:?}"
        );
    }

    /// Cursor gets the same hub text as a rule, and only when `.cursor/` exists.
    #[test]
    fn cursor_projects_get_the_hub_rule_with_the_same_text() {
        let d = tmp();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        assert!(!d.path().join(".cursor/rules/xerj-hub.mdc").exists());
        fs::create_dir_all(d.path().join(".cursor")).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join(".cursor/rules/xerj-hub.mdc")).unwrap(),
            stamped(HUB_SKILL_MD)
        );
        assert!(
            has_report(&lines, "wrote", "rules/xerj-hub.mdc"),
            "{lines:?}"
        );
    }

    /// A hub skill the user edited is never overwritten.
    #[test]
    fn an_edited_hub_skill_is_kept_not_overwritten() {
        let d = tmp();
        let hub = d.path().join(".claude/skills/xerj-hub/SKILL.md");
        fs::create_dir_all(hub.parent().unwrap()).unwrap();
        fs::write(&hub, "my own rules\n").unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        assert_eq!(fs::read_to_string(&hub).unwrap(), "my own rules\n");
        assert!(
            has_report(&lines, "kept", "skills/xerj-hub/SKILL.md"),
            "{lines:?}"
        );
    }

    /// `--dry-run` reports both hub files and writes neither.
    #[test]
    fn dry_run_reports_the_hub_files_and_writes_none() {
        let d = tmp();
        fs::create_dir_all(d.path().join(".claude")).unwrap();
        fs::create_dir_all(d.path().join(".cursor")).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", true).unwrap();
        assert!(!d.path().join(".claude/skills").exists());
        assert!(!d.path().join(".cursor/rules").exists());
        assert!(has_report(
            &lines,
            "would write",
            "skills/xerj-hub/SKILL.md"
        ));
        assert!(has_report(&lines, "would write", "rules/xerj-hub.mdc"));
        assert!(has_report(&lines, "would write", ".cursor/mcp.json"));
        assert!(!d.path().join(".cursor/mcp.json").exists());
        assert!(
            !lines.iter().any(|l| l.contains("  wrote")),
            "a dry run must not claim it wrote: {lines:?}"
        );
    }

    /// The second run rewrites nothing: all four teaching files are `kept`.
    #[test]
    fn second_run_keeps_all_four_teaching_files() {
        // `ENV_LOCK`: see `second_run_is_idempotent` — the auth note reads the
        // process cwd and HOME, which the sandboxed tests swap.
        let _env = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let d = tmp();
        fs::create_dir_all(d.path().join(".claude")).unwrap();
        fs::create_dir_all(d.path().join(".cursor")).unwrap();
        run_init_in(d.path(), "http://x:1", false).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        for tail in [
            "skills/xerj/SKILL.md",
            "skills/xerj-hub/SKILL.md",
            "rules/xerj.mdc",
            "rules/xerj-hub.mdc",
        ] {
            assert!(has_report(&lines, "kept", tail), "{tail}: {lines:?}");
        }
        assert!(has_report(&lines, "kept", ".cursor/mcp.json"), "{lines:?}");
    }

    /// The reason the file exists: the decision rule, the one line, `--def`,
    /// the safe-stop rule, and the registration per client. Deleting any of
    /// them has to fail here.
    #[test]
    fn hub_skill_carries_the_rule_the_line_and_the_registration() {
        for needed in [
            "**HUB**",
            "**LOCAL**",
            "**NEITHER**",
            r#"xerj "<question>""#,
            r#"xerj ask "<word>""#,
            "xerj ask --def <SYMBOL>",
            "Stop only the node you started",
            "Never `pkill xerj` or `killall xerj`",
            "xerj hub pull",
            "Exit 5",
            "Success: `xerj_search` is in the tool list",
            "claude mcp add",
            "root key `mcpServers`",
            "root key `servers`, not `mcpServers`",
        ] {
            assert!(
                HUB_SKILL_MD.contains(needed),
                "the hub skill lost {needed:?}"
            );
        }
    }

    /// The frontmatter a skill loader reads: a `name` that matches the
    /// directory, and a `description` that is a valid plain YAML scalar with no
    /// angle brackets (the Agent Skills spec forbids XML-like tags there).
    #[test]
    fn hub_skill_frontmatter_is_loadable() {
        let rest = HUB_SKILL_MD
            .strip_prefix("---\n")
            .expect("the skill opens with frontmatter");
        let (front, _body) = rest.split_once("\n---\n").expect("the frontmatter closes");
        fn field<'a>(front: &'a str, key: &str) -> &'a str {
            front
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .unwrap_or_else(|| panic!("frontmatter has no {key}"))
        }
        assert_eq!(field(front, "name: "), "xerj-hub");
        let description = field(front, "description: ");
        assert!(!description.is_empty() && description.len() <= 1024);
        assert!(
            !description.contains('<') && !description.contains('>'),
            "no angle brackets in the description: {description}"
        );
        assert!(
            !description.contains(": ") && !description.contains(" #"),
            "a plain YAML scalar may not contain `: ` or ` #`: {description}"
        );
    }

    /// llms.txt study rule 7 (`docs/research/llms-txt-2026-09/README.md`): no
    /// line over about 300 characters in text an agent reads.
    #[test]
    fn hub_texts_have_no_line_over_300_characters() {
        let printed = mcp_registration("/usr/local/bin/xerj", "http://10.0.0.7:9200");
        for (name, text) in [
            ("hub skill", HUB_SKILL_MD),
            ("mcp registration", printed.as_str()),
        ] {
            for (i, line) in text.lines().enumerate() {
                let n = line.chars().count();
                assert!(n <= 300, "{name} line {} is {n} characters", i + 1);
            }
        }
    }

    /// What init prints and what the skill teaches are one registration, not
    /// two that drift: the same Claude Code command and the same root keys.
    #[test]
    fn printed_registration_and_hub_skill_agree() {
        assert!(HUB_SKILL_MD
            .contains(r#"claude mcp add --scope user xerj -- "$(command -v xerj)" mcp"#));
        let printed = mcp_registration("/usr/local/bin/xerj", DEFAULT_URL);
        assert!(
            printed.contains("claude mcp add --scope user xerj -- "),
            "{printed}"
        );
        for key in ["mcpServers", "servers", "stdio"] {
            assert!(HUB_SKILL_MD.contains(key), "skill: {key}");
            assert!(printed.contains(key), "printed: {key}");
        }
        let success = format!("`{MCP_SUCCESS_TOOL}` is in the tool list");
        assert!(HUB_SKILL_MD.contains(&success));
        assert!(printed.contains(&success), "{printed}");
        let closing = closing_message("/usr/local/bin/xerj", DEFAULT_URL, true);
        assert!(closing.contains(&printed));
        assert!(closing.contains(r#"xerj "<question>""#));
        assert!(closing.contains("xerj autoindex <folder> --prefix <name>"));
        assert!(closing.contains("default `ax` prefix"));
    }

    /// The printed Claude Code line is pasteable in any shell: it carries the
    /// absolute path init resolved (the one `.mcp.json` got), not a
    /// `$(command -v xerj)` that only a POSIX shell expands, and it registers
    /// the same node `.mcp.json` points at.
    #[test]
    fn printed_claude_line_uses_this_binary_and_this_url() {
        let printed = mcp_registration("/opt/x y/xerj", DEFAULT_URL);
        assert!(!printed.contains("$("), "{printed}");
        assert!(
            !printed.contains("XERJ_URL"),
            "default URL needs no env: {printed}"
        );
        let quoted = shell_quote("/opt/x y/xerj");
        assert!(
            printed.contains(&format!("claude mcp add --scope user xerj -- {quoted} mcp")),
            "{printed}"
        );
        let custom = mcp_registration("/opt/xerj", "http://10.0.0.7:9200");
        let url = shell_quote("http://10.0.0.7:9200");
        assert!(
            custom.contains(&format!(
                "claude mcp add --scope user xerj -e XERJ_URL={url} -- "
            )),
            "{custom}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_quote_survives_a_single_quote_on_unix() {
        assert_eq!(shell_quote("/a b/xerj"), "'/a b/xerj'");
        assert_eq!(shell_quote("/it's/xerj"), r"'/it'\''s/xerj'");
    }

    /// The hub tools `xerj mcp` serves (plan step 7; `xerj-mcp/src/hub.rs`,
    /// whose own test pins the same three names to `tool_specs()`). The skill
    /// names each, says a tool call never downloads a pack, and promises no
    /// other `xerj_hub_*` tool; the printed registration still proves itself
    /// with `xerj_search`.
    #[test]
    fn the_hub_skill_names_exactly_the_hub_tools_mcp_serves() {
        const SERVED: [&str; 3] = ["xerj_hub_ask", "xerj_hub_def", "xerj_hub_find"];
        for name in SERVED {
            assert!(
                HUB_SKILL_MD.contains(name),
                "the skill does not name {name}"
            );
        }
        let mut rest = HUB_SKILL_MD;
        while let Some(at) = rest.find("xerj_hub_") {
            let tail = &rest[at..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len());
            let named = &tail[..end];
            assert!(
                SERVED.contains(&named),
                "the skill names an unserved tool {named}"
            );
            rest = &tail[end..];
        }
        assert!(HUB_SKILL_MD.contains("never downloads one"));
        assert!(HUB_SKILL_MD.contains("run `xerj hub sync` in a shell once"));
        assert!(!HUB_SKILL_MD.contains("has no hub tool"));
        let printed = mcp_registration("/usr/local/bin/xerj", DEFAULT_URL);
        assert!(printed.contains("xerj_search"));
    }

    /// `--help` lists every file init can write. The hub pair is listed only
    /// when this build can verify hub packs; otherwise the help must not
    /// teach `xerj "<question>"`, which exits 5.
    #[test]
    fn help_lists_every_file_init_writes() {
        let with_hub = usage(true);
        for file in [
            ".mcp.json",
            ".cursor/mcp.json",
            ".claude/skills/xerj/SKILL.md",
            ".claude/skills/xerj-hub/SKILL.md",
            ".cursor/rules/xerj.mdc",
            ".cursor/rules/xerj-hub.mdc",
            "AGENTS.md",
        ] {
            assert!(with_hub.contains(file), "--help does not list {file}");
        }
        assert!(
            with_hub.contains("xerj autoindex <folder> --prefix <name>"),
            "{with_hub}"
        );
        assert!(with_hub.contains("default `ax` prefix"), "{with_hub}");
        assert!(with_hub.contains("asks the hub"), "{with_hub}");

        let no_hub = usage(false);
        for file in [
            ".mcp.json",
            ".claude/skills/xerj/SKILL.md",
            ".cursor/rules/xerj.mdc",
            "AGENTS.md",
        ] {
            assert!(no_hub.contains(file), "--help does not list {file}");
        }
        assert!(
            !no_hub.contains("xerj-hub"),
            "a no-key build must not list hub teaching files it will not write:\n{no_hub}"
        );
        assert!(
            !no_hub.contains(r#"asks the hub"#),
            "a no-key build must not say the hub answers:\n{no_hub}"
        );
        assert!(
            no_hub.contains("exits 5") && no_hub.contains("xerj code"),
            "{no_hub}"
        );
    }

    // ── the version marker: upgrade only what init wrote and nobody edited ──

    /// The marker sits after the frontmatter (a loader reads `---` on line 1),
    /// and removing it gives back the text exactly.
    #[test]
    fn stamp_goes_after_the_frontmatter_and_reads_back_pristine() {
        for text in [skill_md(), HUB_SKILL_MD.to_string()] {
            let s = stamped(&text);
            assert!(s.starts_with("---\n"), "{s}");
            assert_eq!(read_stamp(&s), Stamp::Pristine(INIT_FILES_VERSION));
            let (front, _) = s[4..].split_once("\n---\n").unwrap();
            assert!(!front.contains(MARKER_PREFIX));
        }
        assert!(stamped("plain\n").starts_with(MARKER_PREFIX));
        assert_eq!(read_stamp("my own rules\n"), Stamp::None);
    }

    /// `text` as an init of generation `v` would have written it.
    fn stamped_as(v: u32, text: &str) -> String {
        stamped(text).replacen(
            &format!("{MARKER_PREFIX}{INIT_FILES_VERSION} "),
            &format!("{MARKER_PREFIX}{v} "),
            1,
        )
    }

    /// Case 1: an older marker on an unedited file — upgraded, old text in
    /// `.bak`.
    #[test]
    fn an_unedited_file_from_an_older_init_is_upgraded_with_a_backup() {
        let d = tmp();
        let hub = d.path().join(".claude/skills/xerj-hub/SKILL.md");
        fs::create_dir_all(hub.parent().unwrap()).unwrap();
        let old = stamped_as(0, "---\nname: xerj-hub\n---\n\nold text\n");
        fs::write(&hub, &old).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        assert!(
            has_report(&lines, "upgraded", "skills/xerj-hub/SKILL.md"),
            "{lines:?}"
        );
        assert_eq!(fs::read_to_string(&hub).unwrap(), stamped(HUB_SKILL_MD));
        assert_eq!(
            fs::read_to_string(hub.with_extension("md.bak")).unwrap(),
            old
        );
    }

    /// The upgrade is fail-closed like `.mcp.json`: no backup, no edit.
    #[test]
    fn a_failed_backup_blocks_the_upgrade() {
        let d = tmp();
        let hub = d.path().join(".claude/skills/xerj-hub/SKILL.md");
        fs::create_dir_all(hub.with_extension("md.bak")).unwrap();
        let old = stamped_as(0, "old text\n");
        fs::write(&hub, &old).unwrap();
        let err = run_init_in(d.path(), "http://x:1", false).unwrap_err();
        assert!(err.contains("could not back up"), "{err}");
        assert_eq!(fs::read_to_string(&hub).unwrap(), old);
    }

    /// Case 2: an older marker on a file the user edited — kept, one line
    /// saying why, no backup made.
    #[test]
    fn an_edited_file_from_an_older_init_is_kept_with_a_notice() {
        let d = tmp();
        let hub = d.path().join(".claude/skills/xerj-hub/SKILL.md");
        fs::create_dir_all(hub.parent().unwrap()).unwrap();
        let edited = format!("{}my addition\n", stamped_as(0, "old text\n"));
        fs::write(&hub, &edited).unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        assert_eq!(fs::read_to_string(&hub).unwrap(), edited);
        assert!(!hub.with_extension("md.bak").exists());
        let notice: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("xerj-hub") && l.contains("edited since xerj init"))
            .collect();
        assert_eq!(notice.len(), 1, "{lines:?}");
        assert!(notice[0].contains("kept"), "{lines:?}");
    }

    /// Case 3: no marker — the user's file, kept untouched (also covered for
    /// plain text by `an_edited_hub_skill_is_kept_not_overwritten`). Even the
    /// exact current text without a marker is not claimed.
    #[test]
    fn a_file_without_a_marker_is_user_owned() {
        let d = tmp();
        let rule = d.path().join(".cursor/rules/xerj-hub.mdc");
        fs::create_dir_all(rule.parent().unwrap()).unwrap();
        fs::write(&rule, "old unmarked text\n").unwrap();
        let lines = run_init_in(d.path(), "http://x:1", false).unwrap();
        assert_eq!(fs::read_to_string(&rule).unwrap(), "old unmarked text\n");
        assert!(!rule.with_extension("mdc.bak").exists());
        assert!(
            has_report(&lines, "kept", "rules/xerj-hub.mdc"),
            "{lines:?}"
        );
    }

    /// The generated AGENTS.md section used to teach
    /// `xerj search "<words>"` with no `--prefix`, so an agent on a node whose
    /// repos were indexed under their own prefixes ran the bare command, hit
    /// the default `ax-*` pattern, and found nothing (field report
    /// 2026-10-15-reference-coding-installed-xerj-indexed-this-repo). The
    /// section must show the flag on the index step AND on the query step.
    #[test]
    fn agents_md_section_shows_prefix_on_index_and_query() {
        let text = agents_md_section();
        assert!(
            text.contains("xerj autoindex <folder> --prefix <name>"),
            "AGENTS.md must show --prefix on the index step:\n{text}"
        );
        assert!(
            text.contains("same `--prefix <name>`"),
            "AGENTS.md must tell search/def to reuse the prefix:\n{text}"
        );
        assert!(
            text.contains("default `ax` prefix"),
            "AGENTS.md must name what a bare search reads:\n{text}"
        );
    }

    /// The hub skill's LOCAL bullet used to teach a bare `xerj autoindex` and
    /// a bare `xerj search` / `xerj def`, the same omission as the project
    /// skill (#1580). An agent that followed it searched the default `ax`
    /// prefix.
    #[test]
    fn hub_skill_shows_prefix_on_index_and_query() {
        assert!(
            HUB_SKILL_MD.contains("xerj autoindex <folder> --prefix <name>"),
            "{HUB_SKILL_MD}"
        );
        assert!(HUB_SKILL_MD.contains("`--prefix <name>`"), "{HUB_SKILL_MD}");
        assert!(
            HUB_SKILL_MD.contains("default `ax` prefix"),
            "{HUB_SKILL_MD}"
        );
    }

    /// The skill used to teach `xerj search` / `xerj def` with no `--prefix`,
    /// so an agent on a node whose repos sit under their own prefixes searched
    /// the default `ax-*` and found nothing (#1580).
    #[test]
    fn skill_shows_prefix_on_index_and_query() {
        let text = skill_md();
        assert!(
            text.contains("xerj autoindex <folder> --prefix <name>"),
            "{text}"
        );
        assert!(text.contains("same `--prefix <name>`"), "{text}");
        assert!(text.contains("default `ax` prefix"), "{text}");
    }

    /// A text change without a version bump would leave every pristine older
    /// file un-upgraded. Changing either text fails here until
    /// `INIT_FILES_VERSION` is bumped and these hashes are re-pinned.
    #[test]
    fn teaching_texts_are_pinned_to_the_init_files_version() {
        assert_eq!(
            (
                INIT_FILES_VERSION,
                content_hash(&skill_md()).as_str(),
                content_hash(HUB_SKILL_MD).as_str()
            ),
            (3, "c59537bc1064a30f", "6040c97600edc004"),
            "a teaching text changed: bump INIT_FILES_VERSION and re-pin"
        );
    }

    // ── auth note (#961) ────────────────────────────────────────────────

    /// The auth note reads the working directory and `HOME` (that is what
    /// admin-key discovery does), so its tests sandbox both — serialised,
    /// because they are per-process state.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Sandbox {
        _lock: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
        previous_dir: PathBuf,
        previous_home: Option<std::ffi::OsString>,
    }

    impl Sandbox {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let previous_dir = std::env::current_dir().unwrap();
            let previous_home = std::env::var_os("HOME");
            std::env::set_current_dir(dir.path()).unwrap();
            std::env::set_var("HOME", dir.path().join("home"));
            Self {
                _lock: lock,
                dir,
                previous_dir,
                previous_home,
            }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous_dir).unwrap();
            match &self.previous_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// The happy path of #961: a local node's key exists, init says the MCP
    /// server will use it — and the secret still goes nowhere near the
    /// committable `.mcp.json`.
    #[test]
    fn auth_note_names_the_discovered_key_and_writes_no_secret() {
        let sandbox = Sandbox::new();
        sandbox.write("data/admin.key", "local-admin-key\n");
        let lines = run_init_in(sandbox.dir.path(), "http://localhost:9200", false).unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("admin key at") && l.ends_with("data/admin.key (local node)")),
            "the note must name the file the MCP server will use: {lines:?}"
        );
        let mcp = fs::read_to_string(sandbox.dir.path().join(".mcp.json")).unwrap();
        assert!(
            !mcp.contains("local-admin-key") && !mcp.contains("XERJ_AUTH"),
            "a credential must never be written into a committable file: {mcp}"
        );
    }

    /// No key on disk: the note must say the manual step plainly instead of
    /// letting the first tool call be the user's first 401.
    #[test]
    fn auth_note_gives_the_manual_step_when_no_key_exists() {
        let sandbox = Sandbox::new();
        let lines = run_init_in(sandbox.dir.path(), "http://localhost:9200", false).unwrap();
        let note = lines
            .iter()
            .find(|l| l.contains("auth"))
            .expect("the auth note is always present");
        assert!(
            note.contains("XERJ_AUTH") && note.contains("admin.key"),
            "the fallback note must name the env var and the key file: {note}"
        );
        assert!(
            note.contains("http://localhost:9200") && !note.contains("{url}"),
            "the note names the node it means, not a literal placeholder: {note}"
        );
    }

    /// A build with no hub signing key can verify no pack, so `xerj ask` only
    /// ever exits 5 there: init must not teach it. No hub skill, no hub rule,
    /// no `xerj "<question>"` line; the local skill and MCP wiring are intact.
    #[test]
    fn a_build_without_a_hub_key_does_not_teach_the_hub() {
        let d = tmp();
        fs::create_dir_all(d.path().join(".claude")).unwrap();
        fs::create_dir_all(d.path().join(".cursor")).unwrap();
        let lines = run_init_with(d.path(), "http://x:1", false, false).unwrap();
        assert!(d.path().join(".claude/skills/xerj/SKILL.md").exists());
        assert!(d.path().join(".cursor/rules/xerj.mdc").exists());
        assert!(d.path().join(".mcp.json").exists());
        assert!(
            !d.path().join(".claude/skills/xerj-hub").exists(),
            "{lines:?}"
        );
        assert!(
            !d.path().join(".cursor/rules/xerj-hub.mdc").exists(),
            "{lines:?}"
        );
        let closing = closing_message("/usr/local/bin/xerj", DEFAULT_URL, false);
        assert!(!closing.contains(r#"xerj "<question>""#), "{closing}");
        assert!(
            closing.contains("xerj autoindex <folder> --prefix <name>"),
            "{closing}"
        );
        assert!(closing.contains("default `ax` prefix"), "{closing}");
    }

    /// A non-loopback URL never triggers discovery, even with a local key
    /// sitting right there — reading it would only encourage sending it
    /// off-box.
    #[test]
    fn remote_url_gets_no_discovery_even_with_a_local_key_present() {
        let sandbox = Sandbox::new();
        sandbox.write("data/admin.key", "local-admin-key\n");
        let lines =
            run_init_in(sandbox.dir.path(), "http://search.example.com:9200", false).unwrap();
        let note = lines
            .iter()
            .find(|l| l.contains("auth"))
            .expect("the auth note is always present");
        assert!(
            note.contains("XERJ_AUTH") && !note.contains("admin key at"),
            "a remote node gets the manual step, never a local key: {note}"
        );
    }
}
