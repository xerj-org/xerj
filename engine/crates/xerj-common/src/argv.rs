//! Arguments after a flag that was written before the subcommand.
//!
//! `xerj --url http://127.0.0.1:9200 corpus index name` used to die in the
//! server-flag parser (`unknown argument: --url`) because the subcommand was
//! not argv[1]. [`reorder_flags_before_command`] moves that subcommand to
//! the front and keeps the flags. [`install_hoist`] records the reordered
//! list for the process; subcommands read it through [`subcommand`] and
//! [`args_after_subcommand`].

use std::sync::Mutex;

static HOISTED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// `Some` when leading flags were moved behind a subcommand. `None` leaves
/// every reader on the real process arguments.
pub fn install_hoist(argv_without_program: Option<Vec<String>>) {
    *HOISTED.lock().unwrap_or_else(|e| e.into_inner()) = argv_without_program;
}

fn hoisted() -> Option<Vec<String>> {
    HOISTED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// argv[1]: the subcommand, after a hoist when one was installed.
pub fn subcommand() -> Option<String> {
    if let Some(v) = hoisted() {
        return v.into_iter().next();
    }
    std::env::args().nth(1)
}

/// Arguments after the subcommand word. A hoist appends the flags that
/// preceded it, so `xerj --url U corpus index name` is seen as
/// `index name --url U`.
///
/// `--disable-feedback` is removed here. `feedback::enabled` still sees it
/// on the raw process arguments (it turns the help invitation off), and a
/// subcommand parser must not reject it as unknown. Parsers that also match
/// the flag themselves keep working: it is simply absent from this list.
pub fn args_after_subcommand() -> Vec<String> {
    let raw = if let Some(v) = hoisted() {
        v.into_iter().skip(1).collect::<Vec<_>>()
    } else {
        std::env::args().skip(2).collect::<Vec<_>>()
    };
    raw.into_iter()
        .filter(|a| a != crate::feedback::DISABLE_FLAG)
        .collect()
}

/// Move flags that precede a command word to the end of the list.
///
/// A token after a flag is that flag's value when it is not itself a
/// command and does not start with `-`. A flag whose next token is a
/// command is boolean (`xerj --insecure corpus list`). Returns `None` when
/// the first token is already the command, or when no command follows the
/// flags (a bare `xerj --insecure` stays a server start).
pub fn reorder_flags_before_command(
    args: &[String],
    is_command: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let first = args.first()?;
    if !first.starts_with('-') || is_command(first) {
        return None;
    }
    let mut i = 0;
    let mut leading = Vec::new();
    while i < args.len() && args[i].starts_with('-') && !is_command(&args[i]) {
        leading.push(args[i].clone());
        i += 1;
        if i < args.len() && !args[i].starts_with('-') && !is_command(&args[i]) {
            leading.push(args[i].clone());
            i += 1;
        }
    }
    if leading.is_empty() || i >= args.len() || !is_command(&args[i]) {
        return None;
    }
    let mut out = Vec::with_capacity(args.len());
    out.push(args[i].clone());
    out.extend(args[i + 1..].iter().cloned());
    out.extend(leading);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_top(s: &str) -> bool {
        matches!(s, "corpus" | "code" | "autoindex" | "def")
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_url_before_corpus_is_passed_to_the_subcommand() {
        let got = reorder_flags_before_command(
            &v(&["--url", "http://127.0.0.1:9200", "corpus", "index", "name"]),
            is_top,
        )
        .unwrap();
        assert_eq!(
            got,
            v(&["corpus", "index", "name", "--url", "http://127.0.0.1:9200"])
        );
    }

    #[test]
    fn a_boolean_flag_does_not_swallow_the_subcommand() {
        let got =
            reorder_flags_before_command(&v(&["--insecure", "corpus", "list"]), is_top).unwrap();
        assert_eq!(got, v(&["corpus", "list", "--insecure"]));
    }

    #[test]
    fn a_server_start_with_only_flags_is_left_alone() {
        assert!(reorder_flags_before_command(&v(&["--insecure"]), is_top).is_none());
        assert!(
            reorder_flags_before_command(&v(&["--url", "http://127.0.0.1:9200"]), is_top).is_none()
        );
    }

    #[test]
    fn disable_feedback_is_stripped_before_a_subcommand_parser_sees_argv() {
        let _guard = HoistGuard;
        install_hoist(Some(v(&[
            "init",
            "--disable-feedback",
            "--dry-run",
            "--url",
            "http://127.0.0.1:9",
        ])));
        assert_eq!(
            args_after_subcommand(),
            v(&["--dry-run", "--url", "http://127.0.0.1:9"])
        );
        install_hoist(Some(v(&["search", "--disable-feedback", "needle"])));
        assert_eq!(args_after_subcommand(), v(&["needle"]));
    }

    /// Clears the process-global hoist even when the test fails.
    struct HoistGuard;
    impl Drop for HoistGuard {
        fn drop(&mut self) {
            install_hoist(None);
        }
    }

    #[test]
    fn a_subcommand_already_in_front_is_left_alone() {
        assert!(reorder_flags_before_command(
            &v(&["corpus", "--url", "http://x", "index"]),
            is_top
        )
        .is_none());
    }
}
