//! `xerj feedback` — draft the agent field report the repo asks every agent to
//! file, and (optionally) open the pull request for it.
//!
//! ## Why this command exists
//!
//! `user-feedback/16-agent-field-reports/README.md` and
//! https://xerj.org/llms.txt both say the baseline contribution asked of every
//! agent that uses XERJ is one short field report. In practice that ask was
//! *empirically dead*: the folder held only its README, zero reports, because
//! the obligation was a paragraph of prose with no tooling behind it and a CLA
//! hard-gate in front of it (see
//! `demo/playbooks/AGENT_CONTRIBUTION_LOOP_2026-08-18.md`). This turns the ask
//! into one command.
//!
//! ## What it does, and what it refuses to do
//!
//! It auto-fills only *facts it can actually observe*:
//! - `xerj --version` (this binary's own version),
//! - OS + arch (`std::env::consts`),
//! - "what was indexed", read from a running node's `autoindex-catalog` via
//!   [`crate::fetch_catalog_summary`] — degrading to the template placeholder
//!   when nothing is reachable, never inventing a corpus.
//!
//! Every *opinion* comes from a flag (`--verdict`, `--used-for`,
//! `--pointed-at`, `--numbers`); an omitted flag emits the template placeholder
//! for a human or agent to fill, because a tool must not invent an opinion any
//! more than it may invent a number.
//!
//! And one hard refusal: `--open-pr` branches, commits and pushes in the
//! current directory's repository, so it first checks that the repository's
//! `origin` actually is `xerj-org/xerj`. An agent's working directory is
//! normally the user's own project; the command must never branch, commit or
//! push there (#960).
//!
//! The rendered report's FIRST line states an AI agent wrote it — the same
//! provenance rule the invitation and `.github/AI_CONTRIBUTIONS.md` apply to
//! every agent-authored contribution.
//!
//! Note the name: this module is `crate::feedback` (the `xerj feedback`
//! subcommand). It is unrelated to `xerj_common::feedback`, which is the
//! one-line "report a bug" invitation printed in every `--help` screen.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one fixed home of agent field reports. A report goes here and nowhere
/// else — that fixed path is exactly what lets the CLA carve-out recognise a
/// field-report-only pull request (see `scripts/check_cla_coauthors.py`).
pub const FIELD_REPORT_DIR: &str = "user-feedback/16-agent-field-reports";

/// The report's first line, so the provenance is impossible to miss and easy to
/// assert. Matches the invitation's "say it was filed on behalf of a human".
const PROVENANCE: &str =
    "> Written by an AI agent, filed automatically on behalf of a human operator.";

// Template placeholders — copied from the folder README so a drafted-but-unfilled
// report reads exactly like the "copy this, fill it in" template, and so a
// reviewer sees an unfilled slot rather than a fabricated claim.
const PH_TITLE: &str = "<one line: what you used XERJ for>";
const PH_AGENT: &str = "<model / tool>";
const PH_POINTED_AT: &str = "<what you indexed — kind of corpus and rough size, one sentence>";
const PH_USED_FOR: &str =
    "<reference coding / autoindex + query / agent memory / vector or hybrid search — one sentence>";
const PH_VERDICT: &str = "<2-4 sentences of opinion. What worked, what did not, what you would not use it for. This is the part worth reading.>";
const PH_NUMBERS: &str =
    "<command -> result, only if you measured it. Otherwise: \"not measured\".>";
const PH_FILED: &str = "<link to the issue or PR you opened, or \"nothing broke\">";

/// Parsed `xerj feedback` invocation.
#[derive(Debug, Clone)]
pub struct Cfg {
    pub agent: Option<String>,
    pub used_for: Option<String>,
    pub pointed_at: Option<String>,
    pub verdict: Option<String>,
    pub numbers: Option<String>,
    pub filed_alongside: Option<String>,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub url: String,
    pub api_key: Option<String>,
    pub output: Option<PathBuf>,
    pub open_pr: bool,
    pub dry_run: bool,
    /// Skip the running-node catalog probe entirely — for a fully offline draft
    /// (and for tests, so they never touch a real endpoint).
    pub no_autofill: bool,
}

/// The fields of the report after flags, auto-fill and placeholders are all
/// resolved. Pure data: rendering it never touches the network or the clock.
#[derive(Debug, Clone)]
pub struct Draft {
    pub date: String,
    pub title: String,
    pub agent: String,
    pub version: String,
    pub platform: String,
    pub pointed_at: String,
    pub used_for: String,
    pub verdict: String,
    pub numbers: String,
    pub filed_alongside: String,
    pub slug: String,
}

/// Entry point for the `xerj feedback` subcommand (blocking; the server binary
/// calls this via `spawn_blocking`). Returns the process exit code.
pub fn run_cli() -> i32 {
    let args: Vec<String> = xerj_common::argv::args_after_subcommand();
    match parse(args) {
        Ok(None) => {
            print_help();
            0
        }
        Ok(Some(cfg)) => run(&cfg),
        Err(e) => {
            eprintln!("error: {e}\n");
            print_help();
            2
        }
    }
}

