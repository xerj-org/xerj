//! `xerj hook` — a struggle-gated search profiler for AI coding harnesses
//! (Claude Code first). It watches the harness's real searches (Bash
//! grep/rg/find/git grep, Grep, Glob, Read), learns per repo which files a
//! search intent ends on, and speaks only during a strict struggle episode
//! with a confident answer: at most 5 `path:line` lines, never a rewrite,
//! never a denial. Design and pre-registered verification:
//! docs/design/harness-search-profiler.md.
//!
//! Every handler exits 0: a hook that fails must never cost the agent a turn.
mod detect;
mod install;
pub mod parse;
mod profile;
mod render;
#[cfg(test)]
mod tests;
mod tier0;
mod tier1;

use detect::{Rule, Trigger};
use parse::Ev;
use profile::{Nudge, Profile, StreamState};
use serde::Serialize;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const USAGE: &str = "\
xerj hook — learn what an AI coding harness is searching for; help only when it is stuck

USAGE:
    xerj hook install   [--scope user|project] [--project DIR] [--mode shadow|active] [--event batch|post] [--dry-run]
    xerj hook uninstall [--scope user|project] [--project DIR]
    xerj hook status    [--json]
    xerj hook why       [<session-id>]
    xerj hook profile   [--json]
    xerj hook purge
    xerj hook run <batch|post|start|end> --host claude [--speak]   (called by the harness)
    xerj hook replay <events.jsonl> --state DIR --out FILE [--budget-ms N]

MODES:
    shadow   observe and log what it would say; the agent sees nothing (install default)
    active   add at most 5 ranked path:line lines after a strict struggle episode
    off      do nothing
    Resolution: $XERJ_HOOK, then .xerj/hook.toml in the repo, then ~/.config/xerj/hook.toml.

Nothing leaves the machine. State: $XDG_STATE_HOME/xerj/hook (0600). `purge` deletes it.
";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Shadow,
    Active,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "off" | "0" | "false" => Some(Mode::Off),
            "shadow" => Some(Mode::Shadow),
            "active" | "on" => Some(Mode::Active),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    pub max_tokens: usize,
    pub stream_cap: usize,
    pub stream_tokens: u64,
    pub min_gap_calls: u64,
    pub tier1: String,
    pub tier1_url: Option<String>,
    pub tier1_prefix: Option<String>,
    pub half_life_days: f64,
    pub min_pair_streams: u32,
    pub budget_ms: u64,
    pub min_files: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Shadow,
            max_tokens: 300,
            stream_cap: 3,
            stream_tokens: 900,
            min_gap_calls: 8,
            tier1: "off".into(),
            tier1_url: None,
            tier1_prefix: None,
            half_life_days: 14.0,
            min_pair_streams: 2,
            budget_ms: 150,
            min_files: 50,
        }
    }
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn user_config_path() -> Option<PathBuf> {
    config_home().map(|c| c.join("xerj").join("hook.toml"))
}

fn apply_toml(cfg: &mut Config, text: &str) {
    let Ok(v) = text.parse::<toml::Table>() else {
        return;
    };
    let t = v
        .get("hook")
        .and_then(|h| h.as_table())
        .cloned()
        .unwrap_or(v);
    if let Some(m) = t.get("mode").and_then(|x| x.as_str()).and_then(Mode::parse) {
        cfg.mode = m;
    }
    let int = |k: &str| t.get(k).and_then(|x| x.as_integer()).filter(|n| *n >= 0);
    if let Some(n) = int("max_tokens") {
        cfg.max_tokens = (n as usize).min(1500);
    }
    if let Some(n) = int("stream_cap") {
        cfg.stream_cap = n as usize;
    }
    if let Some(n) = int("min_gap_calls") {
        cfg.min_gap_calls = n as u64;
    }
    if let Some(n) = int("min_pair_streams") {
        cfg.min_pair_streams = n as u32;
    }
    if let Some(n) = int("budget_ms") {
        cfg.budget_ms = (n as u64).min(1000);
    }
    if let Some(n) = t
        .get("half_life_days")
        .and_then(|x| x.as_float().or(x.as_integer().map(|i| i as f64)))
    {
        cfg.half_life_days = n.max(0.5);
    }
    if let Some(s) = t.get("tier1").and_then(|x| x.as_str()) {
        cfg.tier1 = s.to_string();
    }
    if let Some(s) = t.get("tier1_url").and_then(|x| x.as_str()) {
        cfg.tier1_url = Some(s.to_string());
    }
    if let Some(s) = t.get("tier1_prefix").and_then(|x| x.as_str()) {
        cfg.tier1_prefix = Some(s.to_string());
    }
}

