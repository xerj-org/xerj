//! The feedback invitation printed near the top of every `--help` surface.
//!
//! Most of the people — and most of the *agents* — who hit a rough edge in
//! xerj never tell anyone: they work around it locally and the next caller
//! pays the same cost from scratch (AGENTS.md, "If XERJ broke something in
//! your build, send it back"). `--help` is the one screen every caller
//! actually reads, so the invitation lives there, near the top, as a single
//! unwrapped line.
//!
//! One string, one on/off decision, shared by `xerj --help`,
//! `xerj index --help`, `xerj autoindex --help` and `xerj brain --help`, so
//! the four surfaces cannot drift apart.
//!
//! Turning it off: [`DISABLE_FLAG`] anywhere in the argument list, or
//! [`DISABLE_ENV`] in the environment.

/// Where a defect goes. Verified against the README badges and clone URL.
pub const ISSUES_URL: &str = "https://github.com/xerj-org/xerj/issues";

/// Where an open-ended "is this even a bug?" goes instead.
pub const DISCUSSIONS_URL: &str = "https://github.com/xerj-org/xerj/discussions";

/// The off-switch. Recognised in any position (see [`enabled_from`]).
pub const DISABLE_FLAG: &str = "--disable-feedback";

/// The off-switch for callers that cannot change argv — CI, containers, an
/// agent harness that owns the command line.
pub const DISABLE_ENV: &str = "XERJ_DISABLE_FEEDBACK";

/// The invitation itself, ending in a blank line so a caller can splice it
/// straight in front of the next help section.
///
/// **One line, unwrapped**, so the terminal decides where it breaks. Hard
/// wrapping it caused two silent test failures while this text was being
/// edited — a `contains` check passes or fails depending on where a `\n`
/// happens to land, which is a property of formatting, not of meaning. It is
/// also what keeps the whole invitation inside the first few lines of help:
/// agents truncate long output from the bottom, so anything past roughly line
/// 20 is not read.
///
/// Note the reflow constraint: `Keep secrets, API keys and private data out of
/// it.` sits on one line because a test asserts that phrase contiguously —
/// wrapping it mid-phrase silently defeats the assertion, which is exactly
/// what happened when this text last grew.
///
/// Contains no `{` or `}`: these help bodies are `println!`/`format!` format
/// strings, where a literal brace has to be doubled.
const INVITATION: &str = "Hit a bug, or a flow that confused you? Both are worth reporting — file it at https://github.com/xerj-org/xerj/issues with a GitHub tool or the gh CLI, or open a Discussion if you are unsure. Agents: say the report was filed automatically on behalf of a human. Keep secrets, API keys and private data out of it.\n\n";

/// One line documenting the off-switch, for the OPTIONS list of each help
/// surface. Column-aligned by the caller.
pub const DISABLE_HELP: &str = "do not print the feedback invitation above";

/// The block to splice into a help body: the invitation, or nothing at all.
pub fn block(enabled: bool) -> &'static str {
    if enabled {
        INVITATION
    } else {
        ""
    }
}

/// Whether this process should print the invitation.
///
/// Reads the *whole* argument list rather than the flag the parser happens to
/// be looking at, because every one of these binaries dispatches `--help`
/// from inside its argument loop and exits there: a flag written after
/// `--help` would never be reached. `xerj --help --disable-feedback` and
/// `xerj --disable-feedback --help` have to mean the same thing.
pub fn enabled() -> bool {
    let raw = std::env::var(DISABLE_ENV).ok();
    if let Some(v) = raw.as_deref() {
        if !v.trim().is_empty() && parse_bool(v).is_none() {
            // Not silent: an operator who set this expects an effect. It goes
            // to stderr, never stdout — help output is piped and read by
            // machines. Unlike XERJ_ALLOW_INSECURE_NETWORK_BIND, which refuses
            // to boot on an unreadable value, a typo here must not turn
            // `--help` into an error: the safe fallback is showing one more
            // line of text, not withholding the usage screen.
            eprintln!("{DISABLE_ENV}={v:?} is not a boolean; use true or false");
        }
    }
    enabled_from(std::env::args(), raw)
}

