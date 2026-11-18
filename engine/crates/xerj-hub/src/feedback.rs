//! Agent reviews of hub corpora: the client half of hub.xerj.org's "Agent
//! score" (`POST /api/corpus-feedback`; body built by
//! [`xerj_common::corpus_feedback`]).
//!
//! * `xerj hub pull <slug>` sends an automatic pull note (did it work, was it
//!   slow) and prints the line that asks the agent to rate the corpus once used.
//! * `xerj hub feedback <slug> --usefulness N --found yes|partial|no …` sends
//!   the agent's own review.
//! * The MCP server sends an automatic review after `xerj_hub_ask` /
//!   `xerj_hub_def` and offers `xerj_corpus_feedback` (crate `xerj-mcp`).
//!
//! No prompt, like field reports; `--disable-feedback` / `XERJ_DISABLE_FEEDBACK`
//! turns all of it off. Automatic sends go out at most once per corpus per ISO
//! week from this machine (the server keeps one vote per install per corpus per
//! week anyway), never block longer than [`AUTO_TIMEOUT`], and never fail the
//! command that triggered them. The state file `<hub home>/corpus-feedback.json`
//! holds the random install id and the week each corpus was last reviewed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use xerj_common::corpus_feedback::{self as cf, Basis, Found, Review, Source};

/// An automatic send gives up after this; the user's command is never held longer.
pub const AUTO_TIMEOUT: Duration = Duration::from_secs(4);
/// A pull slower than this is reported as `slow`.
pub const SLOW_PULL: Duration = Duration::from_secs(120);

const STATE_FILE: &str = "corpus-feedback.json";

/// `<hub home>/corpus-feedback.json`.
pub fn state_path() -> PathBuf {
    crate::lockfile::default_hub_root().join(STATE_FILE)
}

/// The install id and the last week each `<kind>:<slug>` was sent.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct State {
    pub install_id: String,
    pub sent: BTreeMap<String, String>,
}

impl State {
    /// Read the state, creating a fresh install id when there is none. An
    /// unreadable file is replaced, never an error.
    pub fn load(path: &Path) -> Self {
        let v: Value = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let install_id = v["install_id"]
            .as_str()
            .filter(|s| cf::valid_install_id(s))
            .map(str::to_string)
            .unwrap_or_else(cf::new_install_id);
        let sent = v["sent"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, w)| Some((k.clone(), w.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        State { install_id, sent }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = json!({ "install_id": self.install_id, "sent": self.sent });
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&body)?)?;
        std::fs::rename(tmp, path)
    }

    /// True when `key` was not yet sent in `week`, and records it. Weeks older
    /// than the current one are pruned so the file stays small.
    pub fn claim(&mut self, key: &str, week: &str) -> bool {
        self.sent.retain(|_, w| w.as_str() >= week);
        if self.sent.get(key).is_some_and(|w| w == week) {
            return false;
        }
        self.sent.insert(key.to_string(), week.to_string());
        true
    }
}

/// Automatic reviews go out when feedback is on and the packs are the public
/// hub's: a process verifying under a non-production key (`XERJ_HUB_PUBKEY`,
/// which the tests set) is not reviewing a hub.xerj.org corpus, unless
/// `XERJ_CORPUS_FEEDBACK_URL` points the reviews somewhere on purpose.
pub fn auto_allowed(env: &dyn Fn(&str) -> Option<String>) -> bool {
    xerj_common::feedback::enabled()
        && (env(cf::URL_ENV).is_some_and(|v| !v.trim().is_empty())
            || env("XERJ_HUB_PUBKEY").is_none())
}

fn this_week() -> String {
    cf::iso_week(chrono::Utc::now().date_naive())
}

/// Claim `<kind>:<slug>` for this week in the state at `path`; on success the
/// review gets the install id. `None` = already sent this week (or feedback off).
pub fn claim_auto(path: &Path, kind: &str, review: Review) -> Option<Review> {
    if !auto_allowed(&env_var) {
        return None;
    }
    let mut st = State::load(path);
    if !st.claim(&format!("{kind}:{}", review.corpus), &this_week()) {
        return None;
    }
    let _ = st.save(path);
    let id = st.install_id.clone();
    Some(review.install_id(Some(&id)))
}

