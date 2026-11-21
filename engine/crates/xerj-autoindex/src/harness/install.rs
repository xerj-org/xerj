//! `xerj hook install|uninstall` for Claude Code. Merges one managed group
//! per event into settings JSON, keeps a private backup of the previous
//! file, and migrates the older `xerj init --hooks` PreToolUse hint away.
use super::profile::{atomic, hash};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const MARK: &str = " hook run ";

pub struct Plan {
    pub lines: Vec<String>,
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn settings_path(scope: &str, project: &Path) -> Result<PathBuf> {
    match scope {
        "user" => {
            let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
                .context("HOME is not set")?;
            Ok(dir.join("settings.json"))
        }
        "project" => Ok(project.join(".claude").join("settings.local.json")),
        other => bail!("unknown --scope '{other}'; use user or project"),
    }
}

fn ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains(MARK) && c.contains("--host claude"))
            })
        })
}

pub fn groups(binary: &Path, event: &str) -> Result<Vec<(&'static str, Value)>> {
    let bin = binary.to_str().context("binary path must be UTF-8")?;
    ensure!(
        !bin.chars().any(char::is_control),
        "control characters in binary path"
    );
    let cmd = |sub: &str| format!("{} hook run {sub} --host claude", quote(bin));
    let speak = match event {
        "batch" => (
            "PostToolBatch",
            json!({"hooks":[{"type":"command","command":cmd("batch"),"timeout":2}]}),
        ),
        "post" => (
            "PostToolUse",
            json!({"matcher":"Bash|Grep|Glob|Read|Edit|Write|MultiEdit","hooks":[{"type":"command","command":cmd("post --speak"),"timeout":2}]}),
        ),
        other => bail!("unknown --event '{other}'; use batch or post"),
    };
    let end = json!({"hooks":[{"type":"command","command":cmd("end"),"timeout":3}]});
    Ok(vec![
        speak,
        ("SessionEnd", end.clone()),
        ("SubagentStop", end),
    ])
}

/// Returns the new settings value (pure; tested without touching disk).
pub fn merge(mut settings: Value, wanted: &[(&'static str, Value)], remove: bool) -> Result<Value> {
    ensure!(
        settings.is_object(),
        "settings must be a JSON object; not touching it"
    );
    let root = settings.as_object_mut().expect("checked");
    let hooks = root.entry("hooks").or_insert(json!({}));
    let hooks = hooks
        .as_object_mut()
        .context("settings.hooks must be an object")?;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for ev in events {
        if let Some(arr) = hooks.get_mut(&ev).and_then(Value::as_array_mut) {
            arr.retain(|g| !ours(g));
            if arr.is_empty() {
                hooks.remove(&ev);
            }
        }
    }
    if !remove {
        for (ev, g) in wanted {
            let arr = hooks.entry(ev.to_string()).or_insert(json!([]));
            arr.as_array_mut()
                .with_context(|| format!("hooks.{ev} must be an array"))?
                .push(g.clone());
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(settings)
}

fn migrate_old_hint(project: &Path, binary: &Path, dry: bool, lines: &mut Vec<String>) {
    let cfg = project.join(".claude/xerj-hints/config.json");
    let Ok(bytes) = fs::read(&cfg) else { return };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    if v.get("enabled") != Some(&json!(true)) {
        return;
    }
    let url = v
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("http://127.0.0.1:9200/")
        .to_string();
    let url = if url.ends_with('/') {
        url
    } else {
        format!("{url}/")
    };
    match crate::search_hints::configure(
        crate::search_hints::Host::Claude,
        project,
        &url,
        dry,
        true,
        binary,
    ) {
        Ok(out) => lines.extend(out.into_iter().map(|l| format!("old search hint: {l}"))),
        Err(e) => lines.push(format!("old search hint left in place: {e}")),
    }
}

pub fn run(
    state_root: &Path,
    scope: &str,
    project: &Path,
    mode: &str,
    event: &str,
    remove: bool,
    dry: bool,
) -> Result<Plan> {
    let binary = std::env::current_exe()?;
    let path = settings_path(scope, project)?;
    let old = match fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let current: Value = match &old {
        Some(b) if !b.iter().all(u8::is_ascii_whitespace) => {
            serde_json::from_slice(b).context("settings is not valid JSON; not touching it")?
        }
        _ => json!({}),
    };
    let wanted = groups(&binary, event)?;
    let next = merge(current, &wanted, remove)?;
    let mut bytes = serde_json::to_vec_pretty(&next)?;
    bytes.push(b'\n');
    let mut lines = vec![format!(
        "{} xerj hook ({}) in {}",
        match (dry, remove) {
            (true, true) => "would remove",
            (true, false) => "would install",
            (false, true) => "removed",
            (false, false) => "installed",
        },
        if remove {
            "all events".to_string()
        } else {
            format!("{} + SessionEnd/SubagentStop, mode {mode}", wanted[0].0)
        },
        path.display()
    )];
    if scope == "project" || project.join(".claude/xerj-hints/config.json").exists() {
        migrate_old_hint(project, &binary, dry, &mut lines);
    }
    if dry {
        return Ok(Plan { lines });
    }
    if let Some(old) = &old {
        let backup = state_root.join("backups").join(format!(
            "settings-{}.json",
            hash("backup", &String::from_utf8_lossy(old))
        ));
        if !backup.exists() {
            atomic(&backup, old)?;
        }
        lines.push(format!(
            "backup of the previous settings: {}",
            backup.display()
        ));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.xerj-tmp");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, &path)?;
    if !remove {
        super::write_user_mode(mode)?;
        lines.push(format!(
            "mode {mode} (override per shell with XERJ_HOOK=off|shadow|active)"
        ));
    }
    Ok(Plan { lines })
}