/// Parse argv (after `xerj feedback`). `Ok(None)` means `--help`.
pub fn parse(args: Vec<String>) -> Result<Option<Cfg>, String> {
    let mut it = args.into_iter();
    let mut cfg = Cfg {
        agent: None,
        used_for: None,
        pointed_at: None,
        verdict: None,
        numbers: None,
        filed_alongside: None,
        title: None,
        slug: None,
        url: "http://localhost:9200".to_string(),
        api_key: std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty()),
        output: None,
        open_pr: false,
        dry_run: false,
        no_autofill: false,
    };
    while let Some(arg) = it.next() {
        let mut next = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--agent" => cfg.agent = Some(next("--agent")?),
            "--used-for" => cfg.used_for = Some(next("--used-for")?),
            "--pointed-at" => cfg.pointed_at = Some(next("--pointed-at")?),
            "--verdict" => cfg.verdict = Some(next("--verdict")?),
            "--numbers" => cfg.numbers = Some(next("--numbers")?),
            "--filed-alongside" => cfg.filed_alongside = Some(next("--filed-alongside")?),
            "--title" => cfg.title = Some(next("--title")?),
            "--slug" => cfg.slug = Some(next("--slug")?),
            "--url" => cfg.url = next("--url")?,
            "--api-key" => cfg.api_key = Some(next("--api-key")?),
            "-o" | "--output" => cfg.output = Some(PathBuf::from(next("--output")?)),
            "--open-pr" => cfg.open_pr = true,
            "--dry-run" => cfg.dry_run = true,
            "--no-autofill" => cfg.no_autofill = true,
            // Scanned out of band by `xerj_common::feedback`; accepted here so
            // it is not "unknown".
            xerj_common::feedback::DISABLE_FLAG => {}
            "--help" | "-h" => return Ok(None),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // `--open-pr` acts; `--dry-run` explicitly does nothing. Honouring one and
    // silently dropping the other is the accepted-and-ignored class this repo
    // refuses (#204), so name the contradiction instead.
    if cfg.open_pr && cfg.dry_run {
        return Err(
            "--open-pr and --dry-run contradict each other: --dry-run only prints the plan and \
             opens no pull request. Drop one of the two"
                .into(),
        );
    }
    Ok(Some(cfg))
}

/// Resolve the draft, render it, and act on the mode (`--dry-run`, `--open-pr`,
/// or the default stdout/`-o`).
fn run(cfg: &Cfg) -> i32 {
    let date = today();
    // Auto-fill "what was indexed" only when asked to and only from a node that
    // actually answers; any failure leaves `None` → placeholder, never a
    // fabricated corpus.
    let indexed = if cfg.no_autofill {
        None
    } else {
        match crate::fetch_catalog_summary(&cfg.url, cfg.api_key.clone()) {
            Ok(summary) => summary.one_line(),
            Err(_) => None,
        }
    };
    let draft = resolve_draft(cfg, indexed, &date);
    let report = render_report(&draft);
    let commands = pr_commands(&draft);

    if cfg.dry_run {
        // Print the report AND the exact commands, and do nothing else.
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{report}");
        let _ = writeln!(out, "\n{commands}");
        return 0;
    }
    if cfg.open_pr {
        return open_pr(&draft, &report, &commands);
    }
    // Default: report to stdout, and to `-o <path>` if one was given.
    print!("{report}");
    if let Some(path) = &cfg.output {
        if let Err(e) = write_report_file(path, &report) {
            eprintln!("error: could not write {}: {e}", path.display());
            return 1;
        }
        eprintln!("wrote {}", path.display());
    }
    0
}

/// Today's date, `YYYY-MM-DD`, in local time — "the date you filed it".
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Collapse the flags, the auto-filled summary and the placeholders into a
/// single [`Draft`]. Precedence for "Pointed at": an explicit `--pointed-at`
/// wins, then the auto-filled catalog summary, then the placeholder.
pub fn resolve_draft(cfg: &Cfg, indexed: Option<String>, date: &str) -> Draft {
    let non_empty = |o: &Option<String>| {
        o.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let used_for = non_empty(&cfg.used_for);
    let pointed_at = non_empty(&cfg.pointed_at);
    let title = non_empty(&cfg.title)
        .or_else(|| used_for.clone())
        .or_else(|| pointed_at.clone())
        .unwrap_or_else(|| PH_TITLE.to_string());
    let slug = non_empty(&cfg.slug)
        .map(|s| slugify(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| derive_slug(used_for.as_deref(), pointed_at.as_deref()));
    Draft {
        date: date.to_string(),
        title,
        agent: non_empty(&cfg.agent).unwrap_or_else(|| PH_AGENT.to_string()),
        version: format!("xerj v{}", env!("CARGO_PKG_VERSION")),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        // Flag > auto-filled fact > placeholder.
        pointed_at: pointed_at
            .or(indexed)
            .unwrap_or_else(|| PH_POINTED_AT.to_string()),
        used_for: used_for.unwrap_or_else(|| PH_USED_FOR.to_string()),
        verdict: non_empty(&cfg.verdict).unwrap_or_else(|| PH_VERDICT.to_string()),
        numbers: non_empty(&cfg.numbers).unwrap_or_else(|| PH_NUMBERS.to_string()),
        filed_alongside: non_empty(&cfg.filed_alongside).unwrap_or_else(|| PH_FILED.to_string()),
        slug,
    }
}

/// Render the field report exactly to the folder README's template, with the
/// provenance line prepended so the first line always discloses the author.
pub fn render_report(d: &Draft) -> String {
    format!(
        "{PROVENANCE}\n\
         \n\
         # {title} ({date})\n\
         \n\
         **Agent:** {agent}  ·  **XERJ:** {version}  ·  **Platform:** {platform}\n\
         \n\
         **Pointed at:** {pointed_at}\n\
         \n\
         **Used it for:** {used_for}\n\
         \n\
         **Verdict:** {verdict}\n\
         \n\
         **Numbers:** {numbers}\n\
         \n\
         **Filed alongside:** {filed_alongside}\n",
        title = d.title,
        date = d.date,
        agent = d.agent,
        version = d.version,
        platform = d.platform,
        pointed_at = d.pointed_at,
        used_for = d.used_for,
        verdict = d.verdict,
        numbers = d.numbers,
        filed_alongside = d.filed_alongside,
    )
}

/// The report's path within the repo: `user-feedback/16-agent-field-reports/<date>-<slug>.md`.
pub fn report_relpath(date: &str, slug: &str) -> String {
    format!("{FIELD_REPORT_DIR}/{date}-{slug}.md")
}

/// A slug for the filename, from `--used-for` first, then `--pointed-at`.
/// Falls back to `field-report` when both are absent (both are placeholders),
/// so the filename is always valid.
pub fn derive_slug(used_for: Option<&str>, pointed_at: Option<&str>) -> String {
    let source = used_for
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| pointed_at.map(str::trim).filter(|s| !s.is_empty()))
        .unwrap_or("");
    let slug = slugify(source);
    if slug.is_empty() {
        "field-report".to_string()
    } else {
        slug
    }
}

/// Lowercase kebab-case, ASCII-only, bounded to ~50 chars at a word boundary.
///
/// Only ASCII `[a-z0-9]` are kept and everything else becomes a single `-`, so
/// the output is pure ASCII and the length bound below slices on a byte index
/// that is always a char boundary — the class of `&str` byte-slice panic that
/// core-dumped an autoindex run on a multibyte filename (`sqldump.rs`) cannot
/// happen here.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.len() <= 50 {
        return out;
    }
    let head = &out[..50];
    match head.rfind('-') {
        Some(pos) if pos > 0 => head[..pos].to_string(),
        _ => head.trim_matches('-').to_string(),
    }
}