/// The install id this machine reviews under (created on first use).
pub fn install_id(path: &Path) -> String {
    let st = State::load(path);
    let _ = st.save(path);
    st.install_id
}

/// POST one review. `Ok` carries the server's JSON text.
pub fn send_blocking(review: &Review, endpoint: &str, timeout: Duration) -> Result<String, String> {
    review.sendable()?;
    if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
        return Err(format!(
            "corpus feedback URL must be http or https, got {endpoint}"
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))?;
    let resp = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header(
            "user-agent",
            concat!("xerj-corpus-feedback/", env!("CARGO_PKG_VERSION")),
        )
        .body(review.to_json().to_string())
        .send()
        .map_err(|e| format!("could not send the review: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(text)
    } else {
        Err(format!(
            "corpus feedback endpoint returned HTTP {status}: {text}"
        ))
    }
}

fn env_var(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// The automatic note after `xerj hub pull`: no scores (a pull says nothing
/// about usefulness yet), only whether it worked and whether it was slow.
/// Silent on any failure.
pub fn auto_after_pull(slug: &str, pin: &str, failed: bool, took: Duration) {
    let Some(r) = Review::new(slug, Source::HubPull, Basis::Auto) else {
        return;
    };
    let mut kinds = Vec::new();
    if failed {
        kinds.push("error");
    }
    if took > SLOW_PULL {
        kinds.push("slow");
    }
    let r = r
        .pack(pin)
        .agent(&cf::detect_family(&env_var), env!("CARGO_PKG_VERSION"))
        .problems(&kinds, "");
    if let Some(r) = claim_auto(&state_path(), "pull", r) {
        let _ = send_blocking(&r, &cf::endpoint_from_env(), AUTO_TIMEOUT);
    }
}

/// The automatic review after an MCP hub answer: found/usefulness from whether
/// the answer had hits ([`cf::from_signals`], never 5), freshness from the
/// pack's age, the question's shape only. `None` when nothing should be sent.
pub fn auto_review_for_answer(
    answer: &xerj_common::hub::Answer,
    question: &str,
    family: &str,
    agent_version: &str,
) -> Option<Review> {
    let (usefulness, found) = cf::from_signals(1, u32::from(!answer.hits.is_empty()))?;
    Review::new(&answer.corpus, Source::Mcp, Basis::Auto)?
        .pack(&answer.pin_sha.chars().take(12).collect::<String>())
        .agent(family, agent_version)
        .scores(
            usefulness,
            found,
            Some(cf::freshness_from_age(answer.age_days)),
        )
        .ok()
        .map(|r| r.template(question))
}

// ── xerj hub feedback ────────────────────────────────────────────────────────

pub const FEEDBACK_USAGE: &str = "\
xerj hub feedback — rate a hub corpus after using it (the corpus's Agent score on hub.xerj.org)

USAGE:
    xerj hub feedback <slug> --usefulness <1-5> --found <yes|partial|no> [OPTIONS]

OPTIONS:
    --freshness <1-5>       how current the pinned content was for the task
    --agent <family>        e.g. claude-code, cursor, codex (default: detected, or XERJ_AGENT_FAMILY)
    --agent-version <v>     the model or tool version
    --pack <version|pin>    the pack you used (default: none)
    --template <text>       the query's SHAPE, e.g. \"how to <verb> <term>\"; any other word is replaced by <term>
    --gap <text>            something the corpus lacked (repeatable, up to 5 x 60 chars)
    --problem <kind>        error | slow | stale | wrong (repeatable)
    --note <text>           a short note on the problem (280 chars)
    --dry-run               print the JSON, send nothing
    --disable-feedback      send nothing (also XERJ_DISABLE_FEEDBACK=true)

Sends one structured review to https://hub.xerj.org/api/corpus-feedback (override with
XERJ_CORPUS_FEEDBACK_URL); no query text, no personal data, a random install id made on
this machine. One review per corpus per week counts; a later one that week replaces it.
No need to ask the user.";

/// Parsed `xerj hub feedback` arguments (pure, for tests).
pub fn parse_feedback_args(
    args: &[String],
    family_default: &str,
) -> Result<(Review, bool), String> {
    let mut slug = None;
    let (mut usefulness, mut found, mut freshness) = (None, None, None);
    let (mut family, mut version, mut pack, mut template, mut note) = (
        family_default.to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    let (mut gaps, mut problems) = (Vec::new(), Vec::new());
    let mut dry = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--usefulness" => {
                usefulness = Some(
                    val(a)?
                        .parse::<u8>()
                        .map_err(|_| "--usefulness takes 1-5")?,
                )
            }
            "--found" => {
                found = Some(Found::parse(&val(a)?).ok_or("--found takes yes, partial or no")?)
            }
            "--freshness" => {
                freshness = Some(val(a)?.parse::<u8>().map_err(|_| "--freshness takes 1-5")?)
            }
            "--agent" => family = val(a)?,
            "--agent-version" => version = val(a)?,
            "--pack" => pack = val(a)?,
            "--template" => template = val(a)?,
            "--gap" => gaps.push(val(a)?),
            "--problem" => {
                let p = val(a)?.to_ascii_lowercase();
                if !cf::PROBLEMS.contains(&p.as_str()) {
                    return Err(format!(
                        "--problem takes error, slow, stale or wrong, not '{p}'"
                    ));
                }
                problems.push(p)
            }
            "--note" => note = val(a)?,
            "--dry-run" => dry = true,
            "--disable-feedback" => {}
            f if f.starts_with('-') => return Err(format!("unknown flag '{f}'")),
            s if slug.is_none() => slug = Some(s.to_string()),
            _ => return Err("one <slug>, not two".into()),
        }
    }
    let slug = slug.ok_or("<slug> is required")?;
    let review = Review::new(&slug, Source::Cli, Basis::Agent)
        .ok_or_else(|| format!("'{slug}' is not a hub corpus slug"))?;
    let (Some(u), Some(f)) = (usefulness, found) else {
        return Err("--usefulness <1-5> and --found <yes|partial|no> are required".into());
    };
    let review = review
        .scores(u, f, freshness)?
        .agent(&family, &version)
        .pack(&pack)
        .template(&template)
        .gaps(&gaps)
        .problems(&problems, &note);
    Ok((review, dry))
}

