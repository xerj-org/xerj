//! The hard gate every manifest-derived path passes BEFORE anything is built
//! from it.
//!
//! A hub manifest is UNTRUSTED INPUT: `--from` is documented as "rebuild a
//! corpus someone else defined", so the file arrives from a hub, a chat
//! message or a pull request, and every field in it is attacker-controlled.
//! `repo` becomes a filesystem path (`corpora/<name>/<repo>`) that the
//! lifecycle then runs `git checkout --force` and `git clean -fd` inside, so
//! a name like `../../../work/repo` walks out of ~/.xerj-code and destroys
//! uncommitted work in an unrelated checkout.
//!
//! The original script validated twice (python then bash) because of the
//! checkout; the port validates ONCE, unskippably, before any path exists.

/// Subcommand nouns a corpus name must not shadow — `xerj corpus list` sorts
/// by these words' meaning, and a corpus called "index" or "list" makes the
/// lifecycle's own help text ambiguous.
pub const RESERVED_CORPUS_NAMES: &[&str] = &[
    "code", "corpus", "add", "index", "list", "own", "def", "search", "help",
];

/// Validate a corpus name: no `/` (no path escape), no `*` (no glob
/// injection into index patterns), no leading `.` (no hidden dir, no `..`),
/// not empty, not a reserved subcommand noun.
pub fn valid_corpus_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("corpus name must not be empty".to_string());
    }
    if name.contains('/') {
        return Err(format!(
            "bad corpus name '{name}': it becomes a directory under the corpora root, so \
             '/' would escape it"
        ));
    }
    if name.contains('*') {
        return Err(format!(
            "bad corpus name '{name}': '*' would be interpreted as an index wildcard"
        ));
    }
    if name.starts_with('.') {
        return Err(format!(
            "bad corpus name '{name}': a leading '.' hides the corpus directory"
        ));
    }
    if RESERVED_CORPUS_NAMES.contains(&name) {
        return Err(format!(
            "corpus name '{name}' is reserved — it is a subcommand of `xerj code`/`xerj corpus`"
        ));
    }
    // #1846: the name is also the index prefix (`xc-<name>-b<stamp>`), and
    // `xerj autoindex` keeps only lower-case [a-z0-9] runs joined by '-' in a
    // prefix. `curl_cffi` was registered, indexed as `xc-curl-cffi-…`, then
    // verified and listed under `xc-curl_cffi-…`: records on the node, a corpus
    // that answered nothing. Refuse up front and name the spelling that works.
    let index_safe = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    if !name.chars().all(index_safe) || name.starts_with('-') || name.ends_with('-') {
        let suggested = index_safe_corpus_name(name);
        let instead = if suggested.is_empty() {
            "use a name made of a-z, 0-9 and '-'".to_string()
        } else {
            format!(
                "use '{suggested}' instead (an existing corpus: rename corpora/{name} to \
                 corpora/{suggested} and set \"corpus\": \"{suggested}\" in its corpus.json)"
            )
        };
        return Err(format!(
            "bad corpus name '{name}': a corpus name is also its index prefix, which keeps only \
             lower-case a-z, 0-9 and '-', so '{name}' would be indexed under xc-{suggested}-… \
             and then looked up under xc-{name}-…, where nothing is — {instead}"
        ));
    }
    Ok(())
}

/// The spelling of `name` that survives as an index prefix unchanged: lower
/// case, every run of other characters turned into one `-`, no `-` at either
/// end. A run of only `-` is kept as written (the hub's `owner--repo`). Used to suggest a
/// name in [`valid_corpus_name`]'s refusal; empty when nothing survives.
pub fn index_safe_corpus_name(name: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if !out.is_empty() && !run.is_empty() {
                if run.chars().all(|r| r == '-') {
                    out.push_str(&run);
                } else {
                    out.push('-');
                }
            }
            run.clear();
            out.push(c.to_ascii_lowercase());
        } else {
            run.push(c);
        }
    }
    out
}