pub fn load_config(repo: Option<&Path>) -> Config {
    let mut cfg = Config::default();
    if let Some(p) = user_config_path() {
        if let Ok(t) = std::fs::read_to_string(p) {
            apply_toml(&mut cfg, &t);
        }
    }
    if let Some(r) = repo {
        if let Ok(t) = std::fs::read_to_string(r.join(".xerj").join("hook.toml")) {
            apply_toml(&mut cfg, &t);
        }
    }
    if let Some(m) = std::env::var("XERJ_HOOK")
        .ok()
        .as_deref()
        .and_then(Mode::parse)
    {
        cfg.mode = m;
    }
    cfg
}

pub(crate) fn write_user_mode(mode: &str) -> anyhow::Result<()> {
    let Some(path) = user_config_path() else {
        anyhow::bail!("no config directory")
    };
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = old
        .lines()
        .filter(|l| !l.trim_start().starts_with("mode"))
        .map(str::to_string)
        .collect();
    lines.insert(0, format!("mode = \"{mode}\""));
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, lines.join("\n") + "\n")?;
    Ok(())
}

pub fn state_root() -> PathBuf {
    if let Some(p) = std::env::var_os("XERJ_HOOK_STATE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return p;
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("xerj")
        .join("hook")
}

/// One logged decision. `cands` is filled only by `replay` (local, private).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Decision {
    pub t: f64,
    pub key: String,
    pub tuid: String,
    pub repo: String,
    pub mode: String,
    pub search: bool,
    pub rule: Option<String>,
    pub reason: String,
    pub emitted: bool,
    pub gate: Option<String>,
    pub tier: Option<String>,
    pub tokens: usize,
    pub lines: usize,
    pub absent: usize,
    pub scan_ms: Option<f64>,
    pub files: Option<usize>,
    pub complete: Option<bool>,
    pub lat_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cands: Option<Value>,
}

pub struct Ctx {
    pub root: PathBuf,
    pub now: Option<f64>,
    pub replay: bool,
    pub cfg_override: Option<Config>,
    salt: String,
    repo_cache: RefCell<HashMap<PathBuf, Option<(PathBuf, String)>>>,
}

impl Ctx {
    pub fn new(root: PathBuf) -> anyhow::Result<Self> {
        profile::private_dir(&root)?;
        let salt = profile::salt(&root);
        Ok(Self {
            root,
            now: None,
            replay: false,
            cfg_override: None,
            salt,
            repo_cache: RefCell::new(HashMap::new()),
        })
    }
    fn now(&self) -> f64 {
        self.now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        })
    }
    fn h(&self, s: &str) -> String {
        profile::hash(&self.salt, s)
    }
    fn repo(&self, dir: &Path) -> Option<(PathBuf, String)> {
        if let Some(v) = self.repo_cache.borrow().get(dir) {
            return v.clone();
        }
        let v = profile::repo_of(dir);
        self.repo_cache
            .borrow_mut()
            .insert(dir.to_path_buf(), v.clone());
        v
    }
    fn cfg(&self, repo: Option<&Path>) -> Config {
        self.cfg_override
            .clone()
            .unwrap_or_else(|| load_config(repo))
    }
}

fn stream_key(input: &Value) -> String {
    let sid = input
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("nosession");
    let agent = input
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("main");
    format!("{}-{}", profile::safe_key(sid), profile::safe_key(agent))
}