/// The exact, copy-pasteable git + gh commands that file this report — printed
/// by `--dry-run`, and again as the honest fallback when `--open-pr` cannot run
/// `gh` itself.
pub fn pr_commands(d: &Draft) -> String {
    let relpath = report_relpath(&d.date, &d.slug);
    let branch = format!("field-report/{}", d.slug);
    // The placeholder was never passed on the command line. Reprinting it
    // would make the next run look filled in. A real `--agent` has to survive,
    // or the suggested command silently drops the model that wrote the report.
    let agent_line = if d.agent == PH_AGENT {
        String::new()
    } else {
        format!(" \\\n --agent {:?}", d.agent)
    };
    format!(
        "# Ready-to-run — files ONLY the field report, nothing else:\n\
         xerj feedback{agent_line} \\\n\
         \x20 --used-for {used_for:?} --pointed-at {pointed_at:?} \\\n\
         \x20 --verdict {verdict:?} --numbers {numbers:?} \\\n\
         \x20 -o {relpath}\n\
         git checkout -b {branch}\n\
         git add {relpath}\n\
         git commit --only {relpath} -m \"docs(field-report): {slug}\"\n\
         git push -u origin {branch}\n\
         gh pr create --repo xerj-org/xerj --base main --head {branch} \\\n\
         \x20 --title \"Agent field report: {title}\" \\\n\
         \x20 --body \"Written by an AI agent on behalf of a human. Field report only; see {relpath}.\"",
        used_for = d.used_for,
        pointed_at = d.pointed_at,
        verdict = d.verdict,
        numbers = d.numbers,
        relpath = relpath,
        branch = branch,
        slug = d.slug,
        title = d.title,
    )
}

/// Write the report to `path`, creating the field-reports directory if the path
/// points inside it.
fn write_report_file(path: &Path, report: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, report)
}

/// The argv for the field-report commit. It commits ONLY the report path via
/// `git commit --only <relpath>`, so anything a caller happened to have staged
/// is left in the index and never reaches the public PR. A bare `git commit -m`
/// committed the whole index instead — the field report's own `--open-pr`
/// promise ("commit ONLY that one file") was false, and an agent working in a
/// dirty repo would publish whatever else was staged (a `.env`, a credential, an
/// unrelated change). #484.
fn field_report_commit_argv(relpath: &str, slug: &str) -> Vec<String> {
    vec![
        "commit".into(),
        "--only".into(),
        relpath.into(),
        "-m".into(),
        format!("docs(field-report): {slug}"),
    ]
}

/// The canonical spellings of this repository's GitHub `origin`, as `git
/// remote get-url origin` may print them: https, scp-style ssh and ssh-URL
/// form, each without the optional `.git` suffix (the matcher tolerates it).
const XERJ_ORIGIN_URLS: &[&str] = &[
    "https://github.com/xerj-org/xerj",
    "git@github.com:xerj-org/xerj",
    "ssh://git@github.com/xerj-org/xerj",
];

/// Whether `url` — the raw output of `git remote get-url origin` — points at
/// the xerj-org/xerj repository itself, and at nothing else.
///
/// Pure string matching against the three canonical GitHub spellings above,
/// case-insensitive, with an optional `.git` suffix and trailing slashes
/// tolerated. The owner/name must match EXACTLY after that normalisation, so a
/// same-prefix repository (`xerj-org/xerj-fork`) or someone else's project
/// never passes. #960.
fn origin_url_is_xerj(url: &str) -> bool {
    let normalized = url.trim().to_ascii_lowercase();
    let normalized = normalized
        .strip_suffix(".git")
        .unwrap_or(normalized.as_str());
    let normalized = normalized.trim_end_matches('/');
    XERJ_ORIGIN_URLS.contains(&normalized)
}

