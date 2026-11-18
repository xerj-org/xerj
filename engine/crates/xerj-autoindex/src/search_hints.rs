//! Opt-in, advisory-only Claude Code PreToolUse / Gemini CLI AfterTool hints (#2642).
//! Host contract: https://code.claude.com/docs/en/hooks#pretooluse-decision-control.
//! Gemini: https://geminicli.com/docs/hooks/reference/#aftertool (milliseconds).
//! Return additionalContext only: never permissionDecision, updatedInput or exit 2.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

// Reuse the session hook's bounded, no-symlink, private/atomic file operations.
use crate::session_memory::{atomic, digest, encode, lock, loopback, private_dir, quote, read};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Host {
    #[default]
    Claude,
    Gemini,
}

impl Host {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "claude" => Ok(Self::Claude),
            "gemini" => Ok(Self::Gemini),
            _ => anyhow::bail!("unknown search hint host '{value}'; use claude or gemini"),
        }
    }
    const fn directory(self) -> &'static str {
        match self {
            Self::Claude => ".claude/xerj-hints",
            Self::Gemini => ".gemini/xerj-hints",
        }
    }
    const fn settings(self) -> &'static str {
        match self {
            Self::Claude => ".claude/settings.json",
            Self::Gemini => ".gemini/settings.json",
        }
    }
    fn event(self) -> &'static str {
        match self {
            Self::Claude => "PreToolUse",
            Self::Gemini => "AfterTool",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Gemini => "Gemini CLI",
        }
    }
    fn flag(self) -> &'static str {
        match self {
            Self::Claude => "",
            Self::Gemini => " --host gemini",
        }
    }
    fn tool(self, name: &str) -> Option<&'static str> {
        match (self, name) {
            (Self::Claude, "Read") | (Self::Gemini, "read_file") => Some("Read"),
            (Self::Claude, "Grep") | (Self::Gemini, "grep_search" | "search_file_content") => {
                Some("Grep")
            }
            (Self::Claude, "Glob") | (Self::Gemini, "glob") => Some("Glob"),
            _ => None,
        }
    }
}

#[cfg(test)]
const DIRECTORY: &str = Host::Claude.directory();
#[cfg(test)]
const SETTINGS: &str = Host::Claude.settings();
const LIMIT: u64 = 64 * 1024;
const INTERVAL: u64 = 10;
const MAX_SESSIONS: usize = 64;

#[derive(Serialize, Deserialize)]
struct Config {
    version: u32,
    enabled: bool,
    project: PathBuf,
    url: String,
    group: Value,
}

fn group(host: Host, project: &Path, binary: &Path) -> Result<Value> {
    let project = project.to_str().context("project path must be UTF-8")?;
    let binary = binary.to_str().context("binary path must be UTF-8")?;
    ensure!(
        !project.chars().chain(binary.chars()).any(char::is_control),
        "control characters in hook paths"
    );
    let (matcher, timeout) = match host {
        Host::Claude => ("Grep|Glob|Read", 2),
        // Gemini regex matchers are unanchored by default. Do not match MCP tools.
        Host::Gemini => ("^(grep_search|search_file_content|glob|read_file)$", 2000),
    };
    Ok(json!({"matcher":matcher, "hooks":[{
        "type":"command", "command":format!("{} init hook --project {}{}", quote(binary), quote(project), host.flag()),
        "timeout":timeout
    }]}))
}

pub(crate) fn configure(
    host: Host,
    project: &Path,
    url: &str,
    dry: bool,
    remove: bool,
    binary: &Path,
) -> Result<Vec<String>> {
    configure_with(host, project, url, (dry, remove), binary, atomic)
}