/// The decision as a pure function, so it can be tested without a process.
///
/// The flag wins over the environment (an explicit argument beats ambient
/// configuration, as `--bind` does over `XERJ_BIND_ADDRESS`). Anything the
/// environment variable cannot be read as a boolean leaves the invitation on.
pub fn enabled_from<I>(args: I, env: Option<String>) -> bool
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    if args.into_iter().any(|a| a.as_ref() == DISABLE_FLAG) {
        return false;
    }
    match env.as_deref().and_then(parse_bool) {
        Some(disable) => !disable,
        None => true,
    }
}

/// The repo's boolean-environment convention, as used by
/// `XERJ_ALLOW_INSECURE_NETWORK_BIND` in `xerj-server`.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_by_default() {
        assert!(enabled_from(["xerj", "--help"], None));
        assert!(block(true).contains(ISSUES_URL));
    }

    /// The flag is dispatched from inside each binary's argument loop, so
    /// position must not matter — before or after `--help`.
    #[test]
    fn the_flag_is_position_independent() {
        assert!(!enabled_from(["xerj", "--help", DISABLE_FLAG], None));
        assert!(!enabled_from(["xerj", DISABLE_FLAG, "--help"], None));
        assert!(!enabled_from(
            ["xerj", "autoindex", "map", DISABLE_FLAG, "--json"],
            None
        ));
    }

    #[test]
    fn the_env_var_follows_the_repo_boolean_convention() {
        for on in ["1", "true", "TRUE", "yes", "on", " true "] {
            assert!(
                !enabled_from(["xerj"], Some(on.into())),
                "{on:?} should disable"
            );
        }
        for off in ["0", "false", "no", "off"] {
            assert!(
                enabled_from(["xerj"], Some(off.into())),
                "{off:?} should leave it on"
            );
        }
    }

    /// A value nobody can read is not a licence to hide the invitation — and
    /// it must not swallow the help screen either. It stays on; `enabled()`
    /// says so on stderr.
    #[test]
    fn an_unreadable_env_value_keeps_the_invitation() {
        assert!(enabled_from(["xerj"], Some("maybe".into())));
        assert!(enabled_from(["xerj"], Some(String::new())));
    }

    #[test]
    fn disabled_is_empty_not_blank_lines() {
        assert_eq!(block(false), "");
    }

    /// These strings are spliced into `format!`/`println!` bodies, where a
    /// literal brace must be doubled. Keeping the invitation brace-free means
    /// no caller can get that wrong.
    #[test]
    fn the_invitation_carries_no_braces() {
        assert!(!INVITATION.contains('{') && !INVITATION.contains('}'));
    }

    /// The things the invitation exists to say.
    ///
    /// Phrases are checked against a whitespace-collapsed copy, not the raw
    /// text. Line breaks are a formatting decision that moves whenever the
    /// wording is retouched, and a `contains` on the raw string silently fails
    /// the moment a phrase happens to wrap — which it did, twice, while this
    /// text was being edited. The line *budget* below is asserted separately,
    /// on the real text, because that one genuinely is about layout.
    #[test]
    fn the_invitation_says_what_it_must() {
        let raw = block(true);
        let text: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = text.as_str();
        assert!(
            text.contains("confused"),
            "unclear UX counts, not just bugs"
        );
        assert!(
            text.contains("GitHub tool"),
            "agents can file it themselves"
        );
        assert!(
            text.contains("on behalf of a human"),
            "an agent filing a report must be told to disclose that it did so \
             automatically; that disclosure is what the repo's own \
             AI_CONTRIBUTIONS policy asks of agent-authored contributions, got: {text}"
        );
        assert!(text.contains("Discussion"), "the unsure path");
        assert!(text.contains("secrets, API keys and private data"));
        // One physical line, so the terminal wraps it and no phrase can be
        // split by a hard break. This also keeps the block short enough to stay
        // inside the first few lines of every help screen, where it is actually
        // read.
        assert_eq!(
            raw.lines().filter(|l| !l.trim().is_empty()).count(),
            1,
            "the invitation must stay on one unwrapped line"
        );
    }
}