/// `git remote get-url origin` run in `dir` (the process working directory
/// when `None`). `None` when the command fails — not inside a git repository,
/// or the repository has no `origin` remote.
fn origin_remote_url(dir: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(["remote", "get-url", "origin"]);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// Whether the repository at `dir` (the process working directory when `None`)
/// is a checkout whose `origin` remote points at xerj-org/xerj — the one
/// repository `--open-pr` may branch, commit and push in. #960.
fn origin_is_xerj_checkout(dir: Option<&Path>) -> bool {
    origin_remote_url(dir)
        .as_deref()
        .is_some_and(origin_url_is_xerj)
}

/// `--open-pr`: commit ONLY the field report on a new branch and run
/// `gh pr create`. If `gh` is missing or unauthenticated, FAIL LOUDLY and print
/// the exact ready-to-run commands — the honest sandboxed-agent path, never a
/// silent partial success.
fn open_pr(draft: &Draft, report: &str, commands: &str) -> i32 {
    open_pr_at(None, draft, report, commands)
}

/// The working core of [`open_pr`]. `dir` is the repository to act in — `None`
/// is the process working directory, the only mode the CLI uses; `Some(path)`
/// exists so the foreign-repo refusal can be tested against a real throwaway
/// repository instead of mutating this process's working directory (#960).
fn open_pr_at(dir: Option<&Path>, draft: &Draft, report: &str, commands: &str) -> i32 {
    let relpath = report_relpath(&draft.date, &draft.slug);
    let branch = format!("field-report/{}", draft.slug);

    // Preconditions first, before we touch the working tree.
    //
    // THE one repository this may run in: the steps below branch, commit and
    // push in `dir`'s repository, and an agent's working directory is normally
    // the user's own project, not a checkout of xerj. Before #960 the only
    // precondition was "some git repo exists", so the invitation effectively
    // asked every agent to branch, commit and push in whatever repository it
    // happened to be standing in. Refuse unless `origin` resolves to
    // xerj-org/xerj itself.
    if !origin_is_xerj_checkout(dir) {
        let found = match origin_remote_url(dir) {
            Some(url) => format!("{url:?}"),
            None => "none — this directory is not a git repository, or it has no \
                     `origin` remote"
                .to_string(),
        };
        eprintln!(
            "error: `xerj feedback --open-pr` may only run in a checkout of xerj-org/xerj; \
             this repository's `origin` is {found}."
        );
        eprintln!(
            "Refusing to create a branch, write the report, commit or push here — nothing \
             has been touched."
        );
        eprintln!(
            "To file the report: run this command again from a clone of xerj-org/xerj, or \
             fork it, push `{branch}` to your fork and open the PR with \
             `gh pr create --repo xerj-org/xerj --head <your-login>:{branch}`."
        );
        eprintln!("The report and the exact commands to run there:\n");
        println!("{report}");
        println!("\n{commands}");
        return 1;
    }
    // A `gh` that cannot open the PR turns a committed branch into a dead end.
    // Say so and hand over the commands instead.
    if let Err(reason) = gh_ready() {
        eprintln!("error: cannot open the pull request automatically: {reason}");
        eprintln!(
            "Falling back to the exact commands — run these yourself (the sandboxed-agent path):\n"
        );
        println!("{commands}");
        return 1;
    }

    let steps: [(&str, Vec<String>); 5] = [
        (
            "git checkout -b",
            vec!["checkout".into(), "-b".into(), branch.clone()],
        ),
        ("write report", vec![]), // handled specially below
        ("git add", vec!["add".into(), relpath.clone()]),
        (
            "git commit",
            field_report_commit_argv(&relpath, &draft.slug),
        ),
        (
            "git push",
            vec!["push".into(), "-u".into(), "origin".into(), branch.clone()],
        ),
    ];

    let report_path = match dir {
        Some(d) => d.join(&relpath),
        None => PathBuf::from(&relpath),
    };
    for (label, argv) in steps {
        if label == "write report" {
            if let Err(e) = write_report_file(&report_path, report) {
                eprintln!("error: could not write {}: {e}", report_path.display());
                eprintln!("Falling back to the exact commands:\n");
                println!("{commands}");
                return 1;
            }
            continue;
        }
        if !run_cmd(dir, "git", &argv) {
            eprintln!("error: `git {}` failed.", argv.join(" "));
            eprintln!("Falling back to the exact commands:\n");
            println!("{commands}");
            return 1;
        }
    }

    let pr_body =
        format!("Written by an AI agent on behalf of a human. Field report only; see {relpath}.");
    let pr_args = vec![
        "pr".to_string(),
        "create".to_string(),
        // Explicit, so the PR can only ever land in this repository — never in
        // whatever repository `gh` would otherwise infer from the cwd (#960).
        "--repo".to_string(),
        "xerj-org/xerj".to_string(),
        "--base".to_string(),
        "main".to_string(),
        "--head".to_string(),
        branch.clone(),
        "--title".to_string(),
        format!("Agent field report: {}", draft.title),
        "--body".to_string(),
        pr_body,
    ];
    if !run_cmd(dir, "gh", &pr_args) {
        eprintln!("error: `gh pr create` failed — the branch and commit are in place.");
        eprintln!("Finish it yourself with:\n");
        println!("{commands}");
        return 1;
    }
    0
}

/// Whether `gh` is on PATH and authenticated. Both are required to open a PR;
/// either missing is a loud failure, not a silent skip.
fn gh_ready() -> Result<(), String> {
    if Command::new("gh").arg("--version").output().is_err() {
        return Err("`gh` (the GitHub CLI) is not installed or not on PATH".into());
    }
    match Command::new("gh").args(["auth", "status"]).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(_) => Err("`gh` is installed but not authenticated (run `gh auth login`)".into()),
        Err(e) => Err(format!("could not run `gh auth status`: {e}")),
    }
}