fn calls_of(event: &str, input: &Value) -> Vec<Value> {
    if event == "batch" {
        input
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else {
        vec![input.clone()]
    }
}

/// What a handler produced: JSON for stdout (active mode only) and decisions.
pub struct Outcome {
    pub stdout: Option<String>,
    pub decisions: Vec<Decision>,
}

/// The shared core for live hooks and replay.
pub fn handle(ctx: &Ctx, event: &str, input: &Value, speak: bool) -> Outcome {
    let mut out = Outcome {
        stdout: None,
        decisions: Vec::new(),
    };
    match event {
        "end" => {
            let _ = fold(ctx, &stream_key(input));
        }
        "batch" | "post" => observe(ctx, event, input, speak, &mut out),
        _ => {}
    }
    out
}

fn observe(ctx: &Ctx, event: &str, input: &Value, speak: bool, out: &mut Outcome) {
    let t0 = Instant::now();
    let key = stream_key(input);
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_string();
    let base_cfg = ctx.cfg(None);
    if base_cfg.mode == Mode::Off && ctx.cfg_override.is_none() {
        return;
    }
    let ring_path = profile::ring_path(&ctx.root, &key);
    let state_path = profile::state_path(&ctx.root, &key);
    let mut ring = profile::read_ring(&ring_path);
    let mut state = profile::read_state(&state_path);
    let now = ctx.now();
    let mut best: Option<(Trigger, usize)> = None;
    let mut new_decisions: Vec<Decision> = Vec::new();
    for call in calls_of(event, input) {
        let tool = call.get("tool_name").and_then(Value::as_str).unwrap_or("");
        let tuid = call
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !tuid.is_empty() && state.seen_tuids.contains(&tuid) {
            continue;
        }
        let ti = call.get("tool_input").cloned().unwrap_or(Value::Null);
        let tr = call.get("tool_response").cloned().unwrap_or(Value::Null);
        let mut ev = parse::event_from_call(tool, &ti, &tr, &cwd, &tuid, now);
        state.n += 1;
        ev.n = state.n;
        ev.shown.truncate(64);
        resolve_nudges(ctx, &mut state, &ev, now);
        if !tuid.is_empty() {
            state.seen_tuids.push(tuid.clone());
            if state.seen_tuids.len() > 128 {
                state.seen_tuids.remove(0);
            }
        }
        let _ = profile::push_ring(&ring_path, &ev);
        let search = ev.is_search();
        ring.push(ev);
        if ring.len() > profile::RING {
            ring.remove(0);
        }
        collect_pairs(ctx, &ring, &mut state);
        let mut d = Decision {
            t: now,
            key: key.clone(),
            tuid,
            search,
            mode: format!("{:?}", base_cfg.mode).to_lowercase(),
            ..Decision::default()
        };
        match detect::detect(&ring) {
            Ok(trig) => {
                d.rule = Some(trig.rule.name().into());
                d.reason = "fired".into();
                let idx = new_decisions.len();
                if best
                    .as_ref()
                    .is_none_or(|(b, _)| rule_rank(trig.rule) > rule_rank(b.rule))
                {
                    best = Some((trig, idx));
                }
            }
            Err(r) => d.reason = r.into(),
        }
        if search || d.rule.is_some() {
            new_decisions.push(d);
        }
    }
    if let Some((trig, idx)) = best {
        let (stdout, d) = answer(ctx, event, &cwd, &trig, &mut state, now, speak);
        let slot = &mut new_decisions[idx];
        slot.repo = d.repo;
        slot.reason = d.reason;
        slot.emitted = d.emitted;
        slot.gate = d.gate;
        slot.tier = d.tier;
        slot.tokens = d.tokens;
        slot.lines = d.lines;
        slot.absent = d.absent;
        slot.scan_ms = d.scan_ms;
        slot.files = d.files;
        slot.complete = d.complete;
        slot.cands = d.cands;
        slot.mode = d.mode;
        out.stdout = stdout;
    }
    let _ = profile::write_state(&state_path, &state);
    let lat = t0.elapsed().as_micros() as u64;
    for mut d in new_decisions {
        d.lat_us = lat;
        if !ctx.replay {
            let mut logged = d.clone();
            logged.cands = None;
            if let Ok(line) = serde_json::to_string(&logged) {
                let _ = profile::append_line(&ctx.root.join("decisions.jsonl"), &line);
            }
        }
        out.decisions.push(d);
    }
    if !ctx.replay {
        rotate_decisions(&ctx.root.join("decisions.jsonl"));
    }
}

fn rotate_decisions(path: &Path) {
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > profile::DECISIONS_ROTATE {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
}

fn rule_rank(r: Rule) -> u8 {
    match r {
        Rule::R1 => 5,
        Rule::R3 => 4,
        Rule::R2 => 3,
        Rule::R5 => 2,
        Rule::R4 => 1,
    }
}

fn resolve_nudges(ctx: &Ctx, state: &mut StreamState, ev: &Ev, now: f64) {
    let touched: HashSet<&String> = ev.reads.iter().chain(&ev.edits).collect();
    let mut updates: Vec<(String, String, bool)> = Vec::new();
    for nd in state.nudges.iter_mut().filter(|n| n.resolved.is_none()) {
        if nd.paths.iter().any(|p| touched.contains(p)) {
            nd.resolved = Some(true);
            updates.push((nd.repo.clone(), nd.rule.clone(), true));
        } else if ev.n.saturating_sub(nd.n) > 6 {
            nd.resolved = Some(false);
            state.ignored += 1;
            updates.push((nd.repo.clone(), nd.rule.clone(), false));
        }
    }
    for (repo, rule, up) in updates {
        let path = profile::profile_path(&ctx.root, &repo);
        let mut p = profile::read_profile(&path);
        let st = p.rules.entry(rule).or_default();
        if up {
            st.uptake += 1;
        }
        if st.nudges >= 10 && (st.uptake as f64) < 0.2 * st.nudges as f64 && st.muted_until < now {
            st.muted_until = now + 7.0 * 86400.0;
        }
        let _ = profile::write_profile(&path, &p);
    }
}

fn skip(d: &mut Decision, reason: &str) -> (Option<String>, Decision) {
    d.reason = reason.into();
    (None, d.clone())
}

/// Backoff, mute, Tier 0 (+history) (+Tier 1), confidence gate, render.
fn answer(
    ctx: &Ctx,
    event: &str,
    cwd: &str,
    trig: &Trigger,
    state: &mut StreamState,
    now: f64,
    speak: bool,
) -> (Option<String>, Decision) {
    let mut d = Decision {
        rule: Some(trig.rule.name().into()),
        ..Decision::default()
    };
    let root_dir = PathBuf::from(&trig.root);
    if !root_dir.is_dir() {
        return skip(&mut d, "root_missing");
    }
    let (repo_root, repo_id) = match ctx.repo(&root_dir) {
        Some(r) => r,
        None => (root_dir.clone(), ctx.h(&trig.root)),
    };
    d.repo = repo_id.clone();
    let cfg = ctx.cfg(Some(&repo_root));
    d.mode = format!("{:?}", cfg.mode).to_lowercase();
    if cfg.mode == Mode::Off {
        return skip(&mut d, "off");
    }
    if !trig.rule.active() {
        return skip(&mut d, "shadow_rule");
    }
    let emitted = state.nudges.len();
    if emitted >= cfg.stream_cap || state.tokens >= cfg.stream_tokens {
        return skip(&mut d, "stream_cap");
    }
    if state.ignored >= 2 {
        return skip(&mut d, "backoff_ignored");
    }
    if let Some(last) = state.nudges.last() {
        if state.n.saturating_sub(last.n) < cfg.min_gap_calls {
            return skip(&mut d, "min_gap");
        }
    }
    let ppath = profile::profile_path(&ctx.root, &repo_id);
    let prof = profile::read_profile(&ppath);
    if prof
        .rules
        .get(trig.rule.name())
        .is_some_and(|r| r.muted_until > now)
    {
        return skip(&mut d, "muted");
    }
    // Scan the repo root (the design's scope) when the trigger is inside it.
    let scan_root = if root_dir.starts_with(&repo_root) {
        repo_root.clone()
    } else {
        root_dir.clone()
    };
    let term_hashes: Vec<String> = trig
        .toks
        .iter()
        .chain(&trig.idents)
        .map(|t| ctx.h(t))
        .collect();
    let hist = prof.prior(&term_hashes, cfg.min_pair_streams);
    let salt = ctx.salt.clone();
    let hasher = move |rel: &str| profile::hash(&salt, rel);
    let q = tier0::Query {
        root: &scan_root,
        toks: &trig.toks,
        idents: &trig.idents,
        seen: &trig.seen,
        hist: &hist,
        hasher: &hasher,
        budget: Duration::from_millis(cfg.budget_ms),
    };
    let scan = tier0::scan(&q);
    d.scan_ms = Some((scan.elapsed_ms * 10.0).round() / 10.0);
    d.files = Some(scan.files);
    d.complete = Some(scan.complete);
    d.tier = Some(if hist.is_empty() { "t0" } else { "t0+h" }.into());
    if ctx.replay {
        let top: Vec<Value> = scan
            .hits
            .iter()
            .filter(|h| !h.seen)
            .take(20)
            .map(|h| {
                let hb = if h.hist >= 2 { 2.0 * hist.get(&ctx.h(&h.rel)).map_or(0.0, |x| x.0.min(3.0)) } else { 0.0 };
                json!({"p": h.abs.display().to_string(), "l": h.line, "s": (h.score * 1000.0).round() / 1000.0, "hb": (hb * 1000.0f64).round() / 1000.0, "c": h.covered, "h": h.hist, "d": h.def})
            })
            .collect();
        d.cands = Some(
            json!({"top": top, "root": scan_root.display().to_string(), "n_hist": hist.len()}),
        );
    }
    if !scan.complete {
        return skip(&mut d, "tier0_budget");
    }
    if scan.files < cfg.min_files {
        return skip(&mut d, "small_repo");
    }
    let unseen: Vec<&tier0::Hit> = scan.hits.iter().filter(|h| !h.seen).collect();
    let total = trig.toks.len();
    let mut gate = None;
    let mut chosen: Vec<&tier0::Hit> = Vec::new();
    // (a) Tier 1 exact definition.
    let mut defs_lines: Vec<render::Line> = Vec::new();
    if let Some(t1) = tier1::Tier1::from_env(&cfg) {
        let defs = t1.defs(&trig.idents, &scan_root);
        for df in defs
            .into_iter()
            .filter(|df| !trig.seen.contains(&df.abs.display().to_string()))
            .take(2)
        {
            defs_lines.push(render::Line {
                abs: df.abs.display().to_string(),
                shown: render::display_path(&df.abs, Path::new(cwd)),
                line: df.line,
                text: df.code,
                tag: render::Tag::Def,
            });
        }
        if !defs_lines.is_empty() {
            gate = Some("a");
            d.tier = Some("t1".into());
        }
        t1.index_once(
            &scan_root,
            &ctx.root.join("repos").join(&repo_id).join("indexed"),
        );
    }
    // (c) history pair resolved to an unseen file that still has a term.
    if let Some(h) = unseen
        .iter()
        .find(|h| h.hist >= cfg.min_pair_streams && h.covered >= 1)
    {
        chosen.push(h);
        gate = gate.or(Some("c"));
    }
    // (b) the top unseen file covers >=2 distinct chain terms and scores
    // >=1.3x the next unseen candidate (amendment A1, see the design doc).
    if let Some(top) = unseen.first().filter(|h| h.covered >= 2) {
        let second = unseen.get(1).map_or(0.0, |h| h.score);
        if top.score >= 1.3 * second {
            gate = gate.or(Some("b"));
            for h in unseen
                .iter()
                .filter(|h| h.covered >= 2)
                .take(render::MAX_LINES)
            {
                if !chosen.iter().any(|c| c.abs == h.abs) {
                    chosen.push(h);
                }
            }
        }
    }
    let absent: Vec<String> = if trig.rule == Rule::R1 {
        scan.absent.clone()
    } else {
        Vec::new()
    };
    if gate.is_none() && absent.is_empty() {
        return skip(&mut d, "gate");
    }
    let terms: Vec<String> = trig.toks.iter().chain(&trig.idents).cloned().collect();
    let mut lines: Vec<render::Line> = defs_lines;
    for h in chosen {
        if lines.len() >= render::MAX_LINES {
            break;
        }
        if h.covered > 0 && tier0::reanchor(&h.abs, h.line, &terms) {
            lines.push(render::line_from_hit(h, Path::new(cwd), total));
        }
    }
    if lines.is_empty() && absent.is_empty() {
        return skip(&mut d, "reanchor_failed");
    }
    let text = render::render(trig, &lines, &absent, None, cfg.max_tokens);
    let tokens = render::estimate_tokens(&text);
    d.gate = gate.map(str::to_string).or(Some("absent".into()));
    d.tokens = tokens;
    d.lines = lines.len();
    d.absent = absent.len().min(2);
    d.reason = "emit".into();
    d.emitted = true;
    if let Some(c) = d.cands.as_mut() {
        c["emit"] = json!(lines
            .iter()
            .map(|l| json!({"p": l.abs, "l": l.line}))
            .collect::<Vec<_>>());
    }
    state.nudges.push(Nudge {
        n: state.n,
        rule: trig.rule.name().into(),
        repo: repo_id.clone(),
        paths: lines.iter().map(|l| l.abs.clone()).collect(),
        resolved: if lines.is_empty() { Some(false) } else { None },
    });
    state.tokens += tokens as u64;
    let mut p = prof;
    p.rules.entry(trig.rule.name().into()).or_default().nudges += 1;
    let _ = profile::write_profile(&ppath, &p);
    let stdout = (cfg.mode == Mode::Active && speak).then(|| {
        let ev_name = if event == "batch" {
            "PostToolBatch"
        } else {
            "PostToolUse"
        };
        json!({"hookSpecificOutput": {"hookEventName": ev_name, "additionalContext": text}})
            .to_string()
    });
    (stdout, d)
}

/// The newest call read or edited a file: it is the target of every search
/// up to 6 calls back that has no target yet (design §5.6).
fn collect_pairs(ctx: &Ctx, ring: &[Ev], state: &mut StreamState) {
    let Some(cur) = ring.last() else { return };
    let Some(target) = cur.edits.first().or(cur.reads.first()) else {
        return;
    };
    for e in ring[..ring.len() - 1].iter().rev() {
        if cur.n.saturating_sub(e.n) > detect::CHAIN_MAX_CALLS {
            break;
        }
        if !e.is_search() || state.assigned.contains(&e.tuid) {
            continue;
        }
        state.assigned.push(e.tuid.clone());
        for p in e
            .probes
            .iter()
            .filter(|p| detect::eligible(p, e.remote).is_ok())
        {
            let Some((root, id)) = ctx.repo(Path::new(&p.root)) else {
                continue;
            };
            let Some(rel) = profile::rel_to(&root, target) else {
                continue;
            };
            let terms: Vec<String> = p.toks.iter().chain(&p.idents).map(|t| ctx.h(t)).collect();
            state.pairs.push((id, ctx.h(&rel), terms));
        }
    }
    if state.assigned.len() > 256 {
        state.assigned.drain(..state.assigned.len() - 256);
    }
    if state.pairs.len() > 500 {
        state.pairs.drain(..state.pairs.len() - 500);
    }
}

/// Stream end: fold intent->target pairs, hot roots and zero terms into each
/// repo profile (salted hashes), then purge rings older than 14 days.
fn fold(ctx: &Ctx, key: &str) -> anyhow::Result<()> {
    let ring_path = profile::ring_path(&ctx.root, key);
    let ring = profile::read_ring(&ring_path);
    let state_path = profile::state_path(&ctx.root, key);
    let mut state = profile::read_state(&state_path);
    let now = ctx.now();
    let mut per_repo: HashMap<
        String,
        (
            PathBuf,
            HashSet<(String, String)>,
            HashMap<String, f64>,
            HashSet<String>,
        ),
    > = HashMap::new();
    for e in &ring {
        for p in e
            .probes
            .iter()
            .filter(|p| detect::eligible(p, e.remote).is_ok())
        {
            let Some((root, id)) = ctx.repo(Path::new(&p.root)) else {
                continue;
            };
            let entry = per_repo
                .entry(id)
                .or_insert_with(|| (root.clone(), HashSet::new(), HashMap::new(), HashSet::new()));
            if let Some(rel) = profile::rel_to(&root, &p.root) {
                let depth: PathBuf = Path::new(&rel).components().take(4).collect();
                *entry
                    .2
                    .entry(ctx.h(&depth.display().to_string()))
                    .or_default() += 1.0;
            }
            if p.zero == Some(true) {
                for t in p.toks.iter().chain(&p.idents) {
                    entry.3.insert(ctx.h(t));
                }
            }
        }
    }
    for (id, target, terms) in &state.pairs {
        let entry = per_repo.entry(id.clone()).or_insert_with(|| {
            (
                PathBuf::new(),
                HashSet::new(),
                HashMap::new(),
                HashSet::new(),
            )
        });
        for t in terms {
            entry.1.insert((t.clone(), target.clone()));
        }
    }
    for (id, (_root, pairs, hot, zeros)) in per_repo {
        let path = profile::profile_path(&ctx.root, &id);
        let mut prof: Profile = profile::read_profile(&path);
        prof.decay(now, ctx.cfg(None).half_life_days);
        let mut targets_seen: HashSet<String> = HashSet::new();
        for (term, target) in pairs {
            let pair = prof.pairs.entry(target.clone()).or_default();
            let ts = pair.terms.entry(term).or_default();
            ts.w += 1.0;
            ts.streams += 1;
            pair.last = now;
            if targets_seen.insert(target) {
                pair.streams += 1;
            }
        }
        for (h, w) in hot {
            *prof.hot.entry(h).or_default() += w;
        }
        for z in zeros {
            *prof.zero_terms.entry(z).or_default() += 1.0;
        }
        prof.streams_folded += 1;
        prof.cap();
        profile::write_profile(&path, &prof)?;
    }
    state.pairs.clear();
    let _ = profile::write_state(&state_path, &state);
    if !ctx.replay {
        purge_old(&ctx.root.join("streams"), 14.0 * 86400.0);
    }
    Ok(())
}

fn purge_old(dir: &Path, max_age: f64) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|a| a.as_secs_f64() > max_age);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ───────────────────────────── CLI ─────────────────────────────

pub fn run_cli(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("run") => run_hook(&args[1..]),
        Some("install") => cmd_install(&args[1..], false),
        Some("uninstall") => cmd_install(&args[1..], true),
        Some("status") => cmd_status(&args[1..]),
        Some("why") => cmd_why(&args[1..]),
        Some("profile") => cmd_profile(&args[1..]),
        Some("replay") => cmd_replay(&args[1..]),
        Some("purge") => {
            let root = state_root();
            match std::fs::remove_dir_all(&root) {
                Ok(()) => println!("removed {}", root.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!("nothing to remove at {}", root.display())
                }
                Err(e) => {
                    eprintln!("xerj hook purge: {e}");
                    return 1;
                }
            }
            0
        }
        Some("-h" | "--help") | None => {
            print!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("xerj hook: unknown subcommand '{other}'\n\n{USAGE}");
            2
        }
    }
}

