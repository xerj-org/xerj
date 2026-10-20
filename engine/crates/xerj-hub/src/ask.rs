//! Corpus Hub v2 — `xerj ask "<question>"`: route a question to a hub corpus and
//! answer it in one step (`docs/CORPUS_HUB_V2_IMPLEMENTATION.md` §5, "Answer
//! flow").
//!
//! The flow, in the plan's own numbering:
//!
//! 1. **Mode.** `--def SYM`, a question that is one identifier, or one matching
//!    `what is the (exact )?value of (\w+)` is a [`AnswerMode::Def`] question;
//!    anything else is [`AnswerMode::How`]; `--files` forces
//!    [`AnswerMode::Files`] ([`detect_mode`]).
//! 2. **Route** ([`crate::router::route`]). `--corpus` skips routing.
//!    Ambiguous → the candidates are printed and the exit code is **4**;
//!    nothing credible → exit **1** with the llms.txt pointer.
//! 3. **Cached pack** → answer locally ([`AnsweredFrom::LocalPack`]); the
//!    network is not touched.
//! 4. **No pack, hosted allowed** → answer through the hosted hub now, then (unless
//!    `--no-fetch`) start a detached `xerj hub pull` for next time — subject to the
//!    pull policy ([`pull_policy`]).
//! 5. **No pack, no hosted** → the pull policy decides first ([`pull_policy`]): an
//!    excluded corpus is never pulled and a pack over the size limit is not
//!    downloaded, whatever else was asked, and neither is ever answered with a
//!    command to run. Then `--offline` (or `--no-fetch`) exits 1 naming the pack to
//!    pull; otherwise pull synchronously and answer locally.
//! 6. **Disk budget** — before any pull, least-recently-used packs are evicted
//!    until the pack cache fits ([`evict_to_budget`], called from `hub pull`).
//! 7. The §1 renderer prints the [`Answer`]; `--json` prints the struct.
//!
//! # Seams
//!
//! [`ask`] holds only the decisions. Everything with side effects sits behind a
//! trait so each decision is testable without a node or a network:
//! [`Backend`] (local pack, node, pull) and [`HostedHub`] (the hosted node,
//! plan §6). The CLI wires [`RealBackend`]; no [`HostedHub`] implementation ships
//! with this module — the hosted endpoints are plan step 6, and until a caller
//! passes one in, "hosted" is simply unavailable and the flow falls back to the
//! local pack / synchronous pull exactly as if the switch were off.
//!
//! # Not here
//!
//! Plan §8 is not wired into this flow: no `xerj.lock` is written, `--locked` does
//! not exist, and the freshness warning is not added (`lockfile.rs` has the
//! pieces; `ask` is where they plug in). `xerj hub sync | update | rm` do not
//! exist. The hosted client is plan step 6/7.
//!
//! # Exit codes
//!
//! | code | meaning |
//! |------|---------|
//! | 0 | answered, at least one hit |
//! | 1 | no match; no corpus; offline/no-fetch with no pack; pull or node failure |
//! | 2 | usage error; unknown `--corpus`; a pull rejected the slug |
//! | 3 | a pulled pack failed verification (sha256 / signature / format) |
//! | 4 | ambiguous — re-run with `--corpus` (the code `xerj autoindex` uses for "needs a decision") |
//! | 5 | this build has no hub public key, so no pack can be verified (nothing was downloaded) |

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use anyhow::Result;
use serde_json::Value;
use xerj_common::hub::{
    render, Answer, AnswerMode, AnsweredFrom, Catalogue, CatalogueEntry, Hit, LicenceClass,
    PackManifest, AUTO_PULL_MAX_BYTES, DEFAULT_HUB_URL,
};
use xerj_common::xccode::{self, licence, passage, state, CodeParams, XcHttp};

use crate::catalogue::load_catalogue;
use crate::node::{self, HubNode, NodeLauncher, ResolvedNode};
use crate::router::{route_ranked, HostedSearch, Routing};
use crate::{cli, def, lockfile, sketch};

/// Answered with at least one hit.
pub const EXIT_OK: i32 = 0;
/// No match / no corpus / offline with no pack / a pull or node failure.
pub const EXIT_NO_ANSWER: i32 = 1;
/// Usage error, or an unknown corpus slug.
pub const EXIT_USAGE: i32 = 2;
/// A pulled pack failed verification.
pub const EXIT_UNVERIFIED: i32 = 3;
/// More than one corpus matches: the caller must choose (`--corpus`).
pub const EXIT_NEEDS_DECISION: i32 = 4;
/// A pull was refused because this build has no hub public key: not a pack that
/// failed verification ([`EXIT_UNVERIFIED`]) but one that cannot be judged at all.
pub const EXIT_NO_KEY: i32 = 5;

/// Printed (stderr) when the router finds nothing credible.
pub const NO_CORPUS_MESSAGE: &str = "no hub corpus matches; see xerj.org/llms.txt";
/// Printed once (stderr) the first time `--hosted auto` resolves to off inside a
/// git work tree.
pub const HOSTED_OFF_NOTICE: &str =
    "hosted answers are off in git repositories; xerj hub config hosted on";
/// Printed (stderr) when `--hosted on` is asked for but no hosted client exists.
const NO_HOSTED_CLIENT: &str =
    "hosted answers were requested but this build has no hosted client; answering from a local pack";

const DEFAULT_K: usize = def::DEFAULT_K;
/// Characters of a passage a How answer keeps (the `xerj code` default).
const PASSAGE_WIDTH: usize = 800;
/// Distinct files a Files answer lists at most.
const FILES_K: usize = 10;

// ── request / outcome types ──────────────────────────────────────────────────

/// What kind of question the caller says this is (`--def` / `--how` /
/// `--files`); [`ModeRequest::Auto`] lets [`detect_mode`] decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeRequest {
    Auto,
    Def(String),
    How,
    Files,
}

/// `--hosted on|off|auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedSwitch {
    Auto,
    On,
    Off,
}

/// Everything `xerj ask` takes from the command line.
#[derive(Debug, Clone)]
pub struct AskArgs {
    pub question: String,
    pub corpus: Option<String>,
    pub mode: ModeRequest,
    pub offline: bool,
    pub no_fetch: bool,
    pub hosted: HostedSwitch,
    pub max_tokens: Option<usize>,
    /// Max hits (`None` = the mode's default). Not a CLI flag: the MCP
    /// `xerj_hub_def` tool and the gRPC `Def` take a `k`, and the flow carries it
    /// to the backend unchanged.
    pub k: Option<usize>,
    pub json: bool,
    pub url: Option<String>,
    /// Use only the catalogue already cached on this machine; never fetch it,
    /// however old it is. Not a CLI flag: the MCP tools set it, so an agent's
    /// tool call never blocks on the hub (the fetch's 10 s connect / 30 s
    /// timeouts). With no cached copy the outcome is [`NO_CACHED_CATALOGUE`].
    /// Unlike `offline`, the rest of the flow is unchanged.
    pub cached_catalogue_only: bool,
}

impl AskArgs {
    /// Defaults for everything but the question.
    pub fn new(question: impl Into<String>) -> Self {
        AskArgs {
            question: question.into(),
            corpus: None,
            mode: ModeRequest::Auto,
            offline: false,
            no_fetch: false,
            hosted: HostedSwitch::Auto,
            max_tokens: None,
            k: None,
            json: false,
            url: None,
            cached_catalogue_only: false,
        }
    }
}

/// What the flow decided: an exit code, the text for stdout, notices for stderr
/// and — when there was one — the [`Answer`] itself (for a caller that is not
/// the CLI, e.g. the MCP tool).
#[derive(Debug, Default, Clone)]
pub struct AskOutcome {
    pub exit: i32,
    /// The §1 rendering, the `--json` form, or the Ambiguous candidate lines.
    /// No trailing newline.
    pub stdout: String,
    /// Notices, one per line, in the order they arose.
    pub stderr: Vec<String>,
    pub answer: Option<Answer>,
}

impl AskOutcome {
    fn fail(exit: i32, message: impl Into<String>) -> Self {
        AskOutcome {
            exit,
            stderr: vec![message.into()],
            ..Default::default()
        }
    }
}

/// One question to one corpus, as the backends see it.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub slug: &'a str,
    pub question: &'a str,
    pub mode: AnswerMode,
    /// Set exactly when `mode` is [`AnswerMode::Def`].
    pub symbol: Option<&'a str>,
    pub max_tokens: Option<usize>,
    /// Max hits; `None` = the mode's default.
    pub k: Option<usize>,
}

// ── seams ────────────────────────────────────────────────────────────────────

/// The hosted hub (plan §6), behind a trait so [`ask`] is network-free in tests
/// and so a build with no hosted client degrades cleanly. It extends
/// [`HostedSearch`], which the router uses for federated corpus search.
pub trait HostedHub: HostedSearch {
    /// Answer `req` through the hosted node (`/_hub/v1/def/…` or
    /// `/_hub/v1/code/…`).
    fn answer(&self, req: &Request<'_>) -> Result<Answer>;
}

/// Lets a `&dyn HostedHub` be handed to the router as a `&dyn HostedSearch`
/// without relying on trait-object upcasting.
struct SearchOnly<'a>(&'a dyn HostedHub);

impl HostedSearch for SearchOnly<'_> {
    fn search(&self, question: &str) -> Result<Vec<(String, f32)>> {
        self.0.search(question)
    }
}

/// A pull that did not complete, with the exit code it maps to.
#[derive(Debug, Clone)]
pub struct PullFailure {
    pub code: i32,
    pub message: String,
}

/// What [`Backend::pull_in_background`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundPull {
    Started {
        pid: u32,
    },
    /// A pull of this corpus was already running; none was started.
    AlreadyRunning {
        pid: u32,
    },
}

/// The side-effecting half of the flow: the local pack, the node it lives on,
/// and pulling. [`RealBackend`] is the production implementation.
pub trait Backend {
    /// Is a pack for `slug` on this machine AND fully loaded on the node queries
    /// run against? `Ok(false)` is the ordinary "not cached"; `Err` is a node
    /// that could not be reached or started. A backend must not start a node when
    /// no pack exists locally.
    fn local_pack_ready(&self, slug: &str) -> Result<bool, String>;
    /// Answer from the local pack. Called after [`Backend::local_pack_ready`]
    /// said yes, or after a successful pull.
    fn answer_local(&self, req: &Request<'_>) -> Result<Answer, String>;
    /// Download, verify and restore the pack, waiting for it to finish.
    fn pull_now(&self, entry: &CatalogueEntry) -> Result<(), PullFailure>;
    /// Start a detached `xerj hub pull` and return at once.
    fn pull_in_background(&self, slug: &str) -> Result<BackgroundPull, String>;
}

/// Everything [`ask`] consults.
pub struct AskDeps<'a> {
    pub hub_home: &'a Path,
    pub cwd: &'a Path,
    pub catalogue: &'a Catalogue,
    /// `None` = no hosted client is configured.
    pub hosted: Option<&'a dyn HostedHub>,
    pub backend: &'a dyn Backend,
}

// ── step 1: mode ─────────────────────────────────────────────────────────────