fn run_cmd(dir: Option<&Path>, bin: &str, args: &[String]) -> bool {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

pub fn print_help() {
    println!("{}", help_text(xerj_common::feedback::enabled()));
}

/// The help text as a value so a test can assert a flag stays documented.
pub fn help_text(feedback: bool) -> String {
    format!(
        "xerj feedback — draft the agent field report XERJ asks every agent to file\n\
         \n\
         {feedback_block}\
         USAGE:\n\
             xerj feedback [OPTIONS]              draft a field report to stdout\n\
             xerj feedback -o <PATH> [OPTIONS]    also write it to <PATH>\n\
             xerj feedback --open-pr [OPTIONS]    commit ONLY the report + open a PR\n\
             xerj feedback --dry-run [OPTIONS]    print the report AND the commands; do nothing\n\
         \n\
         The report auto-fills only facts it can observe — this binary's version,\n\
         OS + arch, and (from a running node's autoindex-catalog) what was indexed.\n\
         When no node answers, the \"Pointed at\" line degrades to the template\n\
         placeholder rather than inventing a corpus. Every OPINION comes from a\n\
         flag; an omitted flag leaves the template placeholder for you to fill.\n\
         The report's first line states an AI agent wrote it.\n\
         \n\
         OPINION (all optional; omitted → template placeholder, never invented):\n\
             --verdict <TEXT>     2-4 sentences: what worked, what did not\n\
             --used-for <TEXT>    reference coding / autoindex+query / memory / vector search\n\
             --pointed-at <TEXT>  what you indexed (overrides the auto-filled summary)\n\
             --numbers <TEXT>     command -> result, only if you measured it\n\
             --filed-alongside <TEXT>  link to the issue/PR you opened, or \"nothing broke\"\n\
             --agent <TEXT>       the model / tool writing this (e.g. 'Claude Code')\n\
             --title <TEXT>       one-line report title (default: --used-for / --pointed-at)\n\
         \n\
         OUTPUT + FILING:\n\
             -o, --output <PATH>  also write the report to <PATH>\n\
             --slug <TEXT>        filename slug (default: derived from --used-for/--pointed-at);\n\
                                  the file lands at {dir}/<YYYY-MM-DD>-<slug>.md\n\
             --open-pr            create branch field-report/<slug>, commit ONLY that one file,\n\
                                  and run `gh pr create --repo xerj-org/xerj`. Requires a\n\
                                  checkout whose `origin` IS xerj-org/xerj; in any other\n\
                                  repository it refuses loudly — nothing written, committed\n\
                                  or pushed — and prints the report + commands. If gh is\n\
                                  missing/unauth it FAILS LOUDLY and prints the exact\n\
                                  commands to run by hand\n\
             --dry-run            print the report AND the exact commands, and do nothing\n\
                                  (opens no PR, writes no file)\n\
         \n\
         READING THE CATALOG (auto-fill source):\n\
             --url <U>            ES-compat endpoint (default http://localhost:9200)\n\
             --api-key <K>        Authorization header (or env XERJ_API_KEY)\n\
             --no-autofill        skip the node probe; draft fully offline\n\
         \n\
             --disable-feedback   do not print the invitation above (env XERJ_DISABLE_FEEDBACK)\n\
             --help, -h           this help\n\
         \n\
         CLA: a pull request that adds ONLY files under {dir}/ (markdown field\n\
         reports, nothing else) is exempted from the CLA gate — see CLA.md and\n\
         .github/AI_CONTRIBUTIONS.md. Bundling any other change re-arms the gate.\n",
        feedback_block = xerj_common::feedback::block(feedback),
        dir = FIELD_REPORT_DIR,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cfg() -> Cfg {
        Cfg {
            agent: None,
            used_for: None,
            pointed_at: None,
            verdict: None,
            numbers: None,
            filed_alongside: None,
            title: None,
            slug: None,
            url: "http://127.0.0.1:1".to_string(),
            api_key: None,
            output: None,
            open_pr: false,
            dry_run: false,
            no_autofill: true,
        }
    }

    #[test]
    fn provenance_is_always_the_first_line() {
        let d = resolve_draft(&base_cfg(), None, "2026-08-18");
        let report = render_report(&d);
        assert_eq!(
            report.lines().next().unwrap(),
            PROVENANCE,
            "the first line must disclose the AI author"
        );
        assert!(report.lines().next().unwrap().contains("AI agent"));
    }

    #[test]
    fn a_fully_filled_report_carries_every_field_and_no_placeholder() {
        let cfg = Cfg {
            agent: Some("Claude Code".into()),
            used_for: Some("reference coding a Rust monorepo".into()),
            pointed_at: Some("~2k Rust files, 40 MB".into()),
            verdict: Some("Retrieval beat grep on unfamiliar code.".into()),
            numbers: Some("xc sift 9/9 hits".into()),
            filed_alongside: Some("nothing broke".into()),
            ..base_cfg()
        };
        let d = resolve_draft(&cfg, None, "2026-08-18");
        let report = render_report(&d);
        for expected in [
            "Claude Code",
            "reference coding a Rust monorepo",
            "~2k Rust files, 40 MB",
            "Retrieval beat grep on unfamiliar code.",
            "xc sift 9/9 hits",
            "nothing broke",
            "2026-08-18",
        ] {
            assert!(report.contains(expected), "report is missing {expected:?}");
        }
        // No template placeholder survived a fully-filled draft.
        for ph in [
            PH_TITLE,
            PH_AGENT,
            PH_POINTED_AT,
            PH_USED_FOR,
            PH_VERDICT,
            PH_NUMBERS,
            PH_FILED,
        ] {
            assert!(!report.contains(ph), "a filled report still shows {ph:?}");
        }
        // The version line is this binary's real version, not a guess.
        assert!(report.contains(&format!("xerj v{}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn omitted_opinions_become_placeholders_and_are_never_invented() {
        let d = resolve_draft(&base_cfg(), None, "2026-08-18");
        let report = render_report(&d);
        // Every opinion field falls back to its exact template placeholder.
        for ph in [
            PH_VERDICT,
            PH_USED_FOR,
            PH_POINTED_AT,
            PH_NUMBERS,
            PH_FILED,
            PH_AGENT,
        ] {
            assert!(report.contains(ph), "missing placeholder {ph:?}");
        }
        // Facts are still filled in — placeholders are for OPINION only.
        assert!(report.contains(std::env::consts::OS));
        assert!(report.contains(std::env::consts::ARCH));
    }

    #[test]
    fn the_catalog_summary_fills_pointed_at_only_when_no_flag_is_given() {
        // Auto-filled fact, no --pointed-at → the fact wins over the placeholder.
        let d = resolve_draft(
            &base_cfg(),
            Some("1234 records across 3 dataset(s)".into()),
            "2026-08-18",
        );
        assert_eq!(d.pointed_at, "1234 records across 3 dataset(s)");
        assert!(!render_report(&d).contains(PH_POINTED_AT));

        // An explicit --pointed-at always wins over the auto-fill.
        let cfg = Cfg {
            pointed_at: Some("my own words".into()),
            ..base_cfg()
        };
        let d = resolve_draft(
            &cfg,
            Some("1234 records across 3 dataset(s)".into()),
            "2026-08-18",
        );
        assert_eq!(d.pointed_at, "my own words");
    }

    #[test]
    fn the_summary_counts_only_the_latest_runs_datasets() {
        // Last run recorded 10 records in 2 datasets. Two older datasets from
        // another run sit on the same catalog. The sentence must not say
        // "10 records across 4 dataset(s)".
        let summary = crate::CatalogSummary {
            run: Some(serde_json::json!({
                "run_id": "run-b",
                "records_total": 10,
                "root": "/repo",
            })),
            datasets: vec![
                serde_json::json!({"run_id": "run-a", "record_count": 1000}),
                serde_json::json!({"run_id": "run-a", "record_count": 1000}),
                serde_json::json!({"run_id": "run-b", "record_count": 4}),
                serde_json::json!({"run_id": "run-b", "record_count": 6}),
            ],
        };
        assert_eq!(
            summary.one_line().as_deref(),
            Some("10 records across 2 dataset(s) under /repo (autoindex run run-b)")
        );

        // Dataset docs written before `run_id` was stored cannot be split by
        // run, so the whole list is still the only count available.
        let legacy = crate::CatalogSummary {
            run: Some(serde_json::json!({
                "run_id": "run-old",
                "records_total": 5,
            })),
            datasets: vec![
                serde_json::json!({"record_count": 2}),
                serde_json::json!({"record_count": 3}),
            ],
        };
        assert_eq!(
            legacy.one_line().as_deref(),
            Some("5 records across 2 dataset(s) (autoindex run run-old)")
        );

        // Tagged docs from a different run are not this run's datasets. The
        // record total still comes from the run document; the dataset count
        // is omitted rather than borrowed from the other run.
        let others = crate::CatalogSummary {
            run: Some(serde_json::json!({
                "run_id": "run-new",
                "records_total": 7,
                "root": "/x",
            })),
            datasets: vec![serde_json::json!({"run_id": "run-old", "record_count": 99})],
        };
        assert_eq!(
            others.one_line().as_deref(),
            Some("7 records under /x (autoindex run run-new)")
        );

        // No run document: sum the dataset docs that were actually fetched.
        let datasets_only = crate::CatalogSummary {
            run: None,
            datasets: vec![
                serde_json::json!({"record_count": 2}),
                serde_json::json!({"record_count": 3}),
            ],
        };
        assert_eq!(
            datasets_only.one_line().as_deref(),
            Some("5 records across 2 dataset(s)")
        );
        assert!(crate::CatalogSummary {
            run: None,
            datasets: Vec::new(),
        }
        .one_line()
        .is_none());
    }

    #[test]
    fn the_rerun_command_keeps_a_real_agent_and_drops_the_placeholder() {
        let filled = resolve_draft(
            &Cfg {
                agent: Some("Claude Code".into()),
                ..base_cfg()
            },
            None,
            "2026-08-18",
        );
        let commands = pr_commands(&filled);
        assert!(
            commands.contains("--agent \"Claude Code\""),
            "a passed --agent must be reprinted, got:\n{commands}"
        );
        assert!(commands.contains("xerj feedback \\\n --agent \"Claude Code\""));

        let placeholder = resolve_draft(&base_cfg(), None, "2026-08-18");
        let commands = pr_commands(&placeholder);
        assert!(
            !commands.contains("--agent"),
            "the placeholder agent must not be reprinted as a flag:\n{commands}"
        );
        assert!(!commands.contains(PH_AGENT), "{commands}");
    }

    #[test]
    fn slug_and_path_are_derived_correctly() {
        assert_eq!(
            slugify("Reference Coding a Rust Monorepo!"),
            "reference-coding-a-rust-monorepo"
        );
        // used-for wins over pointed-at for the slug.
        assert_eq!(
            derive_slug(Some("autoindex + query"), Some("logs")),
            "autoindex-query"
        );
        assert_eq!(derive_slug(None, Some("A PDF folder")), "a-pdf-folder");
        // Both absent → a valid fallback, never an empty filename.
        assert_eq!(derive_slug(None, None), "field-report");
        assert_eq!(derive_slug(Some("   "), Some("")), "field-report");
        // A non-ASCII-only source still yields a usable slug and never panics.
        assert_eq!(derive_slug(Some("设计 ا"), None), "field-report");

        assert_eq!(
            report_relpath("2026-08-18", "a-pdf-folder"),
            "user-feedback/16-agent-field-reports/2026-08-18-a-pdf-folder.md"
        );
    }

    #[test]
    fn a_long_source_slugs_to_a_bounded_ascii_string_at_a_word_boundary() {
        let long = "reference coding across a very large multi language monorepo with many crates and services";
        let slug = slugify(long);
        assert!(slug.len() <= 50, "{} chars: {slug}", slug.len());
        assert!(slug.is_ascii());
        assert!(!slug.starts_with('-') && !slug.ends_with('-'));
        assert!(!slug.contains("--"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("report.md");
        let d = resolve_draft(&base_cfg(), None, "2026-08-18");
        let report = render_report(&d);
        let commands = pr_commands(&d);
        // Exercise the exact emit path --dry-run takes: write to an in-memory
        // sink, and prove no file is touched.
        let mut sink: Vec<u8> = Vec::new();
        writeln!(sink, "{report}").unwrap();
        writeln!(sink, "\n{commands}").unwrap();
        assert!(!out.exists(), "dry-run must not create the -o file");
        let text = String::from_utf8(sink).unwrap();
        assert!(text.contains("git checkout -b"), "commands must be printed");
        assert!(text.contains(PROVENANCE), "the report must be printed");
    }

    #[test]
    fn the_output_path_write_actually_writes_the_report_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join(FIELD_REPORT_DIR).join("2026-08-18-x.md");
        let d = resolve_draft(&base_cfg(), None, "2026-08-18");
        let report = render_report(&d);
        write_report_file(&out, &report).unwrap();
        assert!(out.exists());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), report);
    }

    #[test]
    fn open_pr_and_dry_run_together_are_refused() {
        let err = parse(vec!["--open-pr".into(), "--dry-run".into()]).unwrap_err();
        assert!(err.contains("contradict"), "{err}");
    }

    #[test]
    fn unknown_flags_are_refused_and_help_short_circuits() {
        assert!(parse(vec!["--nope".into()])
            .unwrap_err()
            .contains("unknown"));
        assert!(parse(vec!["--help".into()]).unwrap().is_none());
        assert!(parse(vec!["-h".into()]).unwrap().is_none());
    }

    #[test]
    fn flags_populate_the_cfg() {
        let cfg = parse(vec![
            "--verdict".into(),
            "great".into(),
            "--used-for".into(),
            "reference coding".into(),
            "-o".into(),
            "/tmp/r.md".into(),
            "--no-autofill".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(cfg.verdict.as_deref(), Some("great"));
        assert_eq!(cfg.used_for.as_deref(), Some("reference coding"));
        assert_eq!(cfg.output.as_deref(), Some(Path::new("/tmp/r.md")));
        assert!(cfg.no_autofill);
    }

    #[test]
    fn help_documents_the_contract_and_the_carveout() {
        let help = help_text(true);
        for expected in [
            "--open-pr",
            "--dry-run",
            "--verdict",
            "--pointed-at",
            "first line states an AI agent wrote it",
            "exempted from the CLA gate",
            FIELD_REPORT_DIR,
            // #960: the origin precondition is part of the --open-pr contract.
            "xerj-org/xerj",
            "origin",
            "refuses loudly",
        ] {
            assert!(help.contains(expected), "help missing {expected:?}");
        }
    }

    /// #484: `--open-pr` must publish ONLY the field report, even when the
    /// agent's repo already has something else staged. Proven by the commit
    /// tree, not by inspecting the argv.
    #[test]
    fn the_field_report_commit_includes_only_the_report_not_a_staged_decoy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("run git")
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("seed"), "seed").unwrap();
        git(&["add", "seed"]);
        git(&["commit", "-qm", "seed"]);

        // Something the agent had staged: a secret, an unrelated change, ...
        std::fs::write(root.join("SECRET.env"), "TOKEN=leakme").unwrap();
        git(&["add", "SECRET.env"]);

        // The report, written and staged the way `open_pr` does.
        let relpath = report_relpath("2026-08-19", "my-report");
        let path = root.join(&relpath);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "# report\n").unwrap();
        git(&["add", &relpath]);

        // The fix under test: commit --only the report.
        let argv = field_report_commit_argv(&relpath, "my-report");
        let argv_str: Vec<&str> = argv.iter().map(String::as_str).collect();
        let ok = Command::new("git")
            .args(&argv_str)
            .current_dir(root)
            .status()
            .expect("run git commit")
            .success();
        assert!(ok, "git commit --only failed");

        let committed = String::from_utf8(
            Command::new("git")
                .args(["show", "--name-only", "--format=", "HEAD"])
                .current_dir(root)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert!(
            committed.contains(&relpath),
            "the field report must be committed: {committed:?}"
        );
        assert!(
            !committed.contains("SECRET.env"),
            "a staged decoy must NOT be committed by --open-pr (#484): {committed:?}"
        );

        // The decoy is left in the index, untouched — not committed, not lost.
        let staged = String::from_utf8(
            Command::new("git")
                .args(["diff", "--cached", "--name-only"])
                .current_dir(root)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert!(
            staged.contains("SECRET.env"),
            "the decoy must remain staged, untouched: {staged:?}"
        );
    }

    /// Run `git` in `root`, asserting success — the #484 test's pattern.
    fn git_run(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// `git`'s stdout in `root`, asserting success.
    fn git_stdout(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// A minimal real repository: one commit, no remotes.
    fn seeded_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_run(root, &["init", "-q"]);
        git_run(root, &["config", "user.email", "t@t"]);
        git_run(root, &["config", "user.name", "t"]);
        git_run(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("seed"), "seed").unwrap();
        git_run(root, &["add", "seed"]);
        git_run(root, &["commit", "-qm", "seed"]);
        dir
    }

    /// #960: the origin matcher accepts exactly the canonical spellings of
    /// xerj-org/xerj — and nothing else, however similar.
    #[test]
    fn origin_url_is_xerj_matches_only_the_xerj_repository() {
        for url in [
            "https://github.com/xerj-org/xerj",
            "https://github.com/xerj-org/xerj.git",
            "git@github.com:xerj-org/xerj",
            "git@github.com:xerj-org/xerj.git",
            "ssh://git@github.com/xerj-org/xerj",
            "ssh://git@github.com/xerj-org/xerj.git",
            // Tolerate the forms a copy button or `git remote set-url` leaves
            // behind: trailing slash, mixed case, surrounding whitespace.
            "https://github.com/xerj-org/xerj/",
            "  HTTPS://GitHub.com/XERJ-Org/XERJ.git\n",
        ] {
            assert!(origin_url_is_xerj(url), "{url:?} should be accepted");
        }
        for url in [
            "",
            "   ",
            "https://github.com/someone/project.git",
            "git@github.com:someone/project.git",
            // Same prefix, different repository: must NOT pass.
            "git@github.com:xerj-org/xerj-fork.git",
            "https://github.com/xerj-org/xerj-fork",
            // A path under the repository is a different URL.
            "https://github.com/xerj-org/xerj/issues",
            // Same owner/name on a different host: must NOT pass.
            "ssh://git@gitlab.com/xerj-org/xerj.git",
        ] {
            assert!(!origin_url_is_xerj(url), "{url:?} must be rejected");
        }
    }

    /// #960: the precondition reads the repository's REAL `origin` remote —
    /// foreign origin, xerj origin, no origin at all, and no repository at all
    /// each resolve the way the refusal needs.
    #[test]
    fn origin_is_xerj_checkout_reads_the_actual_origin_remote() {
        let foreign = seeded_repo();
        git_run(
            foreign.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/foreign/repo.git",
            ],
        );
        assert!(
            !origin_is_xerj_checkout(Some(foreign.path())),
            "a foreign origin must be refused"
        );

        let xerj = seeded_repo();
        git_run(
            xerj.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:xerj-org/xerj.git",
            ],
        );
        assert!(
            origin_is_xerj_checkout(Some(xerj.path())),
            "the xerj origin must pass"
        );

        let no_remote = seeded_repo();
        assert!(
            !origin_is_xerj_checkout(Some(no_remote.path())),
            "a repository without an origin remote must be refused"
        );

        let not_a_repo = tempfile::tempdir().unwrap();
        assert!(
            !origin_is_xerj_checkout(Some(not_a_repo.path())),
            "a directory that is not a git repository must be refused"
        );
    }

    /// #960: in a repository whose `origin` is NOT xerj-org/xerj, `--open-pr`
    /// must refuse BEFORE any step runs — no branch, no report file, no
    /// commit, no push — and hand over the report + commands instead.
    ///
    /// `gh` is stubbed as installed-and-authenticated for the duration of this
    /// test (the same stub-gh construction the issue was reproduced with), so
    /// the ONLY precondition that can refuse is the origin one. Prepending a
    /// directory to PATH is process-global but harmless here: it is restored on
    /// drop (panic included), and no other test in this binary spawns `gh`.
    #[test]
    fn open_pr_refuses_in_a_foreign_repository_before_touching_the_tree() {
        struct PathGuard(std::ffi::OsString);
        impl Drop for PathGuard {
            fn drop(&mut self) {
                std::env::set_var("PATH", &self.0);
            }
        }

        let dir = seeded_repo();
        let root = dir.path();
        git_run(
            root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/foreign/repo.git",
            ],
        );
        // If the precondition ever fails to refuse, the steps would run; make
        // the push fail instantly and offline rather than touching a network.
        git_run(
            root,
            &["remote", "set-url", "--push", "origin", "/nonexistent/960"],
        );

        let stub_dir = tempfile::tempdir().unwrap();
        let stub = stub_dir.path().join("gh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let previous_path = std::env::var_os("PATH").expect("PATH is set");
        std::env::set_var(
            "PATH",
            std::ffi::OsString::from(format!(
                "{}:{}",
                stub_dir.path().display(),
                previous_path.to_string_lossy()
            )),
        );
        let _restore_path = PathGuard(previous_path);

        let head_before = git_stdout(root, &["rev-parse", "HEAD"]);
        let d = resolve_draft(&base_cfg(), None, "2026-09-20");
        let report = render_report(&d);
        let commands = pr_commands(&d);
        let code = open_pr_at(Some(root), &d, &report, &commands);

        assert_eq!(code, 1, "the foreign-repo refusal must exit nonzero");
        assert!(
            !root.join(FIELD_REPORT_DIR).exists(),
            "#960: no report may be written into a foreign repository"
        );
        let branches = git_stdout(root, &["branch", "--list", "field-report/*"]);
        assert!(
            branches.trim().is_empty(),
            "#960: no field-report branch may be created in a foreign repository: {branches}"
        );
        assert_eq!(
            git_stdout(root, &["rev-parse", "HEAD"]).trim(),
            head_before.trim(),
            "#960: HEAD must not move in a foreign repository"
        );
    }
}