/// Hook entry point. Reads at most 8 MB of JSON; any failure is silence.
fn run_hook(args: &[String]) -> i32 {
    let event = args.first().cloned().unwrap_or_default();
    let speak = event == "batch" || args.iter().any(|a| a == "--speak");
    let mut buf = Vec::new();
    let _ = std::io::stdin().take(8 * 1024 * 1024).read_to_end(&mut buf);
    let Ok(input) = serde_json::from_slice::<Value>(&buf) else {
        return 0;
    };
    let Ok(ctx) = Ctx::new(state_root()) else {
        return 0;
    };
    let ev = match event.as_str() {
        "batch" | "post" | "end" | "start" => event.as_str(),
        _ => return 0,
    };
    let out = handle(&ctx, ev, &input, speak);
    if let Some(s) = out.stdout {
        let mut so = std::io::stdout().lock();
        let _ = writeln!(so, "{s}");
    }
    0
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn cmd_install(args: &[String], remove: bool) -> i32 {
    let scope = flag(args, "--scope").unwrap_or_else(|| "user".into());
    let mode = flag(args, "--mode").unwrap_or_else(|| "shadow".into());
    let event = flag(args, "--event").unwrap_or_else(|| "batch".into());
    if let Some(h) = flag(args, "--host") {
        if h != "claude" {
            eprintln!("xerj hook install: only --host claude is supported in v1 (Codex stays shadow-only, wire it by hand)");
            return 2;
        }
    }
    if Mode::parse(&mode).is_none() {
        eprintln!("xerj hook install: --mode must be off, shadow or active");
        return 2;
    }
    let project = flag(args, "--project")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let dry = args.iter().any(|a| a == "--dry-run");
    let root = state_root();
    if let Err(e) = profile::private_dir(&root) {
        eprintln!("xerj hook: cannot create {}: {e}", root.display());
        return 1;
    }
    match install::run(&root, &scope, &project, &mode, &event, remove, dry) {
        Ok(plan) => {
            for l in plan.lines {
                println!("{l}");
            }
            if !remove && !dry {
                println!("verify: xerj hook status   (expect: mode {mode}, 0 errors; decisions appear after the next tool batch)");
            }
            0
        }
        Err(e) => {
            eprintln!(
                "xerj hook {}: {e:#}",
                if remove { "uninstall" } else { "install" }
            );
            1
        }
    }
}

fn read_decisions(root: &Path) -> Vec<Value> {
    let Ok(f) = std::fs::File::open(root.join("decisions.jsonl")) else {
        return Vec::new();
    };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

fn pct(v: &mut [u64], q: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn cmd_status(args: &[String]) -> i32 {
    let root = state_root();
    let cfg = load_config(std::env::current_dir().ok().as_deref());
    let ds = read_decisions(&root);
    let mut by: HashMap<String, u64> = HashMap::new();
    let mut lat: Vec<u64> = Vec::new();
    let (mut emitted, mut searches) = (0u64, 0u64);
    for d in &ds {
        let rule = d.get("rule").and_then(Value::as_str).unwrap_or("-");
        let reason = d.get("reason").and_then(Value::as_str).unwrap_or("?");
        *by.entry(format!("{rule}:{reason}")).or_default() += 1;
        lat.push(d.get("lat_us").and_then(Value::as_u64).unwrap_or(0));
        emitted += u64::from(d.get("emitted").and_then(Value::as_bool).unwrap_or(false));
        searches += u64::from(d.get("search").and_then(Value::as_bool).unwrap_or(false));
    }
    let (mut nudges, mut uptake) = (0u32, 0u32);
    if let Ok(rd) = std::fs::read_dir(root.join("repos")) {
        for e in rd.flatten() {
            let p = profile::read_profile(&e.path().join("profile.json"));
            for r in p.rules.values() {
                nudges += r.nudges;
                uptake += r.uptake;
            }
        }
    }
    let summary = json!({
        "mode": cfg.mode, "state": root.display().to_string(), "decisions": ds.len(), "searches": searches,
        "emitted": emitted, "nudges": nudges, "uptake": uptake,
        "self_time_us": {"p50": pct(&mut lat, 0.5), "p95": pct(&mut lat, 0.95), "p99": pct(&mut lat, 0.99)},
        "by_rule_reason": by, "tier1": cfg.tier1,
    });
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
        return 0;
    }
    println!("mode: {:?}  state: {}", cfg.mode, root.display());
    println!("decisions: {}  searches seen: {searches}  nudges emitted: {emitted}  uptake: {uptake}/{nudges}", ds.len());
    println!("hook self-time µs p50/p95/p99: {}", summary["self_time_us"]);
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, n) in v.into_iter().take(15) {
        println!("  {k:<28} {n}");
    }
    0
}

fn cmd_why(args: &[String]) -> i32 {
    let want = args.first().cloned();
    for d in read_decisions(&state_root()) {
        let key = d.get("key").and_then(Value::as_str).unwrap_or("");
        if want
            .as_deref()
            .is_some_and(|w| !key.starts_with(&profile::safe_key(w)))
        {
            continue;
        }
        println!(
            "{} {} rule={} reason={} emitted={} gate={} tokens={}",
            d.get("t").and_then(Value::as_f64).unwrap_or(0.0) as u64,
            key,
            d.get("rule").and_then(Value::as_str).unwrap_or("-"),
            d.get("reason").and_then(Value::as_str).unwrap_or("?"),
            d.get("emitted").and_then(Value::as_bool).unwrap_or(false),
            d.get("gate").and_then(Value::as_str).unwrap_or("-"),
            d.get("tokens").and_then(Value::as_u64).unwrap_or(0),
        );
    }
    0
}

fn cmd_profile(args: &[String]) -> i32 {
    let root = state_root().join("repos");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let p = profile::read_profile(&e.path().join("profile.json"));
            out.push(json!({
                "repo": e.file_name().to_string_lossy(), "streams_folded": p.streams_folded, "pairs": p.pairs.len(),
                "terms": p.pairs.values().map(|x| x.terms.len()).sum::<usize>(), "hot_roots": p.hot.len(),
                "zero_terms": p.zero_terms.len(), "rules": p.rules.iter().map(|(k, r)| (k.clone(), json!({"nudges": r.nudges, "uptake": r.uptake, "muted": r.muted_until > 0.0}))).collect::<serde_json::Map<_, _>>(),
            }));
        }
    }
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        for r in out {
            println!("{r}");
        }
    }
    0
}