/// Decide the answer mode and the symbol (for `Def`). `--def` / `--how` /
/// `--files` win; otherwise a question that is one identifier, or that matches
/// `what is the (exact )?value of (\w+)`, is a definition lookup, and anything
/// else is a how-does-it-work question.
pub fn detect_mode(question: &str, requested: &ModeRequest) -> (AnswerMode, Option<String>) {
    match requested {
        ModeRequest::Def(sym) => (AnswerMode::Def, Some(sym.clone())),
        ModeRequest::How => (AnswerMode::How, None),
        ModeRequest::Files => (AnswerMode::Files, None),
        ModeRequest::Auto => {
            let q = question.trim();
            if is_identifier(q) {
                return (AnswerMode::Def, Some(q.to_string()));
            }
            match value_of_symbol(q) {
                Some(sym) => (AnswerMode::Def, Some(sym)),
                None => (AnswerMode::How, None),
            }
        }
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*` — the whole string is one identifier.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The symbol in `what is the (exact )?value of (\w+)`, matched anywhere in the
/// question and case-insensitively on the fixed words (a sentence starts with a
/// capital); the symbol keeps its own case.
fn value_of_symbol(q: &str) -> Option<String> {
    // ASCII lowercasing keeps every byte offset, so `pos` indexes `q` too.
    let lower = q.to_ascii_lowercase();
    for lead in ["what is the exact value of ", "what is the value of "] {
        if let Some(pos) = lower.find(lead) {
            let sym: String = q[pos + lead.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !sym.is_empty() {
                return Some(sym);
            }
        }
    }
    None
}

// ── step 4/5: the pull policy ────────────────────────────────────────────────

/// Whether the hub may pull a corpus on the caller's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullPolicy {
    Allow,
    /// `pack_bytes` is above [`AUTO_PULL_MAX_BYTES`]: the user must ask.
    TooLarge,
    /// The licence class is `excluded`: never pulled.
    Excluded,
}

/// `Excluded` is never pulled; above the limit is not pulled automatically;
/// anything else (including `approach-only`, which carries a warning instead) is.
pub fn pull_policy(entry: &CatalogueEntry) -> PullPolicy {
    if entry.licence_class == LicenceClass::Excluded {
        PullPolicy::Excluded
    } else if entry.pack_bytes > AUTO_PULL_MAX_BYTES {
        PullPolicy::TooLarge
    } else {
        PullPolicy::Allow
    }
}

/// `12.3` for a pack of 12.3 MiB.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

/// `run: xerj hub pull <slug> (<MB> MB)` — the command that fetches this pack.
///
/// Only ever offered for a pack the pull policy allows ([`PullPolicy::Allow`]):
/// `xerj hub pull` refuses an excluded corpus and a pack over the size limit, so a
/// hint naming it would send the reader to a command that fails — or, worse, one
/// they would be tempted to get around. Callers check [`pull_policy`] first.
pub fn pull_hint(entry: &CatalogueEntry) -> String {
    format!(
        "run: xerj hub pull {} ({} MB)",
        printable(&entry.slug),
        megabytes(entry.pack_bytes)
    )
}

/// A catalogue- or network-supplied string made safe to print on one terminal
/// line. The catalogue is accepted unverified until the hub key ships, and a slug
/// or an `ask_for` is printed for an agent (and a person) to read: a control
/// character in it is an escape sequence, a forged line, or a rewritten screen.
/// Every control character — ESC, CR, LF, BEL, NUL, DEL, the C1 range — the
/// Unicode line/paragraph separators and the bidirectional overrides (which
/// reorder how a slug is *displayed*) become a space; runs of spaces collapse.
pub fn printable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        let hostile = c.is_control()
            || matches!(
                c,
                '\u{2028}' | '\u{2029}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            );
        let c = if hostile { ' ' } else { c };
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// `approach-only licence (<spdx>): read, do not copy`.
pub fn approach_only_warning(spdx: &str) -> String {
    format!(
        "approach-only licence ({}): read, do not copy",
        if spdx.is_empty() { "unknown" } else { spdx }
    )
}

fn push_unique(list: &mut Vec<String>, line: String) {
    if !list.contains(&line) {
        list.push(line);
    }
}

// ── the hosted switch ────────────────────────────────────────────────────────

/// Where `--hosted auto` landed, and the one-time notice (if this run owes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedResolution {
    pub on: bool,
    pub notice: Option<String>,
}

const CONFIG_FILE: &str = "config.toml";
const NOTICE_MARKER: &str = "hosted_notice_shown";

/// Resolve `--hosted`. `on` / `off` are final. `auto` (the default) follows
/// `<hub>/config.toml`'s `hosted = "on" | "off"` when set, and otherwise is OFF
/// inside a git work tree and ON outside one (decision 1 in the discussion). The
/// first time it resolves to off inside a repo it returns [`HOSTED_OFF_NOTICE`]
/// and drops a marker so the notice is shown once.
pub fn resolve_hosted(flag: HostedSwitch, hub_home: &Path, cwd: &Path) -> HostedResolution {
    let decided = |on| HostedResolution { on, notice: None };
    match flag {
        HostedSwitch::On => decided(true),
        HostedSwitch::Off => decided(false),
        HostedSwitch::Auto => match config_hosted(hub_home).as_deref() {
            Some("on") => decided(true),
            Some("off") => decided(false),
            _ => {
                if !inside_git_worktree(cwd) {
                    return decided(true);
                }
                let marker = hub_home.join(NOTICE_MARKER);
                let first_time = !marker.exists();
                if first_time {
                    let _ = fs::create_dir_all(hub_home);
                    let _ = fs::write(&marker, "shown\n");
                }
                HostedResolution {
                    on: false,
                    notice: first_time.then(|| HOSTED_OFF_NOTICE.to_string()),
                }
            }
        },
    }
}

/// The `hosted` value in `<hub>/config.toml`, if the file parses and sets one.
fn config_hosted(hub_home: &Path) -> Option<String> {
    let text = fs::read_to_string(hub_home.join(CONFIG_FILE)).ok()?;
    let table: toml::Table = text.parse().ok()?;
    table
        .get("hosted")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

/// Write `hosted = "on" | "off"` into `<hub>/config.toml`, keeping any other
/// keys; `None` removes the key (back to `auto`).
pub fn set_config_hosted(hub_home: &Path, value: Option<&str>) -> Result<()> {
    let path = hub_home.join(CONFIG_FILE);
    let mut table: toml::Table = match fs::read_to_string(&path) {
        Ok(text) => text.parse()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e.into()),
    };
    match value {
        Some(v) => {
            table.insert("hosted".into(), toml::Value::String(v.to_string()));
        }
        None => {
            table.remove("hosted");
        }
    }
    fs::create_dir_all(hub_home)?;
    fs::write(&path, toml::to_string(&table)?)?;
    Ok(())
}

/// True when `cwd` or an ancestor holds a `.git` entry (directory or worktree
/// gitlink file).
fn inside_git_worktree(cwd: &Path) -> bool {
    lockfile::project_root(cwd).join(".git").exists()
}

// ── the flow ─────────────────────────────────────────────────────────────────

/// Run the answer flow (plan §5, steps 1-7). Pure over [`AskDeps`]; the only
/// state it touches directly is the hosted-notice marker in `hub_home`.
pub fn ask(args: &AskArgs, deps: &AskDeps<'_>) -> AskOutcome {
    let mut out = AskOutcome::default();
    let (mode, symbol) = detect_mode(&args.question, &args.mode);

    // `--offline` never reaches the network, so there is nothing to resolve; a
    // resolved-off switch must never send the question to the hosted node.
    let hosted: Option<&dyn HostedHub> = if args.offline {
        None
    } else {
        let res = resolve_hosted(args.hosted, deps.hub_home, deps.cwd);
        if let Some(notice) = res.notice {
            out.stderr.push(notice);
        }
        if res.on {
            if deps.hosted.is_none() && args.hosted == HostedSwitch::On {
                out.stderr.push(NO_HOSTED_CLIENT.to_string());
            }
            deps.hosted
        } else {
            None
        }
    };

    // Step 2: route.
    let Some(slug) = select_corpus(args, deps, hosted, symbol.as_deref(), &mut out) else {
        return out;
    };
    let entry = deps.catalogue.corpora.iter().find(|e| e.slug == slug);
    // An excluded licence class is never served — not from a pack that happens to
    // be on disk (pulled before a reclassification, or copied in by hand), and not
    // by the hosted node, whose `ask_answer` routes over the non-excluded corpora
    // only and whose answer endpoints refuse one. `xerj_hub_find` never lists one;
    // naming it explicitly gets this plain no rather than an answer.
    if let Some(e) = entry.filter(|e| e.licence_class == LicenceClass::Excluded) {
        out.stderr.push(format!(
            "'{}' has an excluded licence class ({}) and is never served or pulled",
            printable(&slug),
            printable(&e.licence_spdx)
        ));
        out.exit = EXIT_NO_ANSWER;
        return out;
    }
    let req = Request {
        slug: &slug,
        question: &args.question,
        mode,
        symbol: symbol.as_deref(),
        max_tokens: args.max_tokens,
        k: args.k,
    };

    // Step 3: a pack on this machine answers locally, with no network.
    match deps.backend.local_pack_ready(&slug) {
        Ok(true) => return answer_locally(args, deps, &req, entry, out),
        Ok(false) => {}
        Err(e) => out.stderr.push(format!("local pack unavailable: {e}")),
    }

    // Step 4: hosted answers now; the pull happens behind it.
    if let Some(h) = hosted {
        match h.answer(&req) {
            Ok(answer) => return finish_hosted(args, deps, &slug, entry, answer, out),
            Err(e) => out.stderr.push(format!("hosted answer failed: {e:#}")),
        }
    }

    // Step 5: neither a pack nor a hosted answer.
    let shown = printable(&slug);
    let Some(entry) = entry else {
        out.stderr.push(format!(
            "corpus '{shown}' is not in the catalogue, so it cannot be pulled, and no hosted \
             answer is available"
        ));
        out.exit = EXIT_NO_ANSWER;
        return out;
    };
    // The pull policy is decided FIRST, before anything about how this run was asked
    // to behave. Whether a pack may be pulled at all does not depend on `--offline`
    // or `--no-fetch`. An EXCLUDED corpus is never pulled, so no command is printed
    // for it — a `run: xerj hub pull <slug>` would be a way round "never pulled"
    // (the offline branch used to run before the policy and print exactly that; the
    // pull had no licence check, so an agent that followed the hint pulled it). An
    // OVERSIZE pack is different: a question does not drag it down automatically,
    // but `xerj hub pull <slug>` fetches it explicitly, so its message names that
    // command — the same hint an allowed pack gets.
    match pull_policy(entry) {
        PullPolicy::Excluded => {
            out.stderr.push(format!(
                "'{shown}' has an excluded licence class ({}) and is never pulled; no hosted \
                 answer is available",
                printable(&entry.licence_spdx)
            ));
            out.exit = EXIT_NO_ANSWER;
            return out;
        }
        PullPolicy::TooLarge => {
            out.stderr.push(format!(
                "'{shown}' is {} MB, over the {} MB auto-pull cap, so a question does not \
                 download it automatically and no hosted answer is available; run \
                 `xerj hub pull {shown}` to fetch it once, then ask again",
                megabytes(entry.pack_bytes),
                megabytes(AUTO_PULL_MAX_BYTES)
            ));
            out.exit = EXIT_NO_ANSWER;
            return out;
        }
        PullPolicy::Allow => {}
    }
    if args.offline || args.no_fetch {
        let flag = if args.offline {
            "--offline"
        } else {
            "--no-fetch"
        };
        out.stderr.push(format!(
            "no local pack for '{shown}' and {flag} forbids downloading it; {}",
            pull_hint(entry)
        ));
        out.exit = EXIT_NO_ANSWER;
        return out;
    }
    match deps.backend.pull_now(entry) {
        Ok(()) => answer_locally(args, deps, &req, Some(entry), out),
        Err(f) => {
            out.stderr.push(f.message);
            out.exit = f.code;
            out
        }
    }
}

/// A definition name found in the term sketches of more corpora than this names no
/// one corpus (`main`, `init`, `NULL`): it is not evidence, and listing every
/// corpus that defines it would only be noise.
const MAX_SYMBOL_CANDIDATES: usize = 5;
/// How many closest corpora a "nothing matches" answer names.
const CLOSEST_SHOWN: usize = 3;

/// The corpora whose term sketch holds `symbol`, by slug. Sketches hold every
/// definition's symbol lower-cased (catalogues built before that hold the raw
/// case too, so both forms are probed), so a hit means the corpus defines that
/// name, give or take the Bloom false-positive rate (~0.07% per corpus at 16
/// bits per term) and the per-corpus term cap (`sketch::MAX_TERMS`: a corpus
/// with more terms keeps its most characteristic ones, and an exact lookup of a
/// rare symbol there needs the corpus named). Empty when the name is too common
/// to point anywhere ([`MAX_SYMBOL_CANDIDATES`]).
fn symbol_hits(cat: &Catalogue, symbol: &str) -> Vec<String> {
    let lower = symbol.to_lowercase();
    let mut hits: Vec<String> = cat
        .corpora
        .iter()
        .filter(|e| {
            sketch::contains(&e.term_sketch, symbol) || sketch::contains(&e.term_sketch, &lower)
        })
        .map(|e| e.slug.clone())
        .collect();
    if hits.len() > MAX_SYMBOL_CANDIDATES {
        return Vec::new();
    }
    hits.sort();
    hits
}

/// Fold the exact-symbol evidence into the verdict the question's words reached.
///
/// A confident verdict stands: the question (or the project's own dependencies)
/// already decided, and a symbol that merely also exists elsewhere does not
/// override that. Otherwise the symbol decides what the words could not: of an
/// ambiguous list it keeps the candidates that define the name, and when the words
/// found nothing credible it proposes the corpora that define it. One such corpus
/// is a confident answer (an exact name in one corpus's definitions is strong
/// evidence — the question's own words, diluted by every stop word around the
/// name, never could reach that score for a `what is the value of NAME` question);
/// several are ambiguous.
fn with_symbol_evidence(routing: Routing, hits: &[String]) -> Routing {
    let pick = |candidates: Vec<String>| match candidates.len() {
        0 => None,
        1 => Some(Routing::Confident(candidates[0].clone())),
        _ => Some(Routing::Ambiguous(candidates)),
    };
    match routing {
        Routing::Confident(slug) => Routing::Confident(slug),
        Routing::Ambiguous(slugs) => {
            let both: Vec<String> = slugs.iter().filter(|s| hits.contains(s)).cloned().collect();
            pick(both).unwrap_or(Routing::Ambiguous(slugs))
        }
        Routing::NoCorpus => pick(hits.to_vec()).unwrap_or(Routing::NoCorpus),
    }
}

/// The catalogue without its excluded-licence corpora: what routing, the
/// "closest corpora" hint and `find` may ever name. Mirrors the gRPC
/// `ask_answer`'s `routable` catalogue.
fn routable(cat: &Catalogue) -> Catalogue {
    Catalogue {
        format_version: cat.format_version,
        generated_at: cat.generated_at.clone(),
        corpora: cat
            .corpora
            .iter()
            .filter(|e| e.licence_class != LicenceClass::Excluded)
            .cloned()
            .collect(),
    }
}

/// A named corpus is echoed back at most this many bytes long (the hub's own
/// slugs are at most 64; anything longer is not one, and a caller-supplied
/// string is not repeated back in full).
const SLUG_ECHO_MAX: usize = 64;

/// `printable`, cut to [`SLUG_ECHO_MAX`] bytes on a character boundary.
fn printable_slug(s: &str) -> String {
    if s.len() <= SLUG_ECHO_MAX {
        return printable(s);
    }
    let mut end = SLUG_ECHO_MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", printable(&s[..end]))
}

/// Step 2. `None` means `out` already holds the verdict (an exit code and its
/// message / candidate lines).
fn select_corpus(
    args: &AskArgs,
    deps: &AskDeps<'_>,
    hosted: Option<&dyn HostedHub>,
    symbol: Option<&str>,
    out: &mut AskOutcome,
) -> Option<String> {
    if let Some(c) = &args.corpus {
        if deps.catalogue.corpora.iter().any(|e| &e.slug == c) {
            return Some(c.clone());
        }
        out.stderr.push(format!(
            "unknown corpus '{}' (catalogue generated {})",
            printable_slug(c),
            printable(&deps.catalogue.generated_at)
        ));
        out.exit = EXIT_USAGE;
        return None;
    }

    // Route over the corpora that can be served at all: an excluded corpus is
    // never picked, never offered as ambiguous, never named as "closest".
    let catalogue = &routable(deps.catalogue);
    let cached = cached_slugs(deps.hub_home);
    // A definition lookup names its symbol: route on the symbol itself as well as
    // on the sentence around it.
    let by_symbol = symbol
        .map(|s| symbol_hits(catalogue, s.trim()))
        .unwrap_or_default();
    // Route on the catalogue alone first. The hosted node is asked only when
    // that is not decisive, so a question a local pack can answer never leaves
    // the machine.
    let (routing, mut ranked) = route_ranked(&args.question, deps.cwd, catalogue, None, &cached);
    let mut routing = with_symbol_evidence(routing, &by_symbol);
    if !matches!(routing, Routing::Confident(_)) {
        if let Some(h) = hosted {
            let (hosted_routing, hosted_ranked) = route_ranked(
                &args.question,
                deps.cwd,
                catalogue,
                Some(&SearchOnly(h)),
                &cached,
            );
            routing = with_symbol_evidence(hosted_routing, &by_symbol);
            ranked = hosted_ranked;
        }
    }
    // Hosted search evidence can still name an excluded corpus (the local
    // catalogue knows its class even when the hosted node's ranking does not
    // say): drop it from the verdict. A slug the local catalogue does not list at
    // all is left alone — the hosted node may know a newer catalogue.
    let excluded = |s: &String| {
        deps.catalogue
            .corpora
            .iter()
            .any(|e| &e.slug == s && e.licence_class == LicenceClass::Excluded)
    };
    let known = |s: &String| catalogue.corpora.iter().any(|e| &e.slug == s);
    let routing = match routing {
        Routing::Confident(slug) if excluded(&slug) => Routing::NoCorpus,
        Routing::Ambiguous(slugs) => {
            let kept: Vec<String> = slugs.into_iter().filter(|s| !excluded(s)).collect();
            // One left is still not a confident pick: the asker decides.
            if kept.is_empty() {
                Routing::NoCorpus
            } else {
                Routing::Ambiguous(kept)
            }
        }
        other => other,
    };
    match routing {
        Routing::Confident(slug) => Some(slug),
        Routing::Ambiguous(slugs) => {
            let lines: Vec<String> = slugs
                .iter()
                .map(|s| {
                    let ask_for = catalogue
                        .corpora
                        .iter()
                        .find(|e| &e.slug == s)
                        .map(|e| printable(&e.ask_for))
                        .unwrap_or_default();
                    format!("{} — {ask_for}", printable(s))
                })
                .collect();
            out.stdout = lines.join("\n");
            out.stderr
                .push("more than one corpus matches; re-run with --corpus <slug>".to_string());
            out.exit = EXIT_NEEDS_DECISION;
            None
        }
        Routing::NoCorpus => {
            out.stderr.push(NO_CORPUS_MESSAGE.to_string());
            // Nothing was credible, but something may have been close: name the
            // nearest few so the asker can pick one with `--corpus` instead of
            // starting over from "no".
            let closest: Vec<String> = ranked
                .iter()
                .filter(|(slug, _)| known(slug))
                .take(CLOSEST_SHOWN)
                .map(|(slug, _)| printable(slug))
                .collect();
            if !closest.is_empty() {
                out.stderr.push(format!(
                    "closest corpora: {}; re-run with --corpus <slug> to ask one of them",
                    closest.join(", ")
                ));
            }
            out.exit = EXIT_NO_ANSWER;
            None
        }
    }
}

/// Answer from the local pack and finish.
fn answer_locally(
    args: &AskArgs,
    deps: &AskDeps<'_>,
    req: &Request<'_>,
    entry: Option<&CatalogueEntry>,
    mut out: AskOutcome,
) -> AskOutcome {
    match deps.backend.answer_local(req) {
        Ok(answer) => {
            finalize(args, entry, answer, Vec::new(), &mut out);
            out
        }
        Err(e) => {
            out.stderr.push(e);
            out.exit = EXIT_NO_ANSWER;
            out
        }
    }
}

/// A hosted answer was produced: apply the pull policy (spawning the background
/// pull when allowed) and finish.
fn finish_hosted(
    args: &AskArgs,
    deps: &AskDeps<'_>,
    slug: &str,
    entry: Option<&CatalogueEntry>,
    mut answer: Answer,
    mut out: AskOutcome,
) -> AskOutcome {
    // The answer is attributed to the corpus this run asked about, whatever the
    // response says it is: "always names the corpus" must be true of what was
    // asked, not of what a response claims. (The flow below also pulls `slug`.)
    answer.corpus = slug.to_string();
    answer.answered_from = AnsweredFrom::Hosted;
    let mut notes: Vec<String> = Vec::new();
    match entry {
        Some(e) => match pull_policy(e) {
            PullPolicy::Allow => {
                if !args.no_fetch {
                    match deps.backend.pull_in_background(&e.slug) {
                        Ok(BackgroundPull::Started { pid }) => out.stderr.push(format!(
                            "pulling '{}' in the background (pid {pid}); the next answer is local",
                            printable(&e.slug)
                        )),
                        Ok(BackgroundPull::AlreadyRunning { .. }) => {}
                        Err(msg) => out.stderr.push(format!(
                            "could not start the background pull: {}",
                            printable(&msg)
                        )),
                    }
                }
            }
            // An oversize pack is not auto-pulled, but `xerj hub pull` fetches it
            // explicitly, so the note names that command; an excluded one it refuses.
            PullPolicy::TooLarge => notes.push(format!(
                "the pack is {} MB, over the {} MB auto-pull cap, so this answer is from the \
                 hosted hub; run `xerj hub pull {}` to fetch it for local answers",
                megabytes(e.pack_bytes),
                megabytes(AUTO_PULL_MAX_BYTES),
                printable(&e.slug)
            )),
            PullPolicy::Excluded => notes.push(format!(
                "excluded licence ({}): this corpus is never pulled",
                printable(&e.licence_spdx)
            )),
        },
        None => notes.push(format!(
            "corpus '{}' is not in the local catalogue; it was not pulled",
            printable(slug)
        )),
    }
    finalize(args, entry, answer, notes, &mut out);
    out
}

/// Common tail: licence warnings, the token cap, printing, the exit code.
fn finalize(
    args: &AskArgs,
    entry: Option<&CatalogueEntry>,
    mut answer: Answer,
    notes: Vec<String>,
    out: &mut AskOutcome,
) {
    for n in notes {
        push_unique(&mut answer.warnings, n);
    }
    let approach_only = answer.licence_class == LicenceClass::ApproachOnly
        || entry.is_some_and(|e| e.licence_class == LicenceClass::ApproachOnly);
    if approach_only {
        let spdx = entry
            .map(|e| e.licence_spdx.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&answer.licence_spdx)
            .to_string();
        let warning = approach_only_warning(&spdx);
        for hit in &mut answer.hits {
            if hit.licence_warning.is_none() {
                hit.licence_warning = Some(warning.clone());
            }
        }
        push_unique(&mut answer.warnings, warning);
    }
    if let Some(n) = args.max_tokens {
        cap_tokens(&mut answer, n);
    }
    out.stdout = if args.json {
        serde_json::to_string(&answer).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        render(&answer)
    };
    out.exit = if answer.hits.is_empty() {
        EXIT_NO_ANSWER
    } else {
        EXIT_OK
    };
    out.answer = Some(answer);
}

/// Keep the rendered answer within roughly `max_tokens` (4 characters a token):
/// drop trailing hits, then shorten the last one's text, marking the answer
/// `truncated`. The first hit is always kept.
pub fn cap_tokens(answer: &mut Answer, max_tokens: usize) {
    let budget = max_tokens.saturating_mul(4);
    let size = |a: &Answer| render(a).chars().count();
    while size(answer) > budget && answer.hits.len() > 1 {
        answer.hits.pop();
        answer.truncated = true;
    }
    let over = size(answer).saturating_sub(budget);
    if over > 0 {
        if let Some(hit) = answer.hits.first_mut() {
            let keep = hit.text.chars().count().saturating_sub(over + 1);
            let mut shortened: String = hit.text.chars().take(keep).collect();
            shortened.push('…');
            hit.text = shortened;
            answer.truncated = true;
        }
    }
}

// ── the pack cache on disk ───────────────────────────────────────────────────

const PACKS_DIR: &str = "packs";
const PACK_MANIFEST_FILE: &str = "pack.json";
const LAST_USED_FILE: &str = "last_used";

/// One extracted pack: `<hub>/packs/<slug>/<pin8>/`.
#[derive(Debug, Clone)]
pub struct PackDir {
    pub slug: String,
    pub pin8: String,
    pub dir: PathBuf,
    /// The mtime of the pack's `last_used` file, else of the directory itself.
    pub last_used: SystemTime,
}

/// A [`PackDir`] with its size on disk.
#[derive(Debug, Clone)]
pub struct CachedPack {
    pub pack: PackDir,
    pub bytes: u64,
}

/// Every complete pack (a directory holding `pack.json`) under `<hub>/packs`,
/// sorted by slug then pin. Cheap: no directory is walked for its size.
pub fn pack_dirs(hub_home: &Path) -> Vec<PackDir> {
    let mut out = Vec::new();
    let Ok(slugs) = fs::read_dir(hub_home.join(PACKS_DIR)) else {
        return out;
    };
    for slug_ent in slugs.flatten() {
        if !slug_ent.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let slug = slug_ent.file_name().to_string_lossy().into_owned();
        let Ok(pins) = fs::read_dir(slug_ent.path()) else {
            continue;
        };
        for pin_ent in pins.flatten() {
            let dir = pin_ent.path();
            if !pin_ent.file_type().is_ok_and(|t| t.is_dir())
                || !dir.join(PACK_MANIFEST_FILE).is_file()
            {
                continue; // a `.tmp`, a `.lock`, or a half-extracted dir
            }
            let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
            let last_used = mtime(&dir.join(LAST_USED_FILE))
                .or_else(|| mtime(&dir))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(PackDir {
                slug: slug.clone(),
                pin8: pin_ent.file_name().to_string_lossy().into_owned(),
                dir,
                last_used,
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.pin8.cmp(&b.pin8)));
    out
}

/// [`pack_dirs`] with each pack's size.
pub fn list_cached_packs(hub_home: &Path) -> Vec<CachedPack> {
    pack_dirs(hub_home)
        .into_iter()
        .map(|pack| CachedPack {
            bytes: dir_size(&pack.dir),
            pack,
        })
        .collect()
}

/// Slugs with at least one pack on disk (what the router gives a small bonus).
pub fn cached_slugs(hub_home: &Path) -> Vec<String> {
    let mut slugs: Vec<String> = pack_dirs(hub_home).into_iter().map(|p| p.slug).collect();
    slugs.dedup(); // already sorted by slug
    slugs
}

/// Mark every pack of `slug` as just used (the `last_used` file each pack
/// directory gets on every answer). Best effort: LRU order is a courtesy, never
/// a reason to fail an answer.
pub fn touch_last_used(hub_home: &Path, slug: &str) {
    let stamp = chrono::Utc::now().timestamp().to_string();
    for p in pack_dirs(hub_home).into_iter().filter(|p| p.slug == slug) {
        let _ = fs::write(p.dir.join(LAST_USED_FILE), &stamp);
    }
}

/// Total bytes under `dir` (symlinks are not followed).
fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// Make room for a pack of `incoming_bytes` (plan §5 step 6): when `<hub>/packs`
/// plus the incoming pack would exceed `budget`, remove whole packs, least
/// recently used first, until it fits — never a pack of `keep_slug` (the one
/// being pulled). Returns what was removed. If everything else is gone and the
/// budget is still exceeded, the pull proceeds anyway: the budget bounds the
/// cache, it does not veto the pack being fetched.
///
/// Only the extracted pack directories are removed. The restored indices live in
/// the node's own data directory and are not touched — deleting indices from a
/// node is not this function's business.
pub fn evict_to_budget(
    hub_home: &Path,
    keep_slug: &str,
    incoming_bytes: u64,
    budget: u64,
) -> Vec<CachedPack> {
    let mut total = dir_size(&hub_home.join(PACKS_DIR)).saturating_add(incoming_bytes);
    let mut evicted = Vec::new();
    if total <= budget {
        return evicted;
    }
    let mut candidates: Vec<CachedPack> = list_cached_packs(hub_home)
        .into_iter()
        .filter(|c| c.pack.slug != keep_slug)
        .collect();
    candidates.sort_by(|a, b| {
        a.pack
            .last_used
            .cmp(&b.pack.last_used)
            .then_with(|| a.pack.slug.cmp(&b.pack.slug))
            .then_with(|| a.pack.pin8.cmp(&b.pack.pin8))
    });
    for c in candidates {
        if total <= budget {
            break;
        }
        if fs::remove_dir_all(&c.pack.dir).is_ok() {
            total = total.saturating_sub(c.bytes);
            // Drop the slug directory once its last pin is gone.
            if let Some(parent) = c.pack.dir.parent() {
                let _ = fs::remove_dir(parent);
            }
            evicted.push(c);
        }
    }
    evicted
}

// ── answers from a local pack ────────────────────────────────────────────────

/// The corpus facts an [`Answer`] carries, from the state ledger `hub pull`
/// wrote (the same source `def::lookup` uses, so the two modes agree).
struct AnswerMeta {
    corpus: String,
    pin_sha: String,
    pin_date: String,
    age_days: u32,
    licence_spdx: String,
    licence_class: LicenceClass,
    answered_from: AnsweredFrom,
}

fn answer_meta(root: &Path, slug: &str) -> AnswerMeta {
    match state::load_state(root, slug) {
        Ok(st) => {
            let pin_date = st.pin_date.clone().unwrap_or_default();
            AnswerMeta {
                corpus: st.name(slug),
                pin_sha: st.pin_sha.clone().unwrap_or_default(),
                age_days: age_days(&pin_date, st.indexed_at.as_deref()),
                pin_date,
                licence_spdx: st.licence_spdx.clone().unwrap_or_default(),
                licence_class: match st.licence_class.as_deref() {
                    Some("approach-only") => LicenceClass::ApproachOnly,
                    Some("excluded") => LicenceClass::Excluded,
                    _ => LicenceClass::AdaptWithAttribution,
                },
                answered_from: if st.is_hubpack() {
                    AnsweredFrom::LocalPack
                } else {
                    AnsweredFrom::LocalIndex
                },
            }
        }
        Err(_) => AnswerMeta {
            corpus: slug.to_string(),
            pin_sha: String::new(),
            pin_date: String::new(),
            age_days: 0,
            licence_spdx: String::new(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: AnsweredFrom::LocalPack,
        },
    }
}

/// Days from the pin date (`YYYY-MM-DD`) to today; else from `indexed_at`; else 0.
fn age_days(pin_date: &str, indexed_at: Option<&str>) -> u32 {
    if let Ok(d) = chrono::NaiveDate::parse_from_str(pin_date, "%Y-%m-%d") {
        return (chrono::Utc::now().date_naive() - d).num_days().max(0) as u32;
    }
    if let Some(stamp) = indexed_at {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%SZ") {
            let age = chrono::Utc::now()
                .naive_utc()
                .signed_duration_since(dt)
                .num_days();
            return age.max(0) as u32;
        }
    }
    0
}

/// Turn a code-search response (the raw `_search` body `run_code_query` returns
/// with `as_json`) into an [`Answer`] for How or Files mode.
///
/// How: one [`Hit`] per record, the passage chosen exactly as `xerj code` chooses
/// it (the matching definition when the record lists symbols, else the densest
/// window), `kind` the definition's kind or `passage`, and `line` the line that
/// definition starts at — or the record's own `line`, or 0 when it carries none:
/// a line number is reported only when the index supplied one. Files: one hit
/// per distinct file, its licence as the text.
///
/// `licences` maps a repo (the first path segment) to its licence; a repo with a
/// restricted licence tags its hits with the read-don't-copy warning.
fn answer_from_search(
    meta: AnswerMeta,
    resp: &Value,
    question: &str,
    mode: AnswerMode,
    licences: &HashMap<String, String>,
) -> Answer {
    let rows = resp
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut hits = Vec::new();
    for row in &rows {
        let src = row.get("_source").cloned().unwrap_or(Value::Null);
        let locator = passage::provenance(&src);
        let (file, record_line) = split_locator(&locator);
        let score = row.get("_score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let lic = licences
            .get(passage::locator_repo(&locator))
            .cloned()
            .unwrap_or_default();
        let licence_warning =
            (!lic.is_empty() && licence::is_restricted(&lic)).then(|| approach_only_warning(&lic));

        if mode == AnswerMode::Files {
            if !seen_files.insert(file.clone()) {
                continue;
            }
            let shown = if lic.is_empty() {
                meta.licence_spdx.as_str()
            } else {
                lic.as_str()
            };
            hits.push(Hit {
                file,
                line: record_line,
                kind: "file".to_string(),
                text: format!("licence: {shown}"),
                value: None,
                score,
                licence_warning,
            });
            continue;
        }

        let body = src
            .get("body")
            .and_then(Value::as_str)
            .or_else(|| src.get("text").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| longest_string_field(&src));
        let (text, kind, line) = match passage::symbol_passage(&body, &src, question, PASSAGE_WIDTH)
        {
            Some((text, label)) => {
                let (kind, line) = parse_symbol_label(&label);
                (text, kind, line.unwrap_or(record_line))
            }
            None => {
                let (window, _start, _total) = passage::best_window(&body, question, PASSAGE_WIDTH);
                (window, "passage".to_string(), record_line)
            }
        };
        hits.push(Hit {
            file,
            line,
            kind,
            text,
            value: None,
            score,
            licence_warning,
        });
    }
    Answer {
        corpus: meta.corpus,
        pin_sha: meta.pin_sha,
        pin_date: meta.pin_date,
        age_days: meta.age_days,
        licence_spdx: meta.licence_spdx,
        licence_class: meta.licence_class,
        answered_from: meta.answered_from,
        mode,
        hits,
        truncated: false,
        warnings: Vec::new(),
    }
}

/// `path:line` → (`path`, `line`); a locator with no numeric `:line` suffix is
/// all path, line 0.
fn split_locator(locator: &str) -> (String, u32) {
    if let Some((path, tail)) = locator.rsplit_once(':') {
        if let Ok(n) = tail.parse::<u32>() {
            return (path.to_string(), n);
        }
    }
    (locator.to_string(), 0)
}

/// `function foo @ line 12` → (`function`, Some(12)).
fn parse_symbol_label(label: &str) -> (String, Option<u32>) {
    let kind = label.split_whitespace().next().unwrap_or("passage");
    let line = label
        .rsplit_once("@ line ")
        .and_then(|(_, n)| n.trim().parse::<u32>().ok());
    (kind.to_string(), line)
}

/// The longest non-empty string field of a record, "" when there is none — the
/// content of a raw-JSON record that has neither `body` nor `text` (the same
/// rule `xerj code`'s renderer applies).
fn longest_string_field(src: &Value) -> String {
    let mut best: Option<(usize, &str)> = None;
    if let Some(obj) = src.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(s) = obj[key].as_str() {
                let n = s.chars().count();
                if !s.trim().is_empty() && best.is_none_or(|(len, _)| n > len) {
                    best = Some((n, s));
                }
            }
        }
    }
    best.map(|(_, s)| s.to_string()).unwrap_or_default()
}

/// repo → licence from a pack's own `pack.json` (`sources[]`): the licence data
/// a machine that never cloned the corpus still has.
fn pack_licences(manifest: &PackManifest) -> HashMap<String, String> {
    manifest
        .sources
        .iter()
        .map(|s| (s.repo.clone(), s.licence.clone()))
        .collect()
}

/// The pack manifest the ledger's pin names, else the most recently used pack
/// of `slug` on disk.
fn read_pack_manifest(hub_home: &Path, slug: &str, pin_sha: &str) -> Option<PackManifest> {
    let packs: Vec<PackDir> = pack_dirs(hub_home)
        .into_iter()
        .filter(|p| p.slug == slug)
        .collect();
    let want: String = pin_sha.chars().take(8).collect::<String>().to_lowercase();
    let pack = packs
        .iter()
        .find(|p| !want.is_empty() && p.pin8 == want)
        .or_else(|| packs.iter().max_by_key(|p| p.last_used))?;
    let text = fs::read_to_string(pack.dir.join(PACK_MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

// ── the production backend ───────────────────────────────────────────────────

/// [`Backend`] over a real node and the real `hub pull`.
pub struct RealBackend {
    hub_home: PathBuf,
    url_flag: Option<String>,
    launcher: NodeLauncher,
    /// The node, resolved once per run: the outcome — success or failure — is
    /// kept, so a node that could not be started is not retried (and waited on
    /// for another minute) by the next step of the same question.
    node: RefCell<Option<Result<SharedNode, String>>>,
}

/// The resolved node and a client connected to it, shared within one run.
type SharedNode = Rc<(ResolvedNode, HubNode)>;

impl RealBackend {
    pub fn new(hub_home: PathBuf, url_flag: Option<String>, launcher: NodeLauncher) -> Self {
        RealBackend {
            hub_home,
            url_flag,
            launcher,
            node: RefCell::new(None),
        }
    }

    /// The node queries run against, resolved (and started if need be) on first
    /// use and then reused for the rest of the run.
    fn node(&self) -> Result<SharedNode, String> {
        if let Some(done) = self.node.borrow().as_ref() {
            return done.clone();
        }
        let resolved = node::ensure_node(&self.hub_home, self.url_flag.as_deref(), &self.launcher)
            .map_err(|e| format!("{e:#}"))
            .and_then(|r| {
                let client = r.connect().map_err(|e| format!("{e:#}"))?;
                Ok(Rc::new((r, client)))
            });
        *self.node.borrow_mut() = Some(resolved.clone());
        resolved
    }

    /// The How / Files path: the existing retrieval pipeline, asked for its raw
    /// response, converted to an [`Answer`].
    fn answer_search(
        &self,
        node: &HubNode,
        url: &str,
        req: &Request<'_>,
    ) -> Result<Answer, String> {
        let root = xerj_autoindex::xc::code_root();
        let mut params = CodeParams::new(req.slug, req.question);
        params.k = req.k.unwrap_or(if req.mode == AnswerMode::Files {
            FILES_K
        } else {
            DEFAULT_K
        });
        params.full = PASSAGE_WIDTH;
        params.as_json = true;
        let outcome = xccode::run_code_query(&root, node, url, &params, "`--stale-ok`");
        let resp = match outcome.json {
            Some(v) => v,
            None => return Err(outcome.text.trim().to_string()),
        };
        let meta = answer_meta(&root, req.slug);
        let licences = read_pack_manifest(&self.hub_home, req.slug, &meta.pin_sha)
            .map(|m| pack_licences(&m))
            .unwrap_or_default();
        let mut answer = answer_from_search(meta, &resp, req.question, req.mode, &licences);
        answer.warnings.extend(outcome.warnings);
        Ok(answer)
    }
}

impl Backend for RealBackend {
    fn local_pack_ready(&self, slug: &str) -> Result<bool, String> {
        // No pack on disk: not cached, and no reason to start a node to say so.
        if !pack_dirs(&self.hub_home).iter().any(|p| p.slug == slug) {
            return Ok(false);
        }
        let root = xerj_autoindex::xc::code_root();
        let Ok(st) = state::load_state(&root, slug) else {
            return Ok(false); // pack files but no ledger: re-pulling rewrites it
        };
        let Some(manifest) = read_pack_manifest(
            &self.hub_home,
            slug,
            st.pin_sha.as_deref().unwrap_or_default(),
        ) else {
            return Ok(false);
        };
        let node = self.node()?;
        let live = node
            .1
            .cat_indices_json(&format!("{}*", manifest.index_prefix))
            .unwrap_or_default();
        // Every index of the pack must be live: a partial restore would answer a
        // definition lookup "no match" for the wrong reason.
        Ok(!manifest.indices.is_empty()
            && manifest
                .indices
                .iter()
                .all(|i| live.iter().any(|n| n == &i.name)))
    }

    fn answer_local(&self, req: &Request<'_>) -> Result<Answer, String> {
        let node = self.node()?;
        let answer = match (req.mode, req.symbol) {
            (AnswerMode::Def, Some(symbol)) => {
                def::lookup(&node.1, req.slug, symbol, req.k.unwrap_or(DEFAULT_K))
            }
            _ => self.answer_search(&node.1, &node.0.url, req)?,
        };
        touch_last_used(&self.hub_home, req.slug);
        Ok(answer)
    }

    fn pull_now(&self, entry: &CatalogueEntry) -> Result<(), PullFailure> {
        // Everything that can refuse this pull — the licence policy, the size cap, a
        // malformed slug or pin, no key to verify with — is decided BEFORE a node is
        // started for it. A pull that is going to be refused must not first boot a
        // whole node. This is the AUTOMATIC path (a question triggered it), so the
        // ceiling is the 50 MiB auto-pull cap, not the disk budget an explicit
        // `xerj hub pull` uses: `pull_policy` already kept a larger pack off this
        // path, and the cap here is the backstop that keeps it off.
        let plan = cli::plan_pull_entry(entry, AUTO_PULL_MAX_BYTES).map_err(|e| PullFailure {
            code: e.code,
            message: format!("pull of '{}' failed: {}", printable(&entry.slug), e.message),
        })?;
        let node = self.node().map_err(|message| PullFailure {
            code: EXIT_NO_ANSWER,
            message,
        })?;
        eprintln!(
            "xerj hub: pulling '{}' ({} MB) — one-time download, later questions answer locally",
            entry.slug,
            megabytes(entry.pack_bytes)
        );
        match cli::pull_with(&plan, &node.0.url, node.0.managed, false) {
            Ok(cli::Pulled::Already) => Ok(()),
            Ok(cli::Pulled::Done(line)) => {
                eprintln!("xerj hub: {line}");
                Ok(())
            }
            Err(e) => Err(PullFailure {
                code: e.code,
                message: format!("pull of '{}' failed: {}", entry.slug, e.message),
            }),
        }
    }

    fn pull_in_background(&self, slug: &str) -> Result<BackgroundPull, String> {
        // The slug names files below (`pulls/<slug>.pid`) and is an argument of
        // the child: refuse anything `hub pull` itself would refuse, before it
        // touches a path. (The catalogue may be unverified until the hub key
        // ships.)
        if !cli::valid_slug(slug) {
            return Err(format!("'{slug}' is not a valid corpus slug"));
        }
        let pulls = self.hub_home.join("pulls");
        let pid_file = pulls.join(format!("{slug}.pid"));
        let needle = format!("hub pull {slug}");
        if let Some(pid) = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            if node::identify(pid, &[&needle]) == node::Identity::Ours {
                return Ok(BackgroundPull::AlreadyRunning { pid });
            }
        }
        let mut args = vec![
            "hub".to_string(),
            "pull".to_string(),
            slug.to_string(),
            "--quiet".to_string(),
        ];
        if let Some(u) = &self.url_flag {
            args.push("--url".to_string());
            args.push(u.clone());
        }
        let child = node::spawn_detached(&self.launcher, &args, &pulls.join(format!("{slug}.log")))
            .map_err(|e| format!("{e:#}"))?;
        let pid = child.id();
        fs::write(&pid_file, format!("{pid}\n")).map_err(|e| e.to_string())?;
        // The child outlives this process; dropping the handle neither kills
        // nor waits for it.
        drop(child);
        Ok(BackgroundPull::Started { pid })
    }
}

// ── the library entry point ──────────────────────────────────────────────────

/// Load the catalogue, build the production backend, and run [`ask`]. The CLI
/// prints the outcome; another caller (the MCP tool, plan §7) can use
/// [`AskOutcome::answer`] directly. `hosted` is the hosted client to use, or
/// `None` when none is configured.
pub fn run_ask(args: &AskArgs, hosted: Option<&dyn HostedHub>) -> AskOutcome {
    match NodeLauncher::current() {
        Ok(launcher) => run_ask_with(args, hosted, launcher),
        Err(e) => AskOutcome::fail(EXIT_NO_ANSWER, format!("{e:#}")),
    }
}

/// [`run_ask`] with the executable that starts the automatic node named
/// explicitly. The MCP server needs this: as the standalone `xerj-mcp` binary,
/// `current_exe()` is not a `xerj` that accepts node flags.
pub fn run_ask_with(
    args: &AskArgs,
    hosted: Option<&dyn HostedHub>,
    launcher: NodeLauncher,
) -> AskOutcome {
    let hub_home = cli::hub_home();
    let hub_url = std::env::var("XERJ_HUB_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_HUB_URL.to_string());
    // A build with no hub key can never verify a catalogue or a pack, so it
    // never fetches one: only a catalogue already on this machine is used, and
    // without one the answer is the plain no-key message (exit 5), not a parse
    // error from whatever the hub URL happens to serve.
    let catalogue = if !xerj_common::hub::hub_key_configured() {
        match load_cached_catalogue(&hub_home) {
            Ok(c) => c,
            // The cached-catalogue error says "run `xerj hub sync`", which
            // cannot help here; the no-key message is the whole answer.
            Err(_) => return AskOutcome::fail(EXIT_NO_KEY, xerj_common::hub::NO_HUB_KEY_MESSAGE),
        }
    } else if args.cached_catalogue_only {
        match load_cached_catalogue(&hub_home) {
            Ok(c) => c,
            Err(msg) => return AskOutcome::fail(EXIT_NO_ANSWER, msg),
        }
    } else {
        match load_catalogue(&hub_url, &hub_home, args.offline) {
            Ok(c) => c,
            Err(e) => {
                return AskOutcome::fail(
                    EXIT_NO_ANSWER,
                    format!("cannot load the hub catalogue: {e:#}"),
                )
            }
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let backend = RealBackend::new(hub_home.clone(), args.url.clone(), launcher);
    ask(
        args,
        &AskDeps {
            hub_home: &hub_home,
            cwd: &cwd,
            catalogue: &catalogue,
            hosted,
            backend: &backend,
        },
    )
}

// ── discovery: which corpora could answer (MCP `xerj_hub_find`) ─────────────

/// `xerj_hub_find`'s default number of candidates.
pub const FIND_DEFAULT_K: usize = 5;
/// `xerj_hub_find` never lists more than this (the hosted `/search` cap).
pub const FIND_MAX_K: usize = 10;

/// One candidate corpus: what the catalogue says about it, how strongly the
/// router proposed it, and whether its pack is already on this machine.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FoundCorpus {
    pub slug: String,
    /// The router's merged evidence score (dependency 1.0, sketch ≤ 0.8, +0.05
    /// when cached); 0 for a corpus proposed only because its definitions hold
    /// the exact symbol the question names.
    pub score: f32,
    pub cached: bool,
    pub ask_for: String,
    pub domain: String,
    pub pin_sha: String,
    pub pin_date: String,
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub pack_bytes: u64,
}

/// What [`find`] decided: the router's verdict and the candidates, best first.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FindOutcome {
    /// `confident` | `ambiguous` | `no_corpus` — the verdict `xerj ask` would act on.
    pub routing: &'static str,
    pub corpora: Vec<FoundCorpus>,
}

/// Which corpora could answer `question` — the routing half of [`ask`], with
/// nothing answered and nothing pulled. The evidence is exactly what
/// [`ask`]'s corpus selection uses before it consults a hosted node: the
/// project's dependencies (from `cwd`), the catalogue's term sketches, the
/// exact-symbol evidence for a definition question, and the cached-pack bonus.
/// An `excluded` corpus is never listed (it is never served either).
pub fn find(
    question: &str,
    k: usize,
    cwd: &Path,
    hub_home: &Path,
    catalogue: &Catalogue,
) -> FindOutcome {
    let k = k.clamp(1, FIND_MAX_K);
    let catalogue = &routable(catalogue);
    let cached = cached_slugs(hub_home);
    let (_, symbol) = detect_mode(question, &ModeRequest::Auto);
    let by_symbol = symbol
        .map(|s| symbol_hits(catalogue, s.trim()))
        .unwrap_or_default();
    let (routing, ranked) = route_ranked(question, cwd, catalogue, None, &cached);
    let routing = with_symbol_evidence(routing, &by_symbol);
    // The verdict's own order first (a confident pick, or the ambiguous list),
    // then everything else the router ranked, then symbol-only candidates.
    let mut order: Vec<String> = match &routing {
        Routing::Confident(slug) => vec![slug.clone()],
        Routing::Ambiguous(slugs) => slugs.clone(),
        Routing::NoCorpus => Vec::new(),
    };
    for slug in ranked.iter().map(|(s, _)| s).chain(by_symbol.iter()) {
        if !order.contains(slug) {
            order.push(slug.clone());
        }
    }
    let score_of = |slug: &str| {
        ranked
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, score)| *score)
            .unwrap_or(0.0)
    };
    let corpora = order
        .iter()
        .filter_map(|slug| catalogue.corpora.iter().find(|e| &e.slug == slug))
        .filter(|e| e.licence_class != LicenceClass::Excluded)
        .take(k)
        .map(|e| FoundCorpus {
            slug: e.slug.clone(),
            score: score_of(&e.slug),
            cached: cached.contains(&e.slug),
            ask_for: e.ask_for.clone(),
            domain: e.domain.clone(),
            pin_sha: e.pin_sha.clone(),
            pin_date: e.pin_date.clone(),
            licence_spdx: e.licence_spdx.clone(),
            licence_class: e.licence_class,
            pack_bytes: e.pack_bytes,
        })
        .collect();
    FindOutcome {
        routing: match routing {
            Routing::Confident(_) => "confident",
            Routing::Ambiguous(_) => "ambiguous",
            Routing::NoCorpus => "no_corpus",
        },
        corpora,
    }
}

/// What a caller that never fetches the catalogue (the MCP tools) says when
/// none is cached on this machine.
pub const NO_CACHED_CATALOGUE: &str = "no hub catalogue is cached on this machine, and hub tools \
     never fetch one (a tool call does not wait on the network); run `xerj hub sync` in a shell \
     once, then call the tool again";

/// The catalogue cached in `hub_home`, with no network access at all, however
/// old it is (an old copy still warns on stderr). `Err` is
/// [`NO_CACHED_CATALOGUE`] plus the underlying reason.
pub fn load_cached_catalogue(hub_home: &Path) -> Result<Catalogue, String> {
    // `offline = true` never builds an HTTP client; the URL is unused.
    load_catalogue(DEFAULT_HUB_URL, hub_home, true)
        .map_err(|e| format!("{NO_CACHED_CATALOGUE} ({e:#})"))
}

/// Load the catalogue already cached on this machine — never fetching it
/// ([`load_cached_catalogue`]) — and run [`find`] from the current directory.
/// Only the MCP `xerj_hub_find` tool calls this.
pub fn run_find(question: &str, k: usize) -> Result<FindOutcome, String> {
    let hub_home = cli::hub_home();
    let catalogue = load_cached_catalogue(&hub_home).map_err(|e| {
        if xerj_common::hub::hub_key_configured() {
            e
        } else {
            // `xerj hub sync` cannot help in a build with no key: say that,
            // not "run xerj hub sync".
            xerj_common::hub::NO_HUB_KEY_MESSAGE.to_string()
        }
    })?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(find(question, k, &cwd, &hub_home, &catalogue))
}

/// Render a [`FindOutcome`] as text, one corpus per line (catalogue strings
/// made [`printable`]).
pub fn render_find(found: &FindOutcome) -> String {
    if found.corpora.is_empty() {
        return NO_CORPUS_MESSAGE.to_string();
    }
    let mut lines: Vec<String> = found
        .corpora
        .iter()
        .map(|c| {
            format!(
                "{} — {}  (score {:.2}, pin {}, {}{})",
                printable(&c.slug),
                printable(&c.ask_for),
                c.score,
                printable(&c.pin_date),
                printable(&c.licence_spdx),
                if c.cached { ", cached" } else { "" }
            )
        })
        .collect();
    lines.push(format!(
        "routing: {}; ask one with xerj_hub_ask {{question, corpus}}",
        found.routing
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use serde_json::json;
    use xerj_common::hub::Sketch;

    // ── fixtures ────────────────────────────────────────────────────────────

    fn sketch(terms: &[&str]) -> Sketch {
        crate::sketch::build(terms.iter().map(|t| t.to_string()))
    }

    fn entry(slug: &str, bytes: u64, class: LicenceClass, terms: &[&str]) -> CatalogueEntry {
        CatalogueEntry {
            slug: slug.to_string(),
            domain: "TEST".to_string(),
            topics: vec![],
            ask_for: format!("an agent working on {slug} queries this for its internals"),
            pin_sha: "0".repeat(40),
            pin_date: "2026-10-01".to_string(),
            refresh_cadence_days: 7,
            licence_spdx: if class == LicenceClass::ApproachOnly {
                "GPL-3.0".to_string()
            } else {
                "MIT".to_string()
            },
            licence_class: class,
            pack_bytes: bytes,
            pack_url: String::new(),
            pack_sha256: String::new(),
            sig_url: String::new(),
            dependency_names: vec![],
            quality_grade: None,
            term_sketch: sketch(terms),
        }
    }

    fn lz4(bytes: u64, class: LicenceClass) -> CatalogueEntry {
        entry(
            "lz4",
            bytes,
            class,
            &["lz4", "block", "decoder", "compression"],
        )
    }

    /// Four unrelated corpora, so that a term found in only one of the others
    /// is not "common vocabulary" (the router halves terms present in more than
    /// a quarter of all sketches) and a sketch match can be decisive.
    fn with_fillers(mut entries: Vec<CatalogueEntry>) -> Vec<CatalogueEntry> {
        for (slug, terms) in [
            ("filler-a", ["alpha", "bravo", "charlie"]),
            ("filler-b", ["delta", "echo", "foxtrot"]),
            ("filler-c", ["golf", "hotel", "india"]),
            ("filler-d", ["juliet", "kilo", "lima"]),
        ] {
            entries.push(entry(slug, 1, LicenceClass::AdaptWithAttribution, &terms));
        }
        entries
    }

    fn catalogue(entries: Vec<CatalogueEntry>) -> Catalogue {
        Catalogue {
            format_version: 1,
            generated_at: "2026-10-14T00:00:00Z".to_string(),
            corpora: entries,
        }
    }

    fn canned(slug: &str, from: AnsweredFrom, mode: AnswerMode) -> Answer {
        Answer {
            corpus: slug.to_string(),
            pin_sha: "0".repeat(40),
            pin_date: "2026-10-01".to_string(),
            age_days: 13,
            licence_spdx: "MIT".to_string(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: from,
            mode,
            hits: vec![Hit {
                file: "lz4/lib/lz4.h".to_string(),
                line: 100,
                kind: "define".to_string(),
                text: "#define LZ4_VERSION_MAJOR 1".to_string(),
                value: Some("1".to_string()),
                score: 1.0,
                licence_warning: None,
            }],
            truncated: false,
            warnings: vec![],
        }
    }

    /// A backend that records every call and never touches a node.
    struct FakeBackend {
        ready: Cell<bool>,
        ready_err: Option<String>,
        pull_fail: RefCell<Option<PullFailure>>,
        log: RefCell<Vec<String>>,
    }

    impl FakeBackend {
        fn new(ready: bool) -> Self {
            FakeBackend {
                ready: Cell::new(ready),
                ready_err: None,
                pull_fail: RefCell::new(None),
                log: RefCell::new(vec![]),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
        fn called(&self, prefix: &str) -> bool {
            self.log.borrow().iter().any(|c| c.starts_with(prefix))
        }
    }

    impl Backend for FakeBackend {
        fn local_pack_ready(&self, slug: &str) -> Result<bool, String> {
            self.log.borrow_mut().push(format!("ready:{slug}"));
            match &self.ready_err {
                Some(e) => Err(e.clone()),
                None => Ok(self.ready.get()),
            }
        }
        fn answer_local(&self, req: &Request<'_>) -> Result<Answer, String> {
            self.log
                .borrow_mut()
                .push(format!("answer_local:{}", req.slug));
            Ok(canned(req.slug, AnsweredFrom::LocalPack, req.mode))
        }
        fn pull_now(&self, entry: &CatalogueEntry) -> Result<(), PullFailure> {
            self.log
                .borrow_mut()
                .push(format!("pull_now:{}", entry.slug));
            if let Some(f) = self.pull_fail.borrow().clone() {
                return Err(f);
            }
            self.ready.set(true);
            Ok(())
        }
        fn pull_in_background(&self, slug: &str) -> Result<BackgroundPull, String> {
            self.log.borrow_mut().push(format!("pull_bg:{slug}"));
            Ok(BackgroundPull::Started { pid: 4242 })
        }
    }

    /// A hosted hub that fails the test if it is ever contacted.
    struct NoNetwork;
    impl HostedSearch for NoNetwork {
        fn search(&self, _q: &str) -> Result<Vec<(String, f32)>> {
            panic!("hosted search must not be called");
        }
    }
    impl HostedHub for NoNetwork {
        fn answer(&self, _req: &Request<'_>) -> Result<Answer> {
            panic!("hosted answer must not be called");
        }
    }

    /// A hosted hub that answers from a canned response and counts its calls.
    struct StubHosted {
        found: Vec<(String, f32)>,
        fail: bool,
        /// Attribute the answer to this corpus instead of the one asked about —
        /// what a buggy or hostile hosted node could do.
        claim: Option<String>,
        searches: Cell<usize>,
        answers: Cell<usize>,
    }
    impl StubHosted {
        fn new() -> Self {
            StubHosted {
                found: vec![],
                fail: false,
                claim: None,
                searches: Cell::new(0),
                answers: Cell::new(0),
            }
        }
    }
    impl HostedSearch for StubHosted {
        fn search(&self, _q: &str) -> Result<Vec<(String, f32)>> {
            self.searches.set(self.searches.get() + 1);
            Ok(self.found.clone())
        }
    }
    impl HostedHub for StubHosted {
        fn answer(&self, req: &Request<'_>) -> Result<Answer> {
            self.answers.set(self.answers.get() + 1);
            if self.fail {
                anyhow::bail!("hosted node unreachable");
            }
            // Deliberately mislabelled: `ask` must stamp it `Hosted` itself.
            let mut a = canned(req.slug, AnsweredFrom::LocalPack, req.mode);
            if let Some(other) = &self.claim {
                a.corpus = other.clone();
            }
            Ok(a)
        }
    }

    /// Everything an `ask` run needs, in throwaway directories.
    struct Rig {
        hub: tempfile::TempDir,
        cwd: tempfile::TempDir,
    }
    impl Rig {
        fn new() -> Self {
            Rig {
                hub: tempfile::tempdir().unwrap(),
                cwd: tempfile::tempdir().unwrap(),
            }
        }
        /// Make `cwd` look like a git work tree.
        fn in_git_repo(self) -> Self {
            fs::create_dir(self.cwd.path().join(".git")).unwrap();
            self
        }
        fn run(
            &self,
            args: &AskArgs,
            cat: &Catalogue,
            hosted: Option<&dyn HostedHub>,
            backend: &dyn Backend,
        ) -> AskOutcome {
            ask(
                args,
                &AskDeps {
                    hub_home: self.hub.path(),
                    cwd: self.cwd.path(),
                    catalogue: cat,
                    hosted,
                    backend,
                },
            )
        }
    }

    fn args(question: &str, corpus: Option<&str>) -> AskArgs {
        let mut a = AskArgs::new(question);
        a.corpus = corpus.map(str::to_string);
        a
    }

    // ── step 1: mode ────────────────────────────────────────────────────────

    #[test]
    fn mode_is_def_for_the_flag_a_lone_identifier_and_the_value_of_phrase() {
        let auto = ModeRequest::Auto;
        assert_eq!(
            detect_mode("x", &ModeRequest::Def("LZ4_VERSION".into())),
            (AnswerMode::Def, Some("LZ4_VERSION".to_string()))
        );
        assert_eq!(
            detect_mode("LZ4_VERSION_RELEASE", &auto),
            (AnswerMode::Def, Some("LZ4_VERSION_RELEASE".to_string()))
        );
        assert_eq!(
            detect_mode("  mdb_maxkeysize\n", &auto),
            (AnswerMode::Def, Some("mdb_maxkeysize".to_string()))
        );
        // The phrase, matched anywhere and on its fixed words case-insensitively.
        for q in [
            "what is the value of MDB_MAXKEYSIZE",
            "What is the value of MDB_MAXKEYSIZE?",
            "in lmdb, what is the exact value of MDB_MAXKEYSIZE in a release build",
            "WHAT IS THE EXACT VALUE OF MDB_MAXKEYSIZE",
        ] {
            assert_eq!(
                detect_mode(q, &auto),
                (AnswerMode::Def, Some("MDB_MAXKEYSIZE".to_string())),
                "{q}"
            );
        }
    }

    #[test]
    fn mode_is_how_for_prose_and_flags_force_the_rest() {
        let auto = ModeRequest::Auto;
        for q in [
            "how does the lz4 frame decoder work",
            "lz4 decoder",              // two words: not one identifier
            "what is the value of",     // no symbol after the phrase
            "what is the value of ???", // no word character after the phrase
            "LZ4-VERSION",              // a hyphen is not an identifier character
            "",
        ] {
            assert_eq!(detect_mode(q, &auto), (AnswerMode::How, None), "{q:?}");
        }
        // A forced mode ignores what the question looks like.
        assert_eq!(
            detect_mode("LZ4_VERSION_RELEASE", &ModeRequest::How),
            (AnswerMode::How, None)
        );
        assert_eq!(
            detect_mode("LZ4_VERSION_RELEASE", &ModeRequest::Files),
            (AnswerMode::Files, None)
        );
    }

    // ── step 3: a cached pack never touches the network ─────────────────────

    #[test]
    fn a_cached_pack_answers_locally_and_never_contacts_the_hosted_hub() {
        let rig = Rig::new();
        let cat = catalogue(with_fillers(vec![
            lz4(1024, LicenceClass::AdaptWithAttribution),
            entry(
                "zstd",
                1024,
                LicenceClass::AdaptWithAttribution,
                &["zstd", "frame"],
            ),
        ]));
        let backend = FakeBackend::new(true);

        // Routed by the sketch (no --corpus): hosted is on (cwd is not a repo)
        // but the sketch is decisive, so the hosted node is never asked.
        let out = rig.run(
            &args("lz4 block decoder", None),
            &cat,
            Some(&NoNetwork),
            &backend,
        );
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        let ans = out.answer.expect("an answer");
        assert_eq!(ans.answered_from, AnsweredFrom::LocalPack);
        assert!(
            out.stdout.contains("answered from: local pack"),
            "{}",
            out.stdout
        );
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);

        // And with --corpus (routing skipped altogether).
        let backend = FakeBackend::new(true);
        let out = rig.run(
            &args("anything", Some("lz4")),
            &cat,
            Some(&NoNetwork),
            &backend,
        );
        assert_eq!(out.exit, EXIT_OK);
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);
    }

    #[test]
    fn hosted_is_consulted_for_routing_only_when_the_sketch_is_not_decisive() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        // No word of the question is in any sketch: sketch routing finds nothing.
        let q = "completely unrelated wording here";

        let none = rig.run(&args(q, None), &cat, None, &FakeBackend::new(true));
        assert_eq!(none.exit, EXIT_NO_ANSWER);
        assert!(
            none.stderr.contains(&NO_CORPUS_MESSAGE.to_string()),
            "{:?}",
            none.stderr
        );

        // With a hosted node that recognises it, routing becomes confident.
        let mut hosted = StubHosted::new();
        hosted.found = vec![("lz4".to_string(), 7.0)];
        let backend = FakeBackend::new(true);
        let out = rig.run(&args(q, None), &cat, Some(&hosted), &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(hosted.searches.get(), 1, "one federated search");
        assert_eq!(
            hosted.answers.get(),
            0,
            "the pack was cached: no hosted answer"
        );
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);
    }

    // ── step 2: routing outcomes ────────────────────────────────────────────

    #[test]
    fn ambiguous_prints_one_candidate_per_line_and_exits_4() {
        let rig = Rig::new();
        // Two corpora that both hold every word of the question.
        let cat = catalogue(vec![
            entry(
                "alpha",
                1,
                LicenceClass::AdaptWithAttribution,
                &["tokio", "runtime"],
            ),
            entry(
                "beta",
                1,
                LicenceClass::AdaptWithAttribution,
                &["tokio", "runtime"],
            ),
        ]);
        let backend = FakeBackend::new(true);
        let out = rig.run(&args("tokio runtime", None), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_NEEDS_DECISION, "{:?}", out);
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(lines.len(), 2, "{}", out.stdout);
        assert!(
            lines[0].starts_with("alpha — an agent working on alpha"),
            "{}",
            lines[0]
        );
        assert!(lines[1].starts_with("beta — "), "{}", lines[1]);
        assert!(out.stderr.iter().any(|l| l.contains("--corpus")));
        assert!(backend.calls().is_empty(), "nothing is answered or pulled");
    }

    #[test]
    fn no_corpus_exits_1_with_the_llms_txt_pointer() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1, LicenceClass::AdaptWithAttribution)]);
        let out = rig.run(
            &args("gardening tips for tomatoes", None),
            &cat,
            None,
            &FakeBackend::new(false),
        );
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        assert_eq!(out.stderr, [NO_CORPUS_MESSAGE]);
        assert_eq!(
            NO_CORPUS_MESSAGE,
            "no hub corpus matches; see xerj.org/llms.txt"
        );
    }

    #[test]
    fn an_unknown_corpus_flag_is_a_usage_error_naming_the_catalogue_date() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(true);
        let out = rig.run(&args("q", Some("nope")), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_USAGE);
        assert!(
            out.stderr[0].contains("unknown corpus 'nope'"),
            "{:?}",
            out.stderr
        );
        assert!(out.stderr[0].contains("2026-10-14"), "{:?}", out.stderr);
        assert!(backend.calls().is_empty());
    }

    // ── step 4: hosted answers now, the pull follows ────────────────────────

    #[test]
    fn hosted_answers_now_and_starts_the_pull_behind_it() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let hosted = StubHosted::new();
        let backend = FakeBackend::new(false);

        let out = rig.run(&args("q", Some("lz4")), &cat, Some(&hosted), &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(out.answer.unwrap().answered_from, AnsweredFrom::Hosted);
        assert!(
            out.stdout.contains("answered from: hosted hub"),
            "{}",
            out.stdout
        );
        assert_eq!(backend.calls(), ["ready:lz4", "pull_bg:lz4"]);
        assert!(
            out.stderr.iter().any(|l| l.contains("background")),
            "{:?}",
            out.stderr
        );
        assert_eq!(hosted.answers.get(), 1);
    }

    #[test]
    fn no_fetch_answers_hosted_without_starting_a_pull() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(false);
        let mut a = args("q", Some("lz4"));
        a.no_fetch = true;
        let out = rig.run(&a, &cat, Some(&StubHosted::new()), &backend);
        assert_eq!(out.exit, EXIT_OK);
        assert!(out.stdout.contains("answered from: hosted hub"));
        assert!(!backend.called("pull_"), "{:?}", backend.calls());
    }

    #[test]
    fn a_pack_over_the_limit_is_not_auto_pulled_but_the_note_names_the_explicit_pull() {
        let rig = Rig::new();
        let sixty_mib = 60 * 1024 * 1024;
        let cat = catalogue(vec![lz4(sixty_mib, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(false);
        let out = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&StubHosted::new()),
            &backend,
        );
        assert_eq!(out.exit, EXIT_OK);
        // A question still does not AUTO-pull an oversize pack ...
        assert!(!backend.called("pull_"), "{:?}", backend.calls());
        // ... the note names the size and says the answer is from the hosted hub ...
        assert!(out.stdout.contains("60.0 MB"), "{}", out.stdout);
        let ans = out.answer.unwrap();
        assert!(
            ans.warnings
                .iter()
                .any(|w| w.contains("60.0 MB") && w.contains("hosted hub")),
            "{:?}",
            ans.warnings
        );
        // ... and, now that an explicit pull fetches an oversize pack, it names
        // `xerj hub pull` so the reader can get local answers.
        assert!(out.stdout.contains("xerj hub pull lz4"), "{}", out.stdout);
    }

    #[test]
    fn an_excluded_corpus_is_never_pulled() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::Excluded)]);
        // Neither the hosted node (it never serves an excluded corpus) nor a pull:
        // with or without a hosted hub, there is nothing to say but no.
        for hosted in [None, Some(StubHosted::new())] {
            let backend = FakeBackend::new(false);
            let out = rig.run(
                &args("q", Some("lz4")),
                &cat,
                hosted.as_ref().map(|h| h as &dyn HostedHub),
                &backend,
            );
            assert_eq!(out.exit, EXIT_NO_ANSWER);
            assert!(!backend.called("pull_"), "{:?}", backend.calls());
            assert!(
                out.stderr[0].contains("excluded licence class (MIT)")
                    && out.stderr[0].contains("never served or pulled"),
                "{:?}",
                out.stderr
            );
            if let Some(h) = &hosted {
                assert_eq!(h.answers.get(), 0);
            }
        }
    }

    #[test]
    fn an_approach_only_corpus_is_pulled_and_every_answer_carries_the_warning() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::ApproachOnly)]);
        let want = "approach-only licence (GPL-3.0): read, do not copy";

        // Hosted answer: pulled behind it, warning on the answer and on the hit.
        let backend = FakeBackend::new(false);
        let out = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&StubHosted::new()),
            &backend,
        );
        assert!(backend.called("pull_bg:lz4"));
        assert!(
            out.stdout.contains(&format!("warning: {want}")),
            "{}",
            out.stdout
        );
        let ans = out.answer.unwrap();
        assert_eq!(ans.hits[0].licence_warning.as_deref(), Some(want));
        assert_eq!(ans.warnings.iter().filter(|w| *w == want).count(), 1);

        // Local answer: the same.
        let backend = FakeBackend::new(true);
        let out = rig.run(&args("q", Some("lz4")), &cat, None, &backend);
        assert!(
            out.stdout.contains(&format!("warning: {want}")),
            "{}",
            out.stdout
        );
    }

    #[test]
    fn a_failing_hosted_answer_falls_back_to_the_synchronous_pull() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let mut hosted = StubHosted::new();
        hosted.fail = true;
        let backend = FakeBackend::new(false);
        let out = rig.run(&args("q", Some("lz4")), &cat, Some(&hosted), &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert!(out
            .stderr
            .iter()
            .any(|l| l.contains("hosted answer failed")));
        assert_eq!(
            backend.calls(),
            ["ready:lz4", "pull_now:lz4", "answer_local:lz4"]
        );
        assert_eq!(out.answer.unwrap().answered_from, AnsweredFrom::LocalPack);
    }

    // ── step 5: no pack, no hosted ──────────────────────────────────────────

    #[test]
    fn offline_with_no_pack_exits_1_naming_the_pack_to_pull() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(
            12 * 1024 * 1024,
            LicenceClass::AdaptWithAttribution,
        )]);
        let backend = FakeBackend::new(false);
        let mut a = args("q", Some("lz4"));
        a.offline = true;
        // --offline: the hosted hub must not be contacted even though it is on.
        let out = rig.run(&a, &cat, Some(&NoNetwork), &backend);
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        assert!(out.stdout.is_empty());
        let msg = out.stderr.join("\n");
        assert!(msg.contains("--offline"), "{msg}");
        assert!(msg.contains("run: xerj hub pull lz4 (12.0 MB)"), "{msg}");
        assert_eq!(backend.calls(), ["ready:lz4"], "no pull, no answer");
    }

    #[test]
    fn offline_still_answers_from_a_cached_pack() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(true);
        let mut a = args("q", Some("lz4"));
        a.offline = true;
        let out = rig.run(&a, &cat, Some(&NoNetwork), &backend);
        assert_eq!(out.exit, EXIT_OK);
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);
    }

    #[test]
    fn with_no_hosted_the_first_question_pulls_synchronously_then_answers_locally() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(false);
        let out = rig.run(&args("q", Some("lz4")), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(
            backend.calls(),
            ["ready:lz4", "pull_now:lz4", "answer_local:lz4"]
        );
        assert!(out.stdout.contains("answered from: local pack"));
    }

    #[test]
    fn a_failed_pull_keeps_its_exit_code() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        for code in [1, 2, 3] {
            let backend = FakeBackend::new(false);
            *backend.pull_fail.borrow_mut() = Some(PullFailure {
                code,
                message: format!("pull failed {code}"),
            });
            let out = rig.run(&args("q", Some("lz4")), &cat, None, &backend);
            assert_eq!(out.exit, code);
            assert_eq!(
                out.stderr.last().map(String::as_str),
                Some(&*format!("pull failed {code}"))
            );
            assert!(!backend.called("answer_local"), "{:?}", backend.calls());
        }
    }

    #[test]
    fn the_policy_also_binds_the_synchronous_pull() {
        let rig = Rig::new();
        // Over the cap: no AUTOMATIC pull, but the message names the size and the
        // explicit command that does fetch it.
        let cat = catalogue(vec![lz4(
            80 * 1024 * 1024,
            LicenceClass::AdaptWithAttribution,
        )]);
        let backend = FakeBackend::new(false);
        let out = rig.run(&args("q", Some("lz4")), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        assert!(!backend.called("pull_now"), "{:?}", backend.calls());
        assert!(out.stderr[0].contains("80.0 MB"), "{:?}", out.stderr);
        assert!(
            out.stderr[0].contains("xerj hub pull lz4"),
            "names the explicit pull for an over-cap pack: {:?}",
            out.stderr
        );

        // --no-fetch means no download at all, even with nothing else to answer from.
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(false);
        let mut a = args("q", Some("lz4"));
        a.no_fetch = true;
        let out = rig.run(&a, &cat, None, &backend);
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        assert!(out.stderr[0].contains("--no-fetch"), "{:?}", out.stderr);
        assert!(!backend.called("pull_now"));
    }

    #[test]
    fn a_node_that_cannot_be_reached_falls_through_to_the_hosted_answer() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let mut backend = FakeBackend::new(true);
        backend.ready_err = Some("no node answers".to_string());
        let out = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&StubHosted::new()),
            &backend,
        );
        assert_eq!(out.exit, EXIT_OK);
        assert!(out.stderr[0].contains("local pack unavailable: no node answers"));
        assert!(out.stdout.contains("answered from: hosted hub"));
    }

    // ── the pull policy comes before --offline / --no-fetch ─────────────────

    /// The two refusals that must never be answered with a command.
    fn no_pull_command(out: &AskOutcome) {
        let all = format!("{}\n{}", out.stdout, out.stderr.join("\n"));
        assert!(!all.contains("xerj hub pull"), "no pull hint at all: {all}");
        assert!(!all.contains("run:"), "no command to run: {all}");
    }

    #[test]
    fn offline_never_hints_a_pull_for_an_excluded_corpus() {
        // The bypass: `--offline` returned before the pull policy and printed
        // `run: xerj hub pull <slug>` for an excluded corpus; `hub pull` then had no
        // licence check of its own.
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(12 * 1024 * 1024, LicenceClass::Excluded)]);
        for (flag, name) in [(true, "--offline"), (false, "--no-fetch")] {
            let backend = FakeBackend::new(false);
            let mut a = args("q", Some("lz4"));
            if flag {
                a.offline = true;
            } else {
                a.no_fetch = true;
            }
            let out = rig.run(&a, &cat, None, &backend);
            assert_eq!(out.exit, EXIT_NO_ANSWER, "{name}");
            assert!(
                out.stderr[0].contains("excluded licence class")
                    && out.stderr[0].contains("never served or pulled"),
                "{name}: {:?}",
                out.stderr
            );
            no_pull_command(&out);
            assert!(backend.calls().is_empty(), "{name}: nothing read or pulled");
        }
    }

    #[test]
    fn offline_names_the_explicit_pull_for_a_pack_over_the_cap() {
        // An oversize pack is not auto-pulled, but `xerj hub pull` fetches it
        // explicitly — so, like any pullable pack, the offline/no-fetch message
        // names that command (it is the thing to run once back online).
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(
            80 * 1024 * 1024,
            LicenceClass::AdaptWithAttribution,
        )]);
        for offline in [true, false] {
            let backend = FakeBackend::new(false);
            let mut a = args("q", Some("lz4"));
            if offline {
                a.offline = true;
            } else {
                a.no_fetch = true;
            }
            let out = rig.run(&a, &cat, None, &backend);
            assert_eq!(out.exit, EXIT_NO_ANSWER);
            assert!(out.stderr[0].contains("80.0 MB"), "{:?}", out.stderr);
            assert!(
                out.stderr[0].contains("xerj hub pull lz4"),
                "names the explicit pull: {:?}",
                out.stderr
            );
            assert!(
                !backend.called("pull_"),
                "not auto-pulled: {:?}",
                backend.calls()
            );
        }
    }

    #[test]
    fn offline_still_hints_the_pull_for_a_pack_that_may_be_pulled() {
        // The hint is for the case where it works.
        let rig = Rig::new();
        for class in [
            LicenceClass::AdaptWithAttribution,
            LicenceClass::ApproachOnly,
        ] {
            let cat = catalogue(vec![lz4(12 * 1024 * 1024, class)]);
            let mut a = args("q", Some("lz4"));
            a.offline = true;
            let out = rig.run(&a, &cat, None, &FakeBackend::new(false));
            assert!(
                out.stderr
                    .join("\n")
                    .contains("run: xerj hub pull lz4 (12.0 MB)"),
                "{:?}",
                out.stderr
            );
        }
    }

    #[test]
    fn a_named_excluded_corpus_is_refused_with_no_pull_command_either() {
        // The hosted node is not asked: it never serves an excluded corpus.
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(80 * 1024 * 1024, LicenceClass::Excluded)]);
        let backend = FakeBackend::new(false);
        let hosted = StubHosted::new();
        let out = rig.run(&args("q", Some("lz4")), &cat, Some(&hosted), &backend);
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        assert!(
            out.stderr[0].contains("never served or pulled"),
            "{:?}",
            out.stderr
        );
        no_pull_command(&out);
        assert!(backend.calls().is_empty(), "{:?}", backend.calls());
        assert_eq!(hosted.answers.get(), 0);
    }

    // ── hosted answers are attributed to the corpus that was asked ──────────

    #[test]
    fn a_hosted_answer_is_attributed_to_the_corpus_asked_about_not_the_one_it_claims() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let mut hosted = StubHosted::new();
        hosted.claim = Some("some-other-corpus".to_string());
        let out = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&hosted),
            &FakeBackend::new(false),
        );
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(out.answer.as_ref().unwrap().corpus, "lz4");
        assert!(out.stdout.starts_with("lz4  pin "), "{}", out.stdout);
        assert!(!out.stdout.contains("some-other-corpus"), "{}", out.stdout);
    }

    // ── nothing a catalogue says reaches the terminal as control codes ──────

    #[test]
    fn printable_removes_control_characters_and_collapses_what_is_left() {
        assert_eq!(
            printable("an agent working on lz4"),
            "an agent working on lz4"
        );
        assert_eq!(printable("a\nb\r\nc\td"), "a b c d");
        // An escape sequence cannot clear the screen or recolour the line.
        assert_eq!(printable("x\u{1b}[2J\u{1b}[31my"), "x [2J [31my");
        assert!(!printable("\u{1b}]0;title\u{7}z").contains('\u{1b}'));
        assert!(!printable("nul\0byte\u{7f}del\u{85}c1")
            .chars()
            .any(char::is_control));
        // Bidirectional overrides and line separators that reorder or split a line.
        let sneaky = printable("safe\u{202E}evil\u{2028}next\u{2066}x");
        assert!(
            !sneaky
                .chars()
                .any(|c| matches!(c, '\u{202E}' | '\u{2028}' | '\u{2066}')),
            "{sneaky:?}"
        );
        assert_eq!(printable("  padded  "), "padded");
        assert_eq!(printable(""), "");
        // Ordinary non-ASCII text is untouched.
        assert_eq!(printable("sürf — 日本語"), "sürf — 日本語");
    }

    #[test]
    fn candidate_lines_never_carry_terminal_control_codes() {
        let rig = Rig::new();
        let mut a = entry(
            "alpha",
            1,
            LicenceClass::AdaptWithAttribution,
            &["tokio", "runtime"],
        );
        a.ask_for = "fine\n\u{1b}[2Jfake line\r\nanother".to_string();
        let mut b = entry(
            "beta",
            1,
            LicenceClass::AdaptWithAttribution,
            &["tokio", "runtime"],
        );
        b.slug = "be\u{1b}[31mta".to_string();
        let cat = catalogue(vec![a, b]);
        let out = rig.run(
            &args("tokio runtime", None),
            &cat,
            None,
            &FakeBackend::new(true),
        );
        assert_eq!(out.exit, EXIT_NEEDS_DECISION, "{out:?}");
        assert_eq!(
            out.stdout.lines().count(),
            2,
            "one line per candidate: {:?}",
            out.stdout
        );
        assert!(
            !out.stdout.chars().any(|c| c.is_control() && c != '\n'),
            "{:?}",
            out.stdout
        );
    }

    // ── routing on the symbol, and naming the closest on a miss ─────────────

    /// A catalogue where `lz4` defines `LZ4_VERSION_RELEASE` and the fillers do not.
    fn symbol_catalogue() -> Catalogue {
        catalogue(with_fillers(vec![
            entry(
                "lz4",
                1024,
                LicenceClass::AdaptWithAttribution,
                &[
                    "lz4",
                    "block",
                    "decoder",
                    "LZ4_VERSION_RELEASE",
                    "LZ4_compress_default",
                ],
            ),
            entry(
                "zstd",
                1024,
                LicenceClass::AdaptWithAttribution,
                &["zstd", "frame", "ZSTD_VERSION_RELEASE"],
            ),
        ]))
    }

    #[test]
    fn a_definition_question_routes_on_the_symbol_the_words_around_it_cannot() {
        let rig = Rig::new();
        let cat = symbol_catalogue();
        let q = "what is the exact value of LZ4_VERSION_RELEASE";
        // The sentence alone: the symbol is one of six words and its lower-case
        // twin never matches, so the score cannot reach "confident".
        let cached: Vec<String> = vec![];
        assert!(
            matches!(
                crate::router::route(q, rig.cwd.path(), &cat, None, &cached),
                Routing::NoCorpus
            ),
            "the words alone do not route"
        );
        // The flow knows the question is about that symbol, and the symbol is
        // defined by exactly one corpus.
        let backend = FakeBackend::new(true);
        let out = rig.run(&args(q, None), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);

        // `--def SYM` with an unrelated sentence routes the same way.
        let mut a = args("tell me about it", None);
        a.mode = ModeRequest::Def("LZ4_compress_default".to_string());
        let backend = FakeBackend::new(true);
        let out = rig.run(&a, &cat, None, &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);

        // And a bare identifier, the shortest definition question.
        let backend = FakeBackend::new(true);
        let out = rig.run(&args("ZSTD_VERSION_RELEASE", None), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(backend.calls(), ["ready:zstd", "answer_local:zstd"]);
    }

    #[test]
    fn a_symbol_defined_by_several_corpora_is_ambiguous_not_a_guess() {
        let rig = Rig::new();
        let cat = catalogue(with_fillers(vec![
            entry(
                "boringssl",
                1,
                LicenceClass::AdaptWithAttribution,
                &["ssl", "SSL_CTX_new"],
            ),
            entry(
                "openssl",
                1,
                LicenceClass::AdaptWithAttribution,
                &["ssl", "SSL_CTX_new"],
            ),
        ]));
        let backend = FakeBackend::new(true);
        let out = rig.run(
            &args("what is the value of SSL_CTX_new", None),
            &cat,
            None,
            &backend,
        );
        assert_eq!(out.exit, EXIT_NEEDS_DECISION, "{out:?}");
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(lines.len(), 2, "{:?}", out.stdout);
        assert!(lines[0].starts_with("boringssl — "), "{}", lines[0]);
        assert!(lines[1].starts_with("openssl — "), "{}", lines[1]);
        assert!(backend.calls().is_empty());
    }

    #[test]
    fn a_symbol_every_corpus_defines_is_no_evidence() {
        // `main`-like names say nothing about which corpus is meant.
        let rig = Rig::new();
        let entries: Vec<CatalogueEntry> = (0..MAX_SYMBOL_CANDIDATES + 1)
            .map(|i| {
                entry(
                    &format!("c{i}"),
                    1,
                    LicenceClass::AdaptWithAttribution,
                    &["main_loop"],
                )
            })
            .collect();
        let cat = catalogue(entries);
        assert!(symbol_hits(&cat, "main_loop").is_empty());
        let out = rig.run(
            &args("main_loop", None),
            &cat,
            None,
            &FakeBackend::new(true),
        );
        assert_ne!(out.exit, EXIT_OK, "not guessed: {out:?}");
    }

    #[test]
    fn a_confident_question_is_not_overridden_by_a_symbol_defined_elsewhere() {
        let rig = Rig::new();
        let cat = symbol_catalogue();
        // The words name lz4 outright; the symbol happens to live in zstd only.
        let backend = FakeBackend::new(true);
        let mut a = args("lz4 block decoder", None);
        a.mode = ModeRequest::Def("ZSTD_VERSION_RELEASE".to_string());
        let out = rig.run(&a, &cat, None, &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(backend.calls(), ["ready:lz4", "answer_local:lz4"]);
    }

    #[test]
    fn symbol_evidence_narrows_an_ambiguous_list() {
        let both = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            with_symbol_evidence(Routing::Ambiguous(both.clone()), &["b".to_string()]),
            Routing::Confident("b".to_string())
        );
        assert_eq!(
            with_symbol_evidence(
                Routing::Ambiguous(both.clone()),
                &["b".to_string(), "c".to_string()]
            ),
            Routing::Ambiguous(vec!["b".to_string(), "c".to_string()])
        );
        assert_eq!(
            with_symbol_evidence(Routing::Ambiguous(both.clone()), &["z".to_string()]),
            Routing::Ambiguous(both),
            "a symbol defined elsewhere changes nothing"
        );
        assert_eq!(
            with_symbol_evidence(Routing::NoCorpus, &[]),
            Routing::NoCorpus
        );
    }

    #[test]
    fn no_corpus_names_the_three_closest_so_the_asker_can_pick_one() {
        let rig = Rig::new();
        // Four corpora each hold one word of the question: nobody is credible
        // (each scores 0.2, below the 0.3 candidate bar), but there is a ranking.
        let cat = catalogue(vec![
            entry("c-one", 1, LicenceClass::AdaptWithAttribution, &["decoder"]),
            entry(
                "c-two",
                1,
                LicenceClass::AdaptWithAttribution,
                &["internals"],
            ),
            entry(
                "c-three",
                1,
                LicenceClass::AdaptWithAttribution,
                &["overview"],
            ),
            entry("c-four", 1, LicenceClass::AdaptWithAttribution, &["please"]),
            entry(
                "c-five",
                1,
                LicenceClass::AdaptWithAttribution,
                &["unrelated"],
            ),
        ]);
        let out = rig.run(
            &args("decoder internals overview please", None),
            &cat,
            None,
            &FakeBackend::new(true),
        );
        assert_eq!(out.exit, EXIT_NO_ANSWER, "{out:?}");
        assert_eq!(out.stderr[0], NO_CORPUS_MESSAGE);
        let closest = out.stderr.get(1).expect("a closest-corpora line");
        assert!(closest.starts_with("closest corpora: "), "{closest}");
        assert!(closest.contains("--corpus"), "{closest}");
        let named = ["c-one", "c-two", "c-three", "c-four", "c-five"]
            .iter()
            .filter(|s| closest.contains(*s))
            .count();
        assert_eq!(named, 3, "exactly the top three: {closest}");
        assert!(
            !closest.contains("c-five"),
            "the unrelated one is not close: {closest}"
        );
    }

    // ── a pull that will be refused does not start a node ───────────────────

    #[test]
    fn a_refused_pull_is_refused_before_a_node_is_started() {
        // `RealBackend::pull_now` used to start (and wait for) a node, and only
        // then ask the pull whether it would go ahead.
        let hub = tempfile::tempdir().unwrap();
        let forbidden = NodeLauncher {
            exe: PathBuf::from("/nonexistent/xerj-must-not-be-spawned"),
            pre_args: vec![],
        };
        let backend = RealBackend::new(
            hub.path().to_path_buf(),
            Some("http://127.0.0.1:1".to_string()),
            forbidden,
        );
        for (class, bytes, want) in [
            (LicenceClass::Excluded, 1024, "excluded licence class"),
            (
                LicenceClass::AdaptWithAttribution,
                80 * 1024 * 1024,
                "auto-pull cap",
            ),
        ] {
            let failure = backend
                .pull_now(&lz4(bytes, class))
                .expect_err("refused by the policy");
            assert_eq!(failure.code, 1);
            assert!(failure.message.contains(want), "{}", failure.message);
        }
        assert!(
            std::fs::read_dir(hub.path()).unwrap().next().is_none(),
            "no node dir, no log, no record: nothing was started or written"
        );
    }

    // ── the hosted switch ───────────────────────────────────────────────────

    #[test]
    fn hosted_off_never_sends_the_question_anywhere() {
        let rig = Rig::new();
        let cat = catalogue(with_fillers(vec![lz4(
            1024,
            LicenceClass::AdaptWithAttribution,
        )]));
        let backend = FakeBackend::new(false);
        let mut a = args("lz4 block decoder", None);
        a.hosted = HostedSwitch::Off;
        // NoNetwork would panic on the routing search or on an answer.
        let out = rig.run(&a, &cat, Some(&NoNetwork), &backend);
        assert_eq!(out.exit, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(
            backend.calls(),
            ["ready:lz4", "pull_now:lz4", "answer_local:lz4"]
        );
    }

    #[test]
    fn auto_is_off_inside_a_repo_with_a_one_time_notice_and_on_outside_one() {
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);

        // Inside a git work tree: off (NoNetwork would panic), notice once.
        let rig = Rig::new().in_git_repo();
        let first = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&NoNetwork),
            &FakeBackend::new(true),
        );
        assert_eq!(first.exit, EXIT_OK);
        assert_eq!(first.stderr, [HOSTED_OFF_NOTICE]);
        let second = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&NoNetwork),
            &FakeBackend::new(true),
        );
        assert!(second.stderr.is_empty(), "{:?}", second.stderr);

        // Outside one: on, no notice.
        let rig = Rig::new();
        let hosted = StubHosted::new();
        let out = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&hosted),
            &FakeBackend::new(false),
        );
        assert_eq!(hosted.answers.get(), 1);
        assert!(!out.stderr.iter().any(|l| l == HOSTED_OFF_NOTICE));
    }

    #[test]
    fn config_overrides_the_repo_rule_in_both_directions() {
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);

        // hosted = "on" inside a repo: used, and no notice.
        let rig = Rig::new().in_git_repo();
        set_config_hosted(rig.hub.path(), Some("on")).unwrap();
        let hosted = StubHosted::new();
        let out = rig.run(
            &args("q", Some("lz4")),
            &cat,
            Some(&hosted),
            &FakeBackend::new(false),
        );
        assert_eq!(hosted.answers.get(), 1);
        assert!(out.stderr.iter().all(|l| l != HOSTED_OFF_NOTICE));

        // hosted = "off" outside a repo: off.
        let rig = Rig::new();
        set_config_hosted(rig.hub.path(), Some("off")).unwrap();
        let backend = FakeBackend::new(false);
        let out = rig.run(&args("q", Some("lz4")), &cat, Some(&NoNetwork), &backend);
        assert_eq!(out.exit, EXIT_OK);
        assert!(backend.called("pull_now"));
    }

    #[test]
    fn an_explicit_hosted_on_without_a_client_says_so_and_still_answers() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let mut a = args("q", Some("lz4"));
        a.hosted = HostedSwitch::On;
        let out = rig.run(&a, &cat, None, &FakeBackend::new(true));
        assert_eq!(out.exit, EXIT_OK);
        assert!(
            out.stderr.iter().any(|l| l.contains("no hosted client")),
            "{:?}",
            out.stderr
        );
    }

    #[test]
    fn set_config_hosted_keeps_other_keys_and_auto_removes_the_key() {
        let hub = tempfile::tempdir().unwrap();
        fs::write(hub.path().join("config.toml"), "other = 3\n").unwrap();
        set_config_hosted(hub.path(), Some("on")).unwrap();
        assert_eq!(config_hosted(hub.path()).as_deref(), Some("on"));
        let text = fs::read_to_string(hub.path().join("config.toml")).unwrap();
        assert!(text.contains("other = 3"), "{text}");
        set_config_hosted(hub.path(), None).unwrap();
        assert_eq!(config_hosted(hub.path()), None);
        let text = fs::read_to_string(hub.path().join("config.toml")).unwrap();
        assert!(text.contains("other = 3"), "{text}");
    }

    // ── output ──────────────────────────────────────────────────────────────

    #[test]
    fn json_prints_the_answer_and_the_rendering_otherwise() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let mut a = args("q", Some("lz4"));
        a.json = true;
        let out = rig.run(&a, &cat, None, &FakeBackend::new(true));
        let parsed: Answer = serde_json::from_str(&out.stdout).expect("stdout is the Answer JSON");
        assert_eq!(parsed.corpus, "lz4");
        assert_eq!(parsed.answered_from, AnsweredFrom::LocalPack);

        let out = rig.run(&args("q", Some("lz4")), &cat, None, &FakeBackend::new(true));
        assert!(out
            .stdout
            .starts_with("lz4  pin 2026-10-01 (13 days old)  MIT  lz4/lib/lz4.h:100"));
    }

    #[test]
    fn an_answer_with_no_hits_exits_1_but_is_still_printed() {
        struct Empty;
        impl Backend for Empty {
            fn local_pack_ready(&self, _: &str) -> Result<bool, String> {
                Ok(true)
            }
            fn answer_local(&self, req: &Request<'_>) -> Result<Answer, String> {
                let mut a = canned(req.slug, AnsweredFrom::LocalPack, req.mode);
                a.hits.clear();
                Ok(a)
            }
            fn pull_now(&self, _: &CatalogueEntry) -> Result<(), PullFailure> {
                unreachable!()
            }
            fn pull_in_background(&self, _: &str) -> Result<BackgroundPull, String> {
                unreachable!()
            }
        }
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let out = rig.run(&args("q", Some("lz4")), &cat, None, &Empty);
        assert_eq!(out.exit, EXIT_NO_ANSWER);
        assert!(
            out.stdout.contains("no match in lz4 (pin 2026-10-01)"),
            "{}",
            out.stdout
        );
    }

    #[test]
    fn max_tokens_drops_trailing_hits_then_shortens_the_last() {
        let mut a = canned("lz4", AnsweredFrom::LocalPack, AnswerMode::How);
        let hit = a.hits[0].clone();
        a.hits = (0..4)
            .map(|i| Hit {
                line: 100 + i,
                text: "x".repeat(200),
                ..hit.clone()
            })
            .collect();

        // Generous budget: nothing changes.
        let mut big = a.clone();
        cap_tokens(&mut big, 10_000);
        assert_eq!(big.hits.len(), 4);
        assert!(!big.truncated);

        // Room for about two hits: the tail is dropped and the answer says so.
        let mut mid = a.clone();
        cap_tokens(&mut mid, 120);
        assert!(
            mid.hits.len() < 4 && !mid.hits.is_empty(),
            "{}",
            mid.hits.len()
        );
        assert!(mid.truncated);
        assert!(render(&mid).chars().count() <= 120 * 4);

        // A budget smaller than one hit: the first hit survives, shortened.
        let mut tiny = a.clone();
        cap_tokens(&mut tiny, 60);
        assert_eq!(tiny.hits.len(), 1);
        assert!(tiny.hits[0].text.ends_with('…'));
        assert!(tiny.hits[0].text.chars().count() < 200);
        assert!(tiny.truncated);
        assert!(render(&tiny).chars().count() <= 60 * 4);
    }

    // ── the disk budget ─────────────────────────────────────────────────────

    /// A fake extracted pack: `pack.json` plus a payload of `payload` bytes.
    fn fake_pack(hub: &Path, slug: &str, pin8: &str, payload: usize, used_secs_ago: u64) {
        let dir = hub.join("packs").join(slug).join(pin8);
        fs::create_dir_all(dir.join("snapshot")).unwrap();
        fs::write(dir.join("pack.json"), "{}").unwrap();
        fs::write(dir.join("snapshot").join("data"), vec![7u8; payload]).unwrap();
        let used = dir.join("last_used");
        fs::write(&used, "x").unwrap();
        let when = SystemTime::now() - Duration::from_secs(used_secs_ago);
        fs::File::options()
            .write(true)
            .open(&used)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn slugs_on_disk(hub: &Path) -> Vec<String> {
        pack_dirs(hub).into_iter().map(|p| p.slug).collect()
    }

    #[test]
    fn the_least_recently_used_pack_is_evicted_first() {
        let hub = tempfile::tempdir().unwrap();
        // Three packs of ~1000 bytes; "old" was used longest ago, "new" just now.
        fake_pack(hub.path(), "mid", "aaaa0001", 1000, 500);
        fake_pack(hub.path(), "old", "aaaa0002", 1000, 9000);
        fake_pack(hub.path(), "new", "aaaa0003", 1000, 5);

        // An incoming pack that needs one slot's worth of room.
        let total = dir_size(&hub.path().join("packs"));
        let budget = total; // already full: any incoming byte is over
        let evicted = evict_to_budget(hub.path(), "incoming", 900, budget);
        let names: Vec<_> = evicted.iter().map(|c| c.pack.slug.as_str()).collect();
        assert_eq!(names, ["old"], "only the oldest goes: that is enough");
        assert_eq!(slugs_on_disk(hub.path()), ["mid", "new"]);
        // The emptied slug directory went with it.
        assert!(!hub.path().join("packs").join("old").exists());

        // A second incoming pack the size of "mid": the next-oldest goes too, and
        // the newest stays.
        let total = dir_size(&hub.path().join("packs"));
        let mid_bytes = list_cached_packs(hub.path())
            .into_iter()
            .find(|c| c.pack.slug == "mid")
            .map(|c| c.bytes)
            .unwrap();
        let evicted = evict_to_budget(hub.path(), "incoming", mid_bytes, total);
        let names: Vec<_> = evicted.iter().map(|c| c.pack.slug.as_str()).collect();
        assert_eq!(names, ["mid"]);
        assert_eq!(slugs_on_disk(hub.path()), ["new"]);
    }

    #[test]
    fn eviction_never_removes_the_pack_being_pulled_and_stops_when_it_fits() {
        let hub = tempfile::tempdir().unwrap();
        // The slug being pulled is the OLDEST of all.
        fake_pack(hub.path(), "target", "bbbb0001", 1000, 99_999);
        fake_pack(hub.path(), "other", "bbbb0002", 1000, 100);
        let total = dir_size(&hub.path().join("packs"));

        // Over budget: "other" must go, "target" must stay although it is oldest.
        let evicted = evict_to_budget(hub.path(), "target", 5000, total);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].pack.slug, "other");
        assert_eq!(slugs_on_disk(hub.path()), ["target"]);

        // Even when nothing else is left and the budget is still exceeded, the
        // pull proceeds and the target survives.
        let evicted = evict_to_budget(hub.path(), "target", u64::MAX / 2, 10);
        assert!(evicted.is_empty());
        assert_eq!(slugs_on_disk(hub.path()), ["target"]);
    }

    #[test]
    fn under_budget_evicts_nothing() {
        let hub = tempfile::tempdir().unwrap();
        fake_pack(hub.path(), "a", "cccc0001", 1000, 1);
        fake_pack(hub.path(), "b", "cccc0002", 1000, 2);
        let evicted = evict_to_budget(hub.path(), "x", 100, 1_000_000);
        assert!(evicted.is_empty());
        assert_eq!(slugs_on_disk(hub.path()), ["a", "b"]);
    }

    #[test]
    fn the_real_budget_is_four_gib_and_a_pack_dir_needs_pack_json_to_count() {
        assert_eq!(xerj_common::hub::DISK_BUDGET_BYTES, 4 * 1024 * 1024 * 1024);
        let hub = tempfile::tempdir().unwrap();
        fake_pack(hub.path(), "real", "dddd0001", 10, 1);
        // A half-extracted dir, a temp dir and a lock dir are not packs.
        for junk in ["dddd0002", "dddd0003.tmp", "dddd0003.lock"] {
            fs::create_dir_all(hub.path().join("packs").join("real").join(junk)).unwrap();
        }
        assert_eq!(pack_dirs(hub.path()).len(), 1);
        assert_eq!(cached_slugs(hub.path()), ["real"]);
    }

    #[test]
    fn answering_touches_last_used_so_the_pack_becomes_the_newest() {
        let hub = tempfile::tempdir().unwrap();
        fake_pack(hub.path(), "a", "eeee0001", 10, 9000);
        fake_pack(hub.path(), "b", "eeee0002", 10, 500);
        let before = pack_dirs(hub.path());
        assert!(
            before[0].last_used < before[1].last_used,
            "a is older than b"
        );
        touch_last_used(hub.path(), "a");
        let after = pack_dirs(hub.path());
        assert!(
            after[0].last_used > after[1].last_used,
            "a is now newer than b"
        );
    }

    // ── converting a search response ────────────────────────────────────────

    fn meta() -> AnswerMeta {
        AnswerMeta {
            corpus: "lz4".to_string(),
            pin_sha: "0".repeat(40),
            pin_date: "2026-10-01".to_string(),
            age_days: 13,
            licence_spdx: "BSD-2-Clause".to_string(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: AnsweredFrom::LocalPack,
        }
    }

    fn search_response() -> Value {
        json!({"hits": {"hits": [
            {"_index": "xc-lz4-b1-docs", "_score": 7.5, "_source": {
                "ax_path": "lz4/lib/lz4.c",
                "body": "int other(void) {\n  return 0;\n}\nint LZ4_compress(const char* s) {\n  return 1;\n}\n",
                "symbols": [
                    {"name": "other", "kind": "function", "line": 1},
                    {"name": "LZ4_compress", "kind": "function", "line": 4}
                ]
            }},
            {"_index": "xc-lz4-b1-docs", "_score": 5.0, "_source": {
                "ax_path": "lz4/lib/lz4.c", "line": 40,
                "body": "second record of the same file about compress"
            }},
            {"_index": "xc-lz4-b1-docs", "_score": 3.0, "_source": {
                "ax_path": "gplrepo/src/x.c",
                "body": "static int compress_gpl(void) { return 2; }"
            }}
        ]}})
    }

    fn licences() -> HashMap<String, String> {
        HashMap::from([
            ("lz4".to_string(), "BSD-2-Clause".to_string()),
            ("gplrepo".to_string(), "GPL-3.0".to_string()),
        ])
    }

    #[test]
    fn a_how_answer_picks_the_matching_definition_with_its_real_line() {
        let a = answer_from_search(
            meta(),
            &search_response(),
            "how does LZ4_compress work",
            AnswerMode::How,
            &licences(),
        );
        assert_eq!(a.mode, AnswerMode::How);
        assert_eq!(a.hits.len(), 3);
        let h = &a.hits[0];
        assert_eq!(h.file, "lz4/lib/lz4.c");
        assert_eq!(h.kind, "function");
        assert_eq!(h.line, 4, "the definition's own start line");
        assert!(h.text.contains("LZ4_compress"), "{}", h.text);
        assert!((h.score - 7.5).abs() < 1e-6);
        assert!(h.licence_warning.is_none());
        // No symbols: a window, the record's own line number.
        assert_eq!(a.hits[1].kind, "passage");
        assert_eq!(a.hits[1].line, 40);
        // A record with no line at all reports 0 rather than inventing one.
        assert_eq!(a.hits[2].line, 0);
        // A restricted licence tags its hit.
        assert_eq!(
            a.hits[2].licence_warning.as_deref(),
            Some("approach-only licence (GPL-3.0): read, do not copy")
        );
    }

    #[test]
    fn a_files_answer_lists_each_file_once_with_its_licence() {
        let a = answer_from_search(
            meta(),
            &search_response(),
            "compress",
            AnswerMode::Files,
            &licences(),
        );
        assert_eq!(a.mode, AnswerMode::Files);
        let files: Vec<&str> = a.hits.iter().map(|h| h.file.as_str()).collect();
        assert_eq!(
            files,
            ["lz4/lib/lz4.c", "gplrepo/src/x.c"],
            "deduplicated, rank order"
        );
        assert_eq!(a.hits[0].kind, "file");
        assert_eq!(a.hits[0].text, "licence: BSD-2-Clause");
        assert_eq!(a.hits[1].text, "licence: GPL-3.0");
        assert!(a.hits[1].licence_warning.is_some());
    }

    #[test]
    fn locators_and_labels_parse() {
        assert_eq!(split_locator("a/b.c:12"), ("a/b.c".to_string(), 12));
        assert_eq!(split_locator("a/b.c"), ("a/b.c".to_string(), 0));
        assert_eq!(split_locator("a/b:c"), ("a/b:c".to_string(), 0));
        assert_eq!(
            parse_symbol_label("function foo @ line 12"),
            ("function".to_string(), Some(12))
        );
        assert_eq!(
            parse_symbol_label("struct bar"),
            ("struct".to_string(), None)
        );
    }

    #[test]
    fn a_raw_json_record_without_body_uses_its_longest_string_field() {
        let src = json!({"id": "x", "summary": "the longest field wins here", "n": 3});
        assert_eq!(longest_string_field(&src), "the longest field wins here");
        assert_eq!(longest_string_field(&json!({"n": 1})), "");
    }

    // ── policy helpers ──────────────────────────────────────────────────────

    #[test]
    fn pull_policy_orders_excluded_before_size() {
        let mib = 1024 * 1024;
        assert_eq!(
            pull_policy(&lz4(
                AUTO_PULL_MAX_BYTES,
                LicenceClass::AdaptWithAttribution
            )),
            PullPolicy::Allow,
            "exactly at the limit is allowed"
        );
        assert_eq!(
            pull_policy(&lz4(
                AUTO_PULL_MAX_BYTES + 1,
                LicenceClass::AdaptWithAttribution
            )),
            PullPolicy::TooLarge
        );
        assert_eq!(
            pull_policy(&lz4(1, LicenceClass::ApproachOnly)),
            PullPolicy::Allow,
            "approach-only is pulled (with a warning), not refused"
        );
        assert_eq!(
            pull_policy(&lz4(500 * mib, LicenceClass::Excluded)),
            PullPolicy::Excluded,
            "excluded wins over size"
        );
        assert_eq!(megabytes(52_428_800), "50.0");
        assert_eq!(megabytes(1_572_864), "1.5");
    }

    #[test]
    fn approach_only_warning_names_the_licence_or_says_unknown() {
        assert_eq!(
            approach_only_warning("AGPL-3.0"),
            "approach-only licence (AGPL-3.0): read, do not copy"
        );
        assert_eq!(
            approach_only_warning(""),
            "approach-only licence (unknown): read, do not copy"
        );
    }

    // ── an excluded corpus is never routed to or answered ───────────────────

    /// Review finding (rev7): `xerj_hub_find` hid an excluded corpus, but the ask
    /// flow behind `xerj_hub_ask` / `xerj hub ask` still routed to it, named it as
    /// a "closest corpus" to re-run with, and — given a pack on disk — answered
    /// from it at step 3 without any licence check. The gRPC `ask_answer` already
    /// routes over the non-excluded corpora only; the local flow now does the same.
    #[test]
    fn rev7_mcp_flow_routes_to_and_serves_an_excluded_corpus_find_hides() {
        let rig = Rig::new();
        let cat = catalogue(with_fillers(vec![
            lz4(1024, LicenceClass::Excluded),
            entry(
                "zstd",
                1024,
                LicenceClass::AdaptWithAttribution,
                &["zstd", "frame"],
            ),
        ]));
        // A pack of the excluded corpus is on disk (pulled before it was
        // reclassified, or copied in by hand).
        let pack = rig.hub.path().join("packs").join("lz4").join("00000000");
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join("pack.json"), "{}").unwrap();

        // find hides it ...
        let found = find("lz4 block decoder", 5, rig.cwd.path(), rig.hub.path(), &cat);
        assert!(found.corpora.iter().all(|c| c.slug != "lz4"), "{found:?}");

        // ... so ask, named or routed, must neither answer from it nor point at it.
        for (question, corpus) in [
            ("q", Some("lz4")),
            ("lz4 block decoder", None),
            ("how does the lz4 block decoder do compression", None),
            ("lz4", None),
        ] {
            for hosted in [None, Some(StubHosted::new())] {
                let backend = FakeBackend::new(true);
                let out = rig.run(
                    &args(question, corpus),
                    &cat,
                    hosted.as_ref().map(|h| h as &dyn HostedHub),
                    &backend,
                );
                let ctx = format!("{question:?} corpus={corpus:?} hosted={}", hosted.is_some());
                assert!(out.answer.is_none(), "{ctx}: answered {:?}", out.answer);
                assert_ne!(out.exit, EXIT_OK, "{ctx}");
                assert!(
                    !backend.called("answer_local") && !backend.called("ready:lz4"),
                    "{ctx}: {:?}",
                    backend.calls()
                );
                assert!(!backend.called("pull_"), "{ctx}: {:?}", backend.calls());
                if let Some(h) = &hosted {
                    assert_eq!(h.answers.get(), 0, "{ctx}: hosted asked about lz4");
                }
                let all = format!("{}\n{}", out.stdout, out.stderr.join("\n"));
                if corpus.is_some() {
                    // Named explicitly: a plain refusal, not "unknown corpus".
                    assert!(all.contains("excluded licence class"), "{ctx}: {all}");
                    assert!(!all.contains("xerj hub pull"), "{ctx}: {all}");
                } else {
                    assert!(!all.contains("lz4"), "{ctx}: points at it: {all}");
                }
            }
        }
    }

    #[test]
    fn an_unknown_named_corpus_is_echoed_back_cut_to_the_slug_cap() {
        let rig = Rig::new();
        let cat = catalogue(vec![lz4(1024, LicenceClass::AdaptWithAttribution)]);
        let backend = FakeBackend::new(false);
        let long = "z".repeat(200 * 1024);
        let out = rig.run(&args("q", Some(&long)), &cat, None, &backend);
        assert_eq!(out.exit, EXIT_USAGE);
        let msg = out.stderr.join("\n");
        assert!(msg.contains("unknown corpus"), "{msg}");
        assert!(msg.len() < 200, "{} bytes", msg.len());
        assert!(
            msg.contains(&format!("{}…", "z".repeat(SLUG_ECHO_MAX))),
            "{msg}"
        );
        // A short unknown slug is shown whole.
        let out = rig.run(&args("q", Some("nope")), &cat, None, &backend);
        assert!(out.stderr[0].contains("'nope'"), "{:?}", out.stderr);
    }

    // ── find (MCP `xerj_hub_find`) ──────────────────────────────────────────

    #[test]
    fn find_lists_what_ask_would_route_to_and_never_an_excluded_corpus() {
        let rig = Rig::new();
        let cat = catalogue(with_fillers(vec![
            lz4(1, LicenceClass::AdaptWithAttribution),
            entry(
                "lz4-closed",
                1,
                LicenceClass::Excluded,
                &["lz4", "block", "decoder", "compression"],
            ),
        ]));
        // A cached pack of lz4 (just a manifest: `pack_dirs` needs nothing else).
        let pack = rig.hub.path().join("packs").join("lz4").join("00000000");
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join("pack.json"), "{}").unwrap();

        let found = find(
            "how does the lz4 block decoder work",
            5,
            rig.cwd.path(),
            rig.hub.path(),
            &cat,
        );
        assert_eq!(found.corpora[0].slug, "lz4", "{found:?}");
        assert!(found.corpora[0].cached);
        assert!(found.corpora[0].score > 0.0);
        assert!(
            found.corpora.iter().all(|c| c.slug != "lz4-closed"),
            "an excluded corpus is never offered: {found:?}"
        );
        let text = render_find(&found);
        assert!(text.starts_with("lz4 — "), "{text}");
        assert!(text.contains("cached"), "{text}");

        // k is clamped to 1..=FIND_MAX_K.
        let one = find("lz4 block decoder", 0, rig.cwd.path(), rig.hub.path(), &cat);
        assert!(one.corpora.len() <= 1);

        // Nothing credible: an empty list and the llms.txt pointer.
        let none = find("zzqx wvut", 5, rig.cwd.path(), rig.hub.path(), &cat);
        assert_eq!(none.routing, "no_corpus");
        if none.corpora.is_empty() {
            assert_eq!(render_find(&none), NO_CORPUS_MESSAGE);
        }
    }
}
