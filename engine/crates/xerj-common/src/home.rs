//! The user's home directory, resolved the same way on every platform.
//!
//! `$HOME` is the Unix convention and is usually **unset on native Windows**,
//! where the profile directory is `%USERPROFILE%`. Reading `HOME` alone sent
//! Windows users' hub cache and agent config to `./.xerj/...` (the current
//! directory) or skipped them entirely. This is the one rule: a non-empty
//! `HOME`, else a non-empty `USERPROFILE`, else `None`.

use std::ffi::OsString;
use std::path::PathBuf;

/// The home directory from the process environment. See [`home_dir_from`].
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(&|key| std::env::var_os(key))
}

/// [`home_dir`] over an injectable environment, so the rule is testable without
/// mutating the process-global one (tests run in parallel).
pub fn home_dir_from(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|key| env(key).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn home_wins_over_userprofile() {
        let e = env(&[("HOME", "/home/u"), ("USERPROFILE", r"C:\Users\u")]);
        assert_eq!(home_dir_from(&e), Some(PathBuf::from("/home/u")));
    }

    #[test]
    fn userprofile_is_the_windows_fallback() {
        let e = env(&[("USERPROFILE", r"C:\Users\u")]);
        assert_eq!(home_dir_from(&e), Some(PathBuf::from(r"C:\Users\u")));
    }

    #[test]
    fn empty_home_falls_through_to_userprofile() {
        let e = env(&[("HOME", ""), ("USERPROFILE", r"C:\Users\u")]);
        assert_eq!(home_dir_from(&e), Some(PathBuf::from(r"C:\Users\u")));
    }

    #[test]
    fn neither_set_is_none() {
        assert_eq!(home_dir_from(&env(&[])), None);
        assert_eq!(
            home_dir_from(&env(&[("HOME", ""), ("USERPROFILE", "")])),
            None
        );
    }
}