/// Offline replay: the same `handle` path, clock injected per line.
/// Input lines are hook stdin objects plus `_event` and `_t`.
fn cmd_replay(args: &[String]) -> i32 {
    let Some(input) = args.first().filter(|a| !a.starts_with('-')).cloned() else {
        eprintln!("xerj hook replay: <events.jsonl> is required\n\n{USAGE}");
        return 2;
    };
    let (Some(state), Some(outp)) = (flag(args, "--state"), flag(args, "--out")) else {
        eprintln!("xerj hook replay: --state DIR and --out FILE are required");
        return 2;
    };
    let mut cfg = Config {
        mode: Mode::Active,
        ..Config::default()
    };
    if let Some(b) = flag(args, "--budget-ms").and_then(|b| b.parse().ok()) {
        cfg.budget_ms = b;
    }
    let Ok(mut ctx) = Ctx::new(PathBuf::from(&state)) else {
        eprintln!("xerj hook replay: cannot use state dir {state}");
        return 1;
    };
    ctx.replay = true;
    ctx.cfg_override = Some(cfg);
    let Ok(f) = std::fs::File::open(&input) else {
        eprintln!("xerj hook replay: cannot open {input}");
        return 1;
    };
    let Ok(mut out) = std::fs::File::create(&outp) else {
        eprintln!("xerj hook replay: cannot write {outp}");
        return 1;
    };
    let (mut lines, mut decisions) = (0u64, 0u64);
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        lines += 1;
        ctx.now = v.get("_t").and_then(Value::as_f64);
        let ev = v
            .get("_event")
            .and_then(Value::as_str)
            .unwrap_or("post")
            .to_string();
        let t0 = Instant::now();
        let o = handle(&ctx, &ev, &v, true);
        let us = t0.elapsed().as_micros() as u64;
        for mut d in o.decisions {
            d.lat_us = us;
            let mut j = serde_json::to_value(&d).unwrap_or(Value::Null);
            j["ev"] = json!(ev);
            j["out"] = json!(o.stdout.is_some());
            let _ = writeln!(out, "{j}");
            decisions += 1;
        }
        if lines % 20000 == 0 {
            eprintln!("replay: {lines} lines, {decisions} decisions");
        }
    }
    eprintln!("replay: done, {lines} lines, {decisions} decisions -> {outp}");
    0
}
