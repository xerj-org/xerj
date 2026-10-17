//! Arguments after a flag that was written before the subcommand.
//!
//! `xerj --url http://127.0.0.1:9200 corpus index name` used to die in the
//! server-flag parser (`unknown argument: --url`) because the subcommand was
//! not argv[1]. [`reorder_flags_before_command`] moves that subcommand to
//! the front and keeps the flags. [`install_hoist`] records the reordered
//! list for the process; subcommands read it through [`subcommand`] and
//! [`args_after_subcommand`].

use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

static HOISTED: Mutex<Option<Vec<OsString>>> = Mutex::new(None);

/// `Some` when leading flags were moved behind a subcommand. `None` leaves
/// every reader on the real process arguments. Tokens stay `OsString` so a
/// non-UTF-8 path (`quarterly-\xFF.pdf`) does not panic the way
/// [`std::env::args`] does.
pub fn install_hoist(argv_without_program: Option<Vec<OsString>>) {
    *HOISTED.lock().unwrap_or_else(|e| e.into_inner()) = argv_without_program;
}

fn hoisted() -> Option<Vec<OsString>> {
    HOISTED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// argv[1]: the subcommand, after a hoist when one was installed.
///
/// Command words are Unicode. A non-UTF-8 first token is not a subcommand.
pub fn subcommand() -> Option<String> {
    let os = if let Some(v) = hoisted() {
        v.into_iter().next()
    } else {
        std::env::args_os().nth(1)
    }?;
    os.into_string().ok()
}

/// Arguments after the subcommand word. A hoist appends the flags that
/// preceded it, so `xerj --url U corpus index name` is seen as
/// `index name --url U`.
///
/// A non-UTF-8 token is rendered with [`OsStr::to_string_lossy`] so this
/// `String` view cannot panic. The PDF and XLS workers do not use it: they
/// read [`std::env::args_os`] and open the path as bytes.
pub fn args_after_subcommand() -> Vec<String> {
    let os: Vec<OsString> = if let Some(v) = hoisted() {
        v.into_iter().skip(1).collect()
    } else {
        std::env::args_os().skip(2).collect()
    };
    os.into_iter().map(os_to_string).collect()
}

fn os_to_string(os: OsString) -> String {
    os.into_string()
        .unwrap_or_else(|os| os.to_string_lossy().into_owned())
}

/// Move flags that precede a command word to the end of the list.
///
/// `T` is [`String`] for the in-process parsers and [`OsString`] for process
/// argv. A token after a flag is that flag's value when it is not itself a
/// command and does not start with `-`, including a non-UTF-8 path. A flag
/// whose next token is a command is boolean (`xerj --insecure corpus list`).
/// Returns `None` when the first token is already the command, or when no
/// command follows the flags (a bare `xerj --insecure` stays a server start).
pub fn reorder_flags_before_command<T>(
    args: &[T],
    is_command: impl Fn(&str) -> bool,
) -> Option<Vec<T>>
where
    T: AsRef<OsStr> + Clone,
{
    let first = args.first()?;
    if !is_leading_flag(first.as_ref(), &is_command) {
        return None;
    }
    let mut i = 0;
    let mut leading = Vec::new();
    while i < args.len() && is_leading_flag(args[i].as_ref(), &is_command) {
        leading.push(args[i].clone());
        i += 1;
        if i < args.len() && is_flag_value(args[i].as_ref(), &is_command) {
            leading.push(args[i].clone());
            i += 1;
        }
    }
    if leading.is_empty() || i >= args.len() || !is_command_token(args[i].as_ref(), &is_command) {
        return None;
    }
    let mut out = Vec::with_capacity(args.len());
    out.push(args[i].clone());
    out.extend(args[i + 1..].iter().cloned());
    out.extend(leading);
    Some(out)
}

fn is_command_token(s: &OsStr, is_command: &impl Fn(&str) -> bool) -> bool {
    s.to_str().is_some_and(is_command)
}

fn is_leading_flag(s: &OsStr, is_command: &impl Fn(&str) -> bool) -> bool {
    match s.to_str() {
        Some(text) => text.starts_with('-') && !is_command(text),
        None => false,
    }
}

/// A flag's value: not a flag, not a command. Non-UTF-8 bytes are a value
/// (a path), never a command word.
fn is_flag_value(s: &OsStr, is_command: &impl Fn(&str) -> bool) -> bool {
    match s.to_str() {
        Some(text) => !text.starts_with('-') && !is_command(text),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

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
    fn a_subcommand_already_in_front_is_left_alone() {
        assert!(reorder_flags_before_command(
            &v(&["corpus", "--url", "http://x", "index"]),
            is_top
        )
        .is_none());
    }

    /// `std::env::args` panics on this path. The reorder must keep the bytes.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_flag_value_round_trips_as_os_string() {
        use std::os::unix::ffi::OsStringExt;

        let path = OsString::from_vec(b"quarterly-\xff.pdf".to_vec());
        let args = vec![
            OsString::from("--file"),
            path.clone(),
            OsString::from("__extract-pdf"),
        ];
        let got = reorder_flags_before_command(&args, |s| s == "__extract-pdf").unwrap();
        assert_eq!(
            got,
            vec![
                OsString::from("__extract-pdf"),
                OsString::from("--file"),
                path
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_argument_after_the_subcommand_is_not_rewritten() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![
            OsString::from("__extract-pdf"),
            OsString::from_vec(b"quarterly-\xff.pdf".to_vec()),
        ];
        assert!(reorder_flags_before_command(&args, |s| s == "__extract-pdf").is_none());
    }
}
