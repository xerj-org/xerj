//! The feedback invitation, checked on the real binary.
//!
//! Every help surface a caller actually meets — `xerj --help`,
//! `xerj index --help`, `xerj autoindex --help`, `xerj brain --help` — must
//! carry the bug/confusing-UX invitation by default, near the top, and must
//! drop it when `--disable-feedback` is given in *either* position or when
//! `XERJ_DISABLE_FEEDBACK` is set.
//!
//! These drive the built binary rather than the library functions on purpose:
//! `--help` is dispatched from inside each argument loop and exits there, so
//! only a real process proves that a flag written *after* `--help` still
//! counts.

use std::process::Command;

/// The first words of the invitation. One needle, so a reworded second or
/// third line does not fail the position check.
const NEEDLE: &str = "Hit a bug, or a flow that confused you?";

/// The issues URL is the load-bearing part: an invitation without a
/// destination is decoration.
const ISSUES_URL: &str = "https://github.com/xerj-org/xerj/issues";

/// "Near the top" is the requirement, not "somewhere in the file" — at the
/// bottom of a 200-line help body nothing would ever read it.
const MAX_LINE: usize = 10;

/// Every surface, as the argv a caller would type.
const SURFACES: [&[&str]; 4] = [
    &["--help"],
    &["index", "--help"],
    &["autoindex", "--help"],
    &["brain", "--help"],
];

fn help(args: &[&str], env: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xerj"));
    cmd.args(args);
    match env {
        Some(v) => cmd.env("XERJ_DISABLE_FEEDBACK", v),
        // The developer's own shell must not decide the default case.
        None => cmd.env_remove("XERJ_DISABLE_FEEDBACK"),
    };
    let out = cmd.output().expect("run xerj");
    assert!(
        out.status.success(),
        "xerj {args:?} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("help is utf-8")
}

/// 1-based line number of the invitation, or `None` if it is not there.
fn invite_line(help: &str) -> Option<usize> {
    help.lines().position(|l| l.contains(NEEDLE)).map(|i| i + 1)
}

#[test]
fn every_help_surface_invites_a_report_by_default() {
    for args in SURFACES {
        let text = help(args, None);
        let line = invite_line(&text)
            .unwrap_or_else(|| panic!("xerj {args:?} printed no invitation:\n{text}"));
        assert!(
            line <= MAX_LINE,
            "xerj {args:?} put the invitation on line {line}; it has to be within the \
             first {MAX_LINE} lines or nobody reads it:\n{text}"
        );
        assert!(
            text.contains(ISSUES_URL),
            "xerj {args:?} invites a report without saying where:\n{text}"
        );
        // Bugs AND confusing flows; file it yourself if you can; a Discussion
        // when unsure; and never paste secrets into either.
        for needle in [
            "confused you",
            "GitHub tool",
            "Discussion",
            "secrets, API keys and private data",
        ] {
            assert!(
                text.contains(needle),
                "xerj {args:?} help is missing {needle:?}:\n{text}"
            );
        }
        // The way out has to be discoverable from the same screen.
        assert!(
            text.contains("--disable-feedback"),
            "xerj {args:?} never documents the off-switch:\n{text}"
        );
    }
}

/// `--help` is handled inside the argument loop and exits there, so a naive
/// implementation would never see a flag typed after it. Both orders, every
/// surface.
#[test]
fn disable_feedback_works_in_either_argument_order() {
    for args in SURFACES {
        let mut after: Vec<&str> = args.to_vec();
        after.push("--disable-feedback");

        // Before `--help`, but after any subcommand — argv[1] is the
        // subcommand selector for `index`/`autoindex`/`brain`.
        let mut before: Vec<&str> = args.to_vec();
        before.insert(before.len() - 1, "--disable-feedback");

        for variant in [after, before] {
            let text = help(&variant, None);
            assert_eq!(
                invite_line(&text),
                None,
                "xerj {variant:?} still printed the invitation:\n{text}"
            );
            // Silencing it must not leave a hole where it stood.
            assert!(
                !text.contains("\n\n\n"),
                "xerj {variant:?} left a blank gap behind:\n{text}"
            );
        }
    }
}

/// CI and agent harnesses often cannot change argv, only the environment.
#[test]
fn the_env_var_silences_every_surface() {
    for args in SURFACES {
        for value in ["1", "true", "yes", "on"] {
            let text = help(args, Some(value));
            assert_eq!(
                invite_line(&text),
                None,
                "XERJ_DISABLE_FEEDBACK={value} left the invitation on xerj {args:?}:\n{text}"
            );
        }
        // A false-y value is not an off-switch, and a value nobody can read
        // must not silently hide it either.
        for value in ["0", "false", "off", "maybe", ""] {
            let text = help(args, Some(value));
            assert!(
                invite_line(&text).is_some(),
                "XERJ_DISABLE_FEEDBACK={value:?} hid the invitation on xerj {args:?}:\n{text}"
            );
        }
    }
}

/// An unreadable value is reported on stderr — never on stdout, which is the
/// help text itself — and never by refusing to print the help.
#[test]
fn an_unreadable_env_value_is_reported_on_stderr_only() {
    let out = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .args(["--help"])
        .env("XERJ_DISABLE_FEEDBACK", "maybe")
        .output()
        .expect("run xerj");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("XERJ_DISABLE_FEEDBACK") && stderr.contains("not a boolean"),
        "an unreadable value was swallowed; stderr was: {stderr}"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("not a boolean"));
}

/// The help bodies are `println!`/`format!` format strings, so a literal brace
/// has to be doubled — get that wrong and either the build breaks or the JSON
/// example in the help stops being JSON. Every balanced brace group in every
/// help surface must still parse.
#[test]
fn json_examples_in_help_still_parse() {
    for args in SURFACES {
        for text in [help(args, None), help(args, Some("true"))] {
            for candidate in brace_groups(&text) {
                serde_json::from_str::<serde_json::Value>(&candidate).unwrap_or_else(|e| {
                    panic!("xerj {args:?} help contains unparseable JSON {candidate:?}: {e}")
                });
            }
        }
    }
}

/// Balanced `{…}` groups, and a hard failure on an unbalanced one — a stray
/// brace is exactly what a mis-escaped format string leaves behind.
fn brace_groups(text: &str) -> Vec<String> {
    let mut groups = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' => {
                depth = depth
                    .checked_sub(1)
                    .unwrap_or_else(|| panic!("unbalanced '}}' in help at byte {i}"));
                if depth == 0 {
                    groups.push(text[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "unclosed '{{' in help text");
    groups
}