/// Validate a repo name from an untrusted manifest: `^[A-Za-z0-9_][A-Za-z0-9._-]*$`.
///
/// The message says WHY because the rule looks arbitrary until the checkout
/// is mentioned: this name becomes a path that is then force-checked-out and
/// cleaned.
pub fn valid_repo_name(repo: &str) -> Result<(), String> {
    let mut chars = repo.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if first_ok && rest_ok {
        return Ok(());
    }
    Err(format!(
        "repo name '{repo}' is not a plain directory name. It is used as a path under the \
         corpus directory and is then force-checked-out and cleaned, so a name containing \
         '/' or '..' would overwrite files outside the corpus. Allowed: letters, digits, \
         '.', '_' and '-', not starting with '.' or '-'."
    ))
}

/// How many paths one repo entry may name. A harvest is a few subtrees
/// (`data/osv`) or files, not a file list; past this, name a parent.
pub const MAX_SPARSE_PATHS: usize = 32;

/// Validate one `paths` entry from an untrusted hub manifest and return the
/// normalized path (`data/osv`, no leading or trailing slash). It may name a
/// directory or a file; the checkout decides which at the pin (#2486).
///
/// The entry becomes a non-cone sparse-checkout pattern, `/{path}/` for a
/// directory and `/{path}` for a file. That pattern is gitignore syntax, so a
/// glob, a negation, or `..` is not a path name; it is an instruction.
/// Reject those before git sees them.
/// An absolute path is rejected rather than stripped: stripping `/etc` into
/// `etc` would harvest a different directory than the one the manifest named.
pub fn valid_sparse_path(raw: &str) -> Result<String, String> {
    let reject = |why: &str| {
        Err(format!(
            "'paths' entry {raw:?} {why}. Name a relative directory or file such as \
             \"data/osv\"; omit 'paths' to harvest the whole pin"
        ))
    };
    if raw.chars().any(|c| c.is_control()) {
        return reject("contains a control character");
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return reject("is empty");
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return reject("is absolute");
    }
    let mut chars = trimmed.chars();
    if matches!(
        (chars.next(), chars.next()),
        (Some(c), Some(':')) if c.is_ascii_alphabetic()
    ) {
        return reject("is absolute");
    }
    if trimmed.contains('\\') {
        return reject("contains a backslash");
    }
    let body = trimmed.trim_end_matches('/');
    if body.is_empty() {
        return reject("is empty");
    }
    if body.len() > 240 {
        return reject("is longer than 240 bytes");
    }
    let mut parts = Vec::new();
    for component in body.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return reject("escapes the repository");
        }
        if component.starts_with('-') {
            return reject("must not start a path component with '-'");
        }
        if component.contains(':') {
            // git reads `<sha>:<path>` and `:(magic)` pathspecs specially,
            // so a colon in the path name is not a path.
            return reject("contains ':'");
        }
        if component
            .chars()
            .any(|c| matches!(c, '*' | '?' | '[' | ']' | '!' | '#'))
        {
            return reject("contains a glob or gitignore pattern character");
        }
        if component.chars().any(|c| c.is_whitespace()) {
            return reject("contains whitespace");
        }
        parts.push(component);
    }
    Ok(parts.join("/"))
}