fn configure_with(
    host: Host,
    project: &Path,
    url: &str,
    action: (bool, bool),
    binary: &Path,
    publish_settings: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<Vec<String>> {
    let (dry, remove) = action;
    ensure!(
        cfg!(unix),
        "search hint hooks currently support macOS and Linux only"
    );
    let project = project.canonicalize()?;
    loopback(url)?;
    let url = reqwest::Url::parse(url)?.to_string();
    let dir = project.join(host.directory());
    let config_path = dir.join("config.json");
    let path = project.join(host.settings());
    let old = read(&path, 1024 * 1024)?;
    let old_config = read(&config_path, LIMIT)?;
    let previous: Option<Config> = old_config
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?;
    if let Some(p) = &previous {
        ensure!(
            p.version == 1 && p.project == project,
            "foreign or unsupported search hint configuration"
        );
    }
    let mut settings: Value = old
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or(json!({}));
    ensure!(
        settings.is_object(),
        "{} settings must be an object; not touching it",
        host.label()
    );
    if host == Host::Gemini && !remove {
        ensure!(settings.pointer("/hooksConfig/enabled") != Some(&json!(false)),
            "Gemini hooks are disabled in this project's settings; enable hooksConfig.enabled before installing search hints");
    }
    if remove && previous.as_ref().is_none_or(|p| !p.enabled) {
        return Ok(vec![
            "search hints already disabled; backups and counters kept".into(),
        ]);
    }
    let wanted = group(host, &project, binary)?;
    let root = settings.as_object_mut().expect("checked object");
    let hooks = root
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .context("hooks must be an object")?;
    let groups = hooks
        .entry(host.event())
        .or_insert(json!([]))
        .as_array_mut()
        .with_context(|| format!("{} must be an array", host.event()))?;
    let owned = previous.as_ref().filter(|p| p.enabled);
    if let Some(p) = owned {
        ensure!(groups.iter().filter(|g| *g == &p.group).count() == 1,
            "managed search hint was edited or removed; restore its group from config.json before retrying");
    }
    ensure!(
        groups.iter().filter(|g| *g == &wanted).count()
            == usize::from(owned.is_some_and(|p| p.group == wanted)),
        "matching search hint exists without unique ownership; not touching it"
    );
    if let Some(p) = owned {
        groups.retain(|g| g != &p.group);
    }
    if !remove {
        groups.push(wanted.clone());
    }
    if groups.is_empty() {
        hooks.remove(host.event());
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    let next = Config {
        version: 1,
        enabled: !remove,
        project: project.clone(),
        url: url.trim_end_matches('/').into(),
        group: wanted,
    };
    let bytes = encode(&settings)?;
    let config_bytes = encode(&next)?;
    ensure!(
        bytes.len() <= 1024 * 1024 && config_bytes.len() as u64 <= LIMIT,
        "search hint configuration exceeds its read budget; not touching settings"
    );
    let lines = vec![
        format!(
            "{} {} search hints ({}; loopback only)",
            match (dry, remove) {
                (true, true) => "would disable",
                (true, false) => "would configure",
                (false, true) => "disabled",
                (false, false) => "enabled",
            },
            host.label(),
            host.event()
        ),
        format!(
            "Use xerj init --hooks{} --remove to disable; private backups/counters remain in {}/",
            host.flag(),
            host.directory()
        ),
    ];
    if dry {
        return Ok(lines);
    }
    fs::create_dir_all(path.parent().context("missing settings parent")?)?;
    private_dir(&dir)?;
    let _lease = lock(&dir.join("state.lock"))?;
    // Serialise installers and runtime counters; refuse a stale preflight.
    ensure!(
        read(&path, 1024 * 1024)? == old && read(&config_path, LIMIT)? == old_config,
        "search hint settings changed; retry"
    );
    atomic(&dir.join(".gitignore"), b"*\n!.gitignore\n")?;
    if old.as_deref() == Some(&bytes) && old_config.as_deref() == Some(&config_bytes) {
        return Ok(lines);
    }
    if let Some(old) = &old {
        let backup = dir.join(format!("settings-{}.bak", &digest(old)[..16]));
        match read(&backup, 1024 * 1024)? {
            Some(found) => ensure!(found == *old, "search hint backup collision"),
            None => {
                atomic(&backup, old)?;
            }
        }
    }
    atomic(&config_path, &config_bytes)?;
    if let Err(error) = publish_settings(&path, &bytes) {
        match old_config {
            Some(old) => atomic(&config_path, &old)?,
            None => fs::remove_file(&config_path)?,
        }
        return Err(error);
    }
    Ok(lines)
}

#[derive(Default, Serialize, Deserialize)]
struct Counters {
    sessions: Vec<Session>,
}
#[derive(Serialize, Deserialize)]
struct Session {
    id: String,
    calls: u64,
    reads: u64,
    last_hint: Option<u64>,
}
impl Counters {
    fn visit(&mut self, id: &str, tool: &str) -> &mut Session {
        let i = if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
            i
        } else {
            if self.sessions.len() == MAX_SESSIONS {
                self.sessions.remove(0);
            }
            self.sessions.push(Session {
                id: id.into(),
                calls: 0,
                reads: 0,
                last_hint: None,
            });
            self.sessions.len() - 1
        };
        let s = &mut self.sessions[i];
        s.calls = s.calls.saturating_add(1);
        if tool == "Read" {
            s.reads = s.reads.saturating_add(1);
        }
        s
    }
}
impl Session {
    fn eligible(&self, tool: &str) -> bool {
        (tool != "Read" || self.reads >= 3)
            && self
                .last_hint
                .is_none_or(|n| self.calls.saturating_sub(n) >= INTERVAL)
    }
}

// Root is not a mapped catalog field (catalog.rs:660); filter the bounded
// run documents client-side, as search.rs:643 does for legacy prefix fields.
// Only latest cohorts with unambiguous root ownership may supply a prefix.
fn indexed_prefix(value: &Value, project: &Path) -> Option<String> {
    if value["timed_out"].as_bool() == Some(true)
        || value
            .pointer("/_shards/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            != 0
    {
        return None;
    }
    let hits = value.pointer("/hits/hits")?.as_array()?;
    if value.pointer("/hits/total/relation")?.as_str()? != "eq"
        || value.pointer("/hits/total/value")?.as_u64()? > hits.len() as u64
    {
        return None;
    }
    let runs: Vec<Value> = hits.iter().map(|h| h["_source"].clone()).collect();
    let root = project.to_str()?;
    let mut prefixes: Vec<&str> = runs
        .iter()
        .filter(|r| {
            r["doc_kind"] == "run"
                && r["root"] == root
                && r["records_total"].as_u64().unwrap_or(0) > 0
        })
        .filter_map(|r| r["prefix"].as_str())
        .filter(|p| valid_prefix(p))
        .collect();
    prefixes.sort_unstable();
    prefixes.dedup();
    prefixes
        .into_iter()
        .find(|p| {
            let latest = runs
                .iter()
                .filter(|r| r["doc_kind"] == "run" && r["prefix"] == *p)
                .filter_map(|r| r["started"].as_str())
                .max();
            latest.is_some()
                && crate::catalog::foreign_prefix_roots(&runs, p, root).is_empty()
                && runs
                    .iter()
                    .filter(|r| {
                        r["doc_kind"] == "run"
                            && r["prefix"] == *p
                            && r["started"].as_str() == latest
                    })
                    .all(|r| r["records_total"].as_u64().unwrap_or(0) > 0)
        })
        .map(str::to_owned)
}
fn valid_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.len() <= 128
        && prefix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}
fn probe(config: &Config) -> Result<Option<String>> {
    let mut headers = reqwest::header::HeaderMap::new();
    // An explicit runtime key wins; discovery must belong to this node.
    let auth = std::env::var("XERJ_AUTH")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("XERJ_API_KEY")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| format!("ApiKey {s}"))
        })
        .or_else(|| {
            let key =
                xerj_common::localauth::discover_local_admin_key_for_url_with_origin(&config.url)?;
            key.foreign_warning("XERJ_AUTH")
                .is_none()
                .then(|| format!("ApiKey {}", key.key))
        });
    if let Some(auth) = auth {
        headers.insert(reqwest::header::AUTHORIZATION, auth.parse()?);
    }
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(100))
        .timeout(Duration::from_millis(300))
        .default_headers(headers)
        .build()?;
    let response = client
        .post(format!("{}/autoindex-catalog/_search", config.url))
        .json(&json!({
            "size":200, "track_total_hits":true, "query":{"term":{"doc_kind":"run"}},
            "_source":["doc_kind","root","prefix","started","records_total"]
        }))
        .send()?
        .error_for_status()?;
    ensure!(
        response.status().is_success(),
        "catalog did not return success"
    );
    let mut bytes = Vec::new();
    response.take(256 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 256 * 1024, "catalog response over budget");
    Ok(indexed_prefix(
        &serde_json::from_slice(&bytes)?,
        &config.project,
    ))
}