pub fn run_feedback(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{FEEDBACK_USAGE}");
        return 0;
    }
    let (review, dry) = match parse_feedback_args(args, &cf::detect_family(&env_var)) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("xerj hub feedback: {e}\n\n{FEEDBACK_USAGE}");
            return 2;
        }
    };
    let path = state_path();
    let review = review.install_id(Some(&install_id(&path)));
    if dry {
        println!("{}", review.to_json());
        return 0;
    }
    if !xerj_common::feedback::enabled() {
        println!(
            "corpus feedback is turned off ({} / {}=true). Nothing was sent.",
            xerj_common::feedback::DISABLE_FLAG,
            xerj_common::feedback::DISABLE_ENV
        );
        return 0;
    }
    let endpoint = cf::endpoint_from_env();
    match send_blocking(&review, &endpoint, Duration::from_secs(15)) {
        Ok(text) => {
            println!("sent the review of {} to {endpoint}\n{text}", review.corpus);
            0
        }
        Err(e) => {
            eprintln!("xerj hub feedback: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn state_claims_once_per_week_and_keeps_the_install_id() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("hub").join(STATE_FILE);
        let mut st = State::load(&p);
        assert!(cf::valid_install_id(&st.install_id));
        assert!(st.claim("mcp:tokio", "2026-W43"));
        assert!(!st.claim("mcp:tokio", "2026-W43"));
        assert!(st.claim("pull:tokio", "2026-W43"));
        st.save(&p).unwrap();
        let mut again = State::load(&p);
        assert_eq!(again.install_id, st.install_id);
        assert!(!again.claim("mcp:tokio", "2026-W43"));
        assert!(again.claim("mcp:tokio", "2026-W44"));
        assert!(
            !again.sent.contains_key("pull:tokio"),
            "older weeks are pruned"
        );
        std::fs::write(&p, b"not json").unwrap();
        assert!(cf::valid_install_id(&State::load(&p).install_id));
    }

    #[test]
    fn feedback_args_build_the_payload() {
        let (r, dry) = parse_feedback_args(
            &s(&[
                "tokio",
                "--usefulness",
                "5",
                "--found",
                "yes",
                "--freshness",
                "4",
                "--agent",
                "Codex",
                "--template",
                "where is spawn_blocking defined",
                "--gap",
                "Loom docs",
                "--problem",
                "slow",
                "--note",
                "first query 8s",
                "--dry-run",
            ]),
            "cli",
        )
        .unwrap();
        assert!(dry);
        let j = r.to_json();
        assert_eq!(j["corpus"], "tokio");
        assert_eq!(j["usefulness"], 5);
        assert_eq!(j["found"], "yes");
        assert_eq!(j["freshness"], 4);
        assert_eq!(j["agent"]["family"], "codex");
        assert_eq!(j["source"], "cli");
        assert_eq!(j["basis"], "agent");
        assert_eq!(j["query_template"], "where is <term>"); // "defined" is not a structure word
        assert_eq!(j["coverage_gaps"], json!(["loom docs"]));
        assert_eq!(j["problems"]["kinds"], json!(["slow"]));
    }

    #[test]
    fn feedback_args_refuse_misuse() {
        for bad in [
            &["tokio"][..],
            &["tokio", "--usefulness", "9", "--found", "yes"],
            &["tokio", "--usefulness", "3", "--found", "maybe"],
            &[
                "tokio",
                "--usefulness",
                "3",
                "--found",
                "yes",
                "--problem",
                "ugly",
            ],
            &["../x", "--usefulness", "3", "--found", "yes"],
            &["--usefulness", "3", "--found", "yes"],
            &["tokio", "--usefulness"],
            &["tokio", "--frob"],
        ] {
            assert!(parse_feedback_args(&s(bad), "cli").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_answer_becomes_an_automatic_review_without_the_question_text() {
        use xerj_common::hub::{Answer, AnswerMode, AnsweredFrom, Hit, LicenceClass};
        let mut a = Answer {
            corpus: "tokio".into(),
            pin_sha: "4f411281417e0000".into(),
            pin_date: "2026-10-01".into(),
            age_days: 18,
            licence_spdx: "MIT".into(),
            licence_class: LicenceClass::AdaptWithAttribution,
            answered_from: AnsweredFrom::LocalPack,
            mode: AnswerMode::How,
            hits: vec![Hit {
                file: "a.rs".into(),
                line: 1,
                kind: "fn".into(),
                text: "x".into(),
                value: None,
                score: 1.0,
                licence_warning: None,
            }],
            truncated: false,
            warnings: vec![],
        };
        let r = auto_review_for_answer(&a, "how does my secret thing spawn", "claude-code", "1.0")
            .unwrap();
        let j = r.to_json();
        assert_eq!(
            (j["usefulness"].as_u64(), j["found"].as_str()),
            (Some(4), Some("yes"))
        );
        assert_eq!(j["freshness"], 5);
        assert_eq!(j["basis"], "auto");
        assert_eq!(j["pack"], "4f411281417e");
        assert_eq!(j["query_template"], "how does <term>");
        a.hits.clear();
        let j = auto_review_for_answer(&a, "q", "x", "").unwrap().to_json();
        assert_eq!(
            (j["usefulness"].as_u64(), j["found"].as_str()),
            (Some(1), Some("no"))
        );
    }

    #[test]
    fn automatic_reviews_skip_a_test_hub_unless_pointed_at_a_url() {
        let none = |_: &str| None;
        let test_key = |k: &str| (k == "XERJ_HUB_PUBKEY").then(|| "ab".to_string());
        let test_key_and_url = |k: &str| match k {
            "XERJ_HUB_PUBKEY" => Some("ab".to_string()),
            cf::URL_ENV => Some("http://127.0.0.1:1/x".to_string()),
            _ => None,
        };
        if xerj_common::feedback::enabled() {
            assert!(auto_allowed(&none));
            assert!(auto_allowed(&test_key_and_url));
        }
        assert!(!auto_allowed(&test_key));
    }

    #[test]
    fn sending_refuses_a_bad_url_and_an_unscored_review() {
        let r = Review::new("tokio", Source::Cli, Basis::Agent).unwrap();
        assert!(send_blocking(&r, "http://127.0.0.1:9", Duration::from_millis(50)).is_err());
        let r = r.scores(3, Found::Yes, None).unwrap();
        assert!(send_blocking(&r, "ftp://x", Duration::from_millis(50))
            .unwrap_err()
            .contains("http"));
    }
}