/// Validate a pinned sha: full 40 lowercase hex.
///
/// A short sha is not fetchable from a remote (`git fetch --depth 1 origin
/// <short>` fails with "couldn't find remote ref"), so a manifest carrying
/// one would rebuild silently at the tip — the opposite of a pin. Fail
/// loudly, with the regeneration advice, instead.
pub fn valid_sha(repo: &str, sha: &str) -> Result<(), String> {
    let full_hex = sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit());
    // The detector records what git prints; lowercase is the convention, but
    // rejecting uppercase would break manifests written by tools that
    // uppercase — accept either case, both are full shas.
    if full_hex {
        Ok(())
    } else {
        Err(format!(
            "repo '{repo}' has sha '{sha}', which is not a full 40-character sha. A short \
             sha is not fetchable from a remote, so this manifest cannot pin a rebuild. \
             Regenerate it with `xerj corpus add <name> <url>...` on a machine that has \
             the corpus."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_corpus_names_are_rejected_before_any_path_is_built() {
        for bad in ["", "/", "a/b", "*", "a*b", ".", "..", ".hidden"] {
            assert!(valid_corpus_name(bad).is_err(), "{bad:?} must be rejected");
        }
        for reserved in RESERVED_CORPUS_NAMES {
            assert!(
                valid_corpus_name(reserved).is_err(),
                "'{reserved}' is a subcommand noun"
            );
        }
        for good in ["kv", "xerj-search", "battle-terse", "owner--repo", "lz4"] {
            assert!(valid_corpus_name(good).is_ok(), "{good:?} is a fine name");
        }
    }

    /// #1846: `curl_cffi` was accepted, indexed as `xc-curl-cffi-…` and then
    /// looked up as `xc-curl_cffi-…` — registered, indexed, unreachable. A name
    /// the index prefix would rewrite is refused, and the refusal names the
    /// spelling that works and how to rename an existing corpus.
    #[test]
    fn a_name_the_index_prefix_would_rewrite_is_refused_with_the_working_spelling() {
        let e = valid_corpus_name("curl_cffi").unwrap_err();
        assert!(e.contains("use 'curl-cffi' instead"), "{e}");
        assert!(e.contains("xc-curl-cffi-"), "{e}");
        assert!(e.contains("corpora/curl-cffi"), "{e}");
        for (bad, fix) in [
            ("rust_text", "rust-text"),
            ("Curl-Cffi", "curl-cffi"),
            ("socket.io", "socket-io"),
            ("c++", "c"),
            ("trail-", "trail"),
            ("-lead", "lead"),
            ("a__b", "a-b"),
        ] {
            let e = valid_corpus_name(bad).unwrap_err();
            assert!(e.contains(&format!("use '{fix}' instead")), "{bad}: {e}");
            assert_eq!(index_safe_corpus_name(bad), fix, "{bad}");
            assert!(
                valid_corpus_name(fix).is_ok(),
                "the suggestion {fix} must pass"
            );
        }
        assert!(valid_corpus_name("+++")
            .unwrap_err()
            .contains("a-z, 0-9 and '-'"));
        assert_eq!(index_safe_corpus_name("owner--repo"), "owner--repo");
    }

    #[test]
    fn unsafe_repo_names_are_rejected_with_the_path_escape_reason() {
        for bad in [
            "../escape",
            "a/b",
            ".hidden",
            "-flag",
            "",
            "repo name",
            "$(boom)",
        ] {
            let err = valid_repo_name(bad).unwrap_err();
            assert!(
                err.contains("force-checked-out"),
                "the message must say why: {err}"
            );
        }
        for good in ["valkey", "ClickHouse", "x_2", "a.b-c", "Sled"] {
            assert!(valid_repo_name(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn short_shas_are_rejected_with_regen_advice() {
        let err = valid_sha("valkey", "e449d17").unwrap_err();
        assert!(
            err.contains("not a full 40-character sha")
                && err.contains("Regenerate it with `xerj corpus add"),
            "{err}"
        );
        let ok = "31081d9f05014003321333553bb3e657eb3da168";
        assert!(valid_sha("ClickHouse", ok).is_ok());
        assert!(valid_sha("ClickHouse", &ok.to_uppercase()).is_ok());
        assert!(valid_sha("x", &format!("{ok}1")).is_err(), "41 chars");
        assert!(valid_sha("x", "zzz").is_err());
    }

    #[test]
    fn sparse_paths_are_relative_directories_and_nothing_else() {
        assert_eq!(valid_sparse_path("data/osv").unwrap(), "data/osv");
        assert_eq!(valid_sparse_path("  data/osv/ ").unwrap(), "data/osv");
        assert_eq!(valid_sparse_path("docs").unwrap(), "docs");
        for bad in [
            "",
            " ",
            "/",
            "/etc/passwd",
            "\\\\server\\share",
            "C:/Windows",
            "..",
            "../secret",
            "data/../../etc",
            "data//osv",
            "data/./osv",
            "data/osv/*",
            "data/osv/**",
            "data:osv",
            "!",
            "#only",
            "-rf",
            "data/osv extra",
            "data/osv\ncmd",
        ] {
            let err = valid_sparse_path(bad).unwrap_err();
            assert!(
                err.contains("relative directory"),
                "{bad:?} should be rejected with the reason: {err}"
            );
        }
    }
}