fn handle(host: Host, project: &Path, input: &[u8]) -> Result<Option<Value>> {
    handle_with(host, project, input, probe)
}
fn handle_with(
    host: Host,
    project: &Path,
    input: &[u8],
    probe: impl FnOnce(&Config) -> Result<Option<String>>,
) -> Result<Option<Value>> {
    ensure!(
        cfg!(unix) && input.len() as u64 <= LIMIT,
        "unsupported hook or oversized input"
    );
    let event: Value = serde_json::from_slice(input)?;
    ensure!(event["hook_event_name"] == host.event(), "wrong event");
    let tool = host
        .tool(event["tool_name"].as_str().context("missing tool")?)
        .context("unmatched tool")?;
    let sid = event["session_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .context("invalid session id")?;
    let project = project.canonicalize()?;
    let cwd = Path::new(event["cwd"].as_str().context("missing cwd")?).canonicalize()?;
    ensure!(cwd == project, "different working directory");
    let dir = project.join(host.directory());
    let config: Config =
        serde_json::from_slice(&read(&dir.join("config.json"), LIMIT)?.context("not configured")?)?;
    ensure!(
        config.version == 1 && config.enabled && config.project == project,
        "disabled or foreign configuration"
    );
    loopback(&config.url)?;
    private_dir(&dir)?;
    let _lease = lock(&dir.join("state.lock"))?;
    // Recheck under the installer lock, so removal cannot race a late hint.
    let current = read(&dir.join("config.json"), LIMIT)?.context("not configured")?;
    ensure!(current == encode(&config)?, "configuration changed");
    let path = dir.join("counters.json");
    let mut counters: Counters = read(&path, LIMIT)?
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_default();
    ensure!(
        counters.sessions.len() <= MAX_SESSIONS,
        "oversized counters"
    );
    let session = counters.visit(&digest(sid.as_bytes()), tool);
    let prefix = if session.eligible(tool) {
        probe(&config).ok().flatten()
    } else {
        None
    };
    let endpoint = quote(
        reqwest::Url::parse(&config.url)?
            .as_str()
            .trim_end_matches('/'),
    );
    let output = prefix.map(|prefix| {
        session.last_hint = Some(session.calls);
        json!({"hookSpecificOutput":{"hookEventName":host.event(), "additionalContext":format!(
            "This folder has an indexed XERJ corpus: try `xerj search --prefix {prefix} --url {endpoint} \"<words>\"` or `xerj def --prefix {prefix} --url {endpoint} \"<symbol>\"` for cited passages.")}})
    });
    atomic(&path, &encode(&counters)?)?;
    Ok(output)
}

/// Internal command, deliberately dispatched before node/runtime setup.
/// Malformed input, unavailable node, lock contention and I/O all abstain silently.
pub fn run(args: &[String]) -> i32 {
    let result = (|| -> Result<Option<Value>> {
        ensure!(
            matches!(args.len(), 2 | 4) && args[0] == "--project",
            "invalid arguments"
        );
        let host = if args.len() == 4 {
            ensure!(args[2] == "--host", "invalid host argument");
            Host::parse(&args[3])?
        } else {
            Host::Claude
        };
        let mut bytes = Vec::new();
        std::io::stdin().take(LIMIT + 1).read_to_end(&mut bytes)?;
        handle(host, Path::new(&args[1]), &bytes)
    })();
    if let Ok(Some(value)) = result {
        use std::io::Write;
        let _ = writeln!(std::io::stdout().lock(), "{value}");
    }
    0
}

#[cfg(all(test, unix))]
mod tests;
