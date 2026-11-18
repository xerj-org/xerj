//! Local admin-key discovery and the loopback guard that makes it safe.
//!
//! The shipped default config has `[auth] enabled = true`, and the server
//! mints `<data_dir>/admin.key` on first start. Commands that talk to a
//! node from the same machine (`xerj mcp`, `xerj autoindex`, `xerj init`'s
//! advice) can read that file instead of demanding the user paste a key —
//! but only when the target is provably loopback, because a locally
//! readable key sent anywhere else is a credential leak, not a
//! convenience.
//!
//! Both helpers started life in `xerj-autoindex/src/cli.rs` (the autoindex
//! 401 fix); `xerj mcp` hit the identical failure mode (#961), so they
//! moved here rather than being duplicated. The invariants live with the
//! tests in `xerj-autoindex` (`url_is_loopback_is_true_only_for_this_machine`,
//! `userinfo_cannot_disguise_a_remote_host_as_loopback`, and the
//! `discover_local_admin_key` sandbox tests) — the calling crates keep
//! wiring tests of their own.
//!
//! # Discovery order
//!
//! [`discover_local_admin_key_for_url`] is the entry point every command
//! that talks to a node uses. It tries, in order, and stops at the first
//! non-empty file:
//!
//! 1. `$HOME/.xerj/by-port/<port>` — a pointer the server publishes on boot
//!    ([`publish_admin_key_for_port`]). It records the publishing node's
//!    `admin.key` path. The port comes from [`xerj_url_port`]; an absent or
//!    unreadable pointer falls through to the remaining candidates.
//! 2. `./data/admin.key`, `./.xerj-data/admin.key`, `./xerj-data/admin.key`
//!    — resolved against the working directory.
//! 3. `/var/lib/xerj/admin.key` — the packaged install's data dir.
//! 4. `$HOME/.xerj/brain/admin.key`, `$HOME/xerj-data/admin.key`,
//!    `$HOME/.xerj/admin.key`.
//!
//! [`discover_local_admin_key`] is the same list without step 1.
//!
//! Because step 1 follows a pointer the *server* wrote, the key it finds can
//! belong to a completely different project than the one the command was run
//! in. Packaged and home-directory keys may also be outside the project, and
//! a pointer can outlive its node. Discovery therefore reports [`KeyOrigin`],
//! without claiming that the named node is still listening. Every caller
//! announces it, and [`DiscoveredKey::foreign_warning`] flags a key that is
//! not under the working tree. None of that changes which key is chosen; it
//! makes the choice visible, and `--api-key` / `XERJ_API_KEY` still win.
//!
//! The URL parsing uses a real parser (`url::Url`, the same type reqwest
//! re-exports) rather than string surgery, because the hand-rolled version
//! of this was wrong in a way that leaked credentials: splitting on the
//! last `:` treats the userinfo in `http://localhost:9200@evil.com/` as a
//! host:port pair, judges it loopback, and sends the admin key to
//! `evil.com`. `Url::host_str` resolves that to `evil.com`, which is the
//! whole point of using it.

use std::path::PathBuf;

/// True when `url`'s host is this machine, so a locally readable admin key
/// is the right credential to send. Anything else — a LAN address, a
/// hostname, a remote deployment — must be given a key explicitly.
pub fn url_is_loopback(url: &str) -> bool {
    // A schemeless `localhost:9200` parses as scheme `localhost`, path `9200`,
    // with no host at all, so retry those through `http://`. The retry still
    // goes through the parser: `localhost:9200@evil.com` becomes
    // `http://localhost:9200@evil.com`, whose host is `evil.com`, not loopback.
    let parsed = match url::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => u,
        _ => match url::Url::parse(&format!("http://{url}")) {
            Ok(u) => u,
            // Unparseable is not loopback. Failing closed here costs a user
            // with an exotic URL one explicit --api-key; failing open costs
            // them the key itself.
            Err(_) => return false,
        },
    };
    match parsed.host_str() {
        // `host_str` strips the brackets from `[::1]` and does not lowercase
        // an IPv6 literal, so compare case-insensitively and cover both forms.
        Some(h) => {
            let h = h.trim_start_matches('[').trim_end_matches(']');
            h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1" || h == "0.0.0.0"
        }
        None => false,
    }
}

/// Which step of the discovery order found the key — the answer to "why
/// *this* file?", which is what #2578 was actually asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOrigin {
    /// `$HOME/.xerj/by-port/<port>`: the node listening on that port
    /// published its own data dir on boot. The pointer can outlive the node
    /// and may name a different project than the caller's.
    PublishedByNode { port: u16 },
    /// One of the `./…/admin.key` candidates, resolved against the working
    /// directory.
    WorkingDirectory,
    /// The packaged install at `/var/lib/xerj/admin.key`.
    PackagedInstall,
    /// One of the `$HOME/…/admin.key` candidates.
    Home,
}

/// A discovered admin key, plus where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredKey {
    /// The bare key, trimmed. Callers format the header per their own wire
    /// convention (`ApiKey <key>`).
    pub key: String,
    /// The file the key was read from, absolute where the candidate was
    /// relative: an announcement must name a path the reader can act on even
    /// when the process was spawned with an arbitrary cwd, which is exactly
    /// what MCP hosts do.
    pub path: PathBuf,
    pub origin: KeyOrigin,
}

impl DiscoveredKey {
    /// The provenance clause for an announcement, e.g.
    /// `published by the node on port 9200`.
    pub fn provenance(&self) -> String {
        match self.origin {
            KeyOrigin::PublishedByNode { port } => {
                format!("published by the node on port {port}")
            }
            KeyOrigin::WorkingDirectory => "found under the working directory".to_string(),
            KeyOrigin::PackagedInstall => "found in the packaged installation".to_string(),
            KeyOrigin::Home => "found under $HOME".to_string(),
        }
    }

    /// A warning when the key belongs to a directory outside the working
    /// tree, or `None` when it does not.
    ///
    /// This is a proxy for the check the issue asks for — "is this the data
    /// dir the node at `XERJ_URL` is running from?" — because answering that
    /// exactly needs an authenticated request to the node, and discovery runs
    /// before the first request. The working tree is the one local fact that
    /// distinguishes "my project's node" from "a node another project left
    /// listening", which is the case the field report hit.
    ///
    /// `explicit_auth` names the per-command flag and environment variable,
    /// e.g. `--auth or XERJ_AUTH` for MCP.
    pub fn foreign_warning(&self, explicit_auth: &str) -> Option<String> {
        let cwd = std::env::current_dir().ok();
        self.foreign_warning_against(cwd.as_deref(), explicit_auth)
    }

    /// [`Self::foreign_warning`] with the working directory passed in, so the
    /// judgement is testable without moving process-global state.
    fn foreign_warning_against(
        &self,
        cwd: Option<&std::path::Path>,
        explicit_auth: &str,
    ) -> Option<String> {
        let cwd = cwd?;
        // The key lives in `<data_dir>/admin.key`; the data dir is what has to
        // be inside the working tree, not the file.
        let data_dir = self.path.parent().unwrap_or(&self.path);
        if path_is_under(data_dir, cwd) {
            return None;
        }
        Some(format!(
            "that key is outside the working directory ({}), so it may belong to a \
             different project's node; pass {explicit_auth} to choose \
             explicitly",
            cwd.display()
        ))
    }
}

/// True when `child` is `parent` or below it, after resolving symlinks so a
/// `/var` vs `/private/var` difference on macOS does not read as "unrelated".
fn path_is_under(child: &std::path::Path, parent: &std::path::Path) -> bool {
    let resolve =
        |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(child).starts_with(resolve(parent))
}

/// The admin key a local server wrote for itself, if we can find it.
///
/// Checked in the order a user is most likely to have created them: the
/// working directory's data dir (what the quickstart tells you to use), then
/// the documented package install location. Absent or unreadable is not an
/// error — the caller falls back to the actionable 401 message.
///
/// Returns the bare key (trimmed) and the file it came from. Relative
/// candidates are resolved against the current directory before being
/// returned.
fn read_admin_key(path: &std::path::Path) -> Option<(String, PathBuf)> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|k| (k, path.to_path_buf()))
}

/// `~/.xerj/by-port/<port>` — one line, the absolute path of that listener's
/// `admin.key`. The server writes it on boot. Discovery reads the path, not
/// a second copy of the key.
fn port_pointer(port: u16) -> Option<PathBuf> {
    crate::home::home_dir().map(|home| home.join(".xerj").join("by-port").join(port.to_string()))
}

/// Record which data dir is serving `port`, so a later command on this
/// machine can find that node's admin key without guessing `--data-dir`.
/// The file holds the key's path. A missing home directory is not an error.
pub fn publish_admin_key_for_port(
    port: u16,
    admin_key_file: &std::path::Path,
) -> std::io::Result<()> {
    let Some(dest) = port_pointer(port) else {
        return Ok(());
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let abs =
        std::fs::canonicalize(admin_key_file).unwrap_or_else(|_| admin_key_file.to_path_buf());
    std::fs::write(&dest, format!("{}\n", abs.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Port of a XERJ URL. An explicit port wins; a URL with none is the product
/// default `9200` (not the scheme default 80/443).
pub fn xerj_url_port(url: &str) -> u16 {
    // A schemeless `localhost:9207` parses as scheme `localhost`, so retry
    // through `http://` the same way `url_is_loopback` does. An explicit port
    // wins; a URL with none is the product default 9200, not 80/443.
    let parsed = match url::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => Some(u),
        _ => url::Url::parse(&format!("http://{url}")).ok(),
    };
    parsed.and_then(|u| u.port()).unwrap_or(9200)
}

/// [`discover_local_admin_key`], but the server's per-port pointer is tried
/// first. Callers must still refuse to send the key anywhere but loopback.
///
/// Prefer [`discover_local_admin_key_for_url_with_origin`] when announcing the
/// result: the origin is what tells a reader why this key was chosen.
pub fn discover_local_admin_key_for_url(url: &str) -> Option<(String, PathBuf)> {
    discover_local_admin_key_for_url_with_origin(url).map(|found| (found.key, found.path))
}

/// [`discover_local_admin_key_for_url`] with the discovery step that found the
/// key, so the caller can announce *why* that file (#2578).
pub fn discover_local_admin_key_for_url_with_origin(url: &str) -> Option<DiscoveredKey> {
    let port = xerj_url_port(url);
    if let Some(pointer) = port_pointer(port) {
        if let Ok(text) = std::fs::read_to_string(&pointer) {
            let target = PathBuf::from(text.trim());
            if let Some((key, path)) = read_admin_key(&target) {
                return Some(DiscoveredKey {
                    key,
                    path,
                    origin: KeyOrigin::PublishedByNode { port },
                });
            }
        }
    }
    discover_local_admin_key_with_origin()
}

pub fn discover_local_admin_key() -> Option<(String, PathBuf)> {
    discover_local_admin_key_with_origin().map(|found| (found.key, found.path))
}

/// [`discover_local_admin_key`] with the discovery step that found the key.
pub fn discover_local_admin_key_with_origin() -> Option<DiscoveredKey> {
    let cwd = std::env::current_dir().ok();
    let home = crate::home::home_dir();
    local_admin_key_candidates(cwd.as_deref(), home.as_deref())
        .into_iter()
        .find_map(|(path, origin)| {
            read_admin_key(&path).map(|(key, path)| DiscoveredKey { key, path, origin })
        })
}

fn local_admin_key_candidates(
    cwd: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Vec<(PathBuf, KeyOrigin)> {
    // Keep the existing discovery order and path resolution. The packaged
    // installation has its own provenance even when no home is available.
    const CANDIDATES: &[(&str, KeyOrigin)] = &[
        ("data/admin.key", KeyOrigin::WorkingDirectory),
        (".xerj-data/admin.key", KeyOrigin::WorkingDirectory),
        ("xerj-data/admin.key", KeyOrigin::WorkingDirectory),
        ("/var/lib/xerj/admin.key", KeyOrigin::PackagedInstall),
    ];
    let mut paths: Vec<(PathBuf, KeyOrigin)> = CANDIDATES
        .iter()
        .map(|(candidate, origin)| {
            let path = PathBuf::from(candidate);
            let path = match (cwd, path.is_absolute()) {
                (Some(cwd), false) => cwd.join(path),
                _ => path,
            };
            (path, *origin)
        })
        .collect();
    if let Some(home) = home {
        paths.push((home.join(".xerj/brain/admin.key"), KeyOrigin::Home));
        paths.push((home.join("xerj-data/admin.key"), KeyOrigin::Home));
        paths.push((home.join(".xerj/admin.key"), KeyOrigin::Home));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_at(path: &str, origin: KeyOrigin) -> DiscoveredKey {
        DiscoveredKey {
            key: "k".to_string(),
            path: PathBuf::from(path),
            origin,
        }
    }

    /// The whole point of #2578: the announcement has to distinguish a key the
    /// node on this port published from one found by walking the candidates,
    /// so a reader can distinguish the port pointer from fallback paths.
    #[test]
    fn provenance_names_the_step_that_found_the_key() {
        assert_eq!(
            key_at(
                "/other-project/data/admin.key",
                KeyOrigin::PublishedByNode { port: 9200 }
            )
            .provenance(),
            "published by the node on port 9200"
        );
        assert_eq!(
            key_at("/proj/data/admin.key", KeyOrigin::WorkingDirectory).provenance(),
            "found under the working directory"
        );
        assert_eq!(
            key_at("/home/u/.xerj/admin.key", KeyOrigin::Home).provenance(),
            "found under $HOME"
        );
    }

    #[test]
    fn local_candidates_preserve_order_and_classify_the_packaged_install() {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let candidates = local_admin_key_candidates(Some(cwd.path()), Some(home.path()));
        assert_eq!(candidates.len(), 7);
        assert_eq!(
            candidates[0],
            (
                cwd.path().join("data/admin.key"),
                KeyOrigin::WorkingDirectory
            )
        );
        assert_eq!(
            candidates[1],
            (
                cwd.path().join(".xerj-data/admin.key"),
                KeyOrigin::WorkingDirectory
            )
        );
        assert_eq!(
            candidates[2],
            (
                cwd.path().join("xerj-data/admin.key"),
                KeyOrigin::WorkingDirectory
            )
        );
        assert_eq!(candidates[3].1, KeyOrigin::PackagedInstall);
        assert_eq!(
            key_at("/var/lib/xerj/admin.key", candidates[3].1).provenance(),
            "found in the packaged installation"
        );
        assert_eq!(
            candidates[4],
            (home.path().join(".xerj/brain/admin.key"), KeyOrigin::Home)
        );
        assert_eq!(
            candidates[5],
            (home.path().join("xerj-data/admin.key"), KeyOrigin::Home)
        );
        assert_eq!(
            candidates[6],
            (home.path().join(".xerj/admin.key"), KeyOrigin::Home)
        );
        let no_cwd = local_admin_key_candidates(None, None);
        assert_eq!(no_cwd.len(), 4);
        assert_eq!(no_cwd[0].1, KeyOrigin::WorkingDirectory);
        assert_eq!(no_cwd[3].1, KeyOrigin::PackagedInstall);
    }

    #[test]
    fn a_key_inside_the_working_tree_is_not_flagged() {
        let found = key_at("/proj/data/admin.key", KeyOrigin::WorkingDirectory);
        assert_eq!(
            found.foreign_warning_against(
                Some(std::path::Path::new("/proj")),
                "--api-key or XERJ_API_KEY"
            ),
            None,
            "the project's own ./data key is the expected answer, not a warning"
        );
    }

    #[test]
    fn a_key_from_another_projects_data_dir_is_flagged_with_the_escape_hatch() {
        let found = key_at(
            "/home/u/Projects/other/data/admin.key",
            KeyOrigin::PublishedByNode { port: 9200 },
        );
        let warning = found
            .foreign_warning_against(
                Some(std::path::Path::new("/home/u/Projects/mine")),
                "--api-key or XERJ_API_KEY",
            )
            .expect("a key outside the working tree must warn");
        assert!(warning.contains("/home/u/Projects/mine"), "{warning}");
        assert!(warning.contains("--api-key or XERJ_API_KEY"), "{warning}");
    }

    /// MCP has no `--api-key`; the warning must name the flag that surface
    /// actually accepts.
    #[test]
    fn the_warning_names_the_flag_that_surface_takes() {
        let found = key_at(
            "/elsewhere/data/admin.key",
            KeyOrigin::PublishedByNode { port: 9201 },
        );
        let warning = found
            .foreign_warning_against(Some(std::path::Path::new("/proj")), "--auth or XERJ_AUTH")
            .unwrap();
        assert!(warning.contains("--auth or XERJ_AUTH"), "{warning}");
        assert!(!warning.contains("--api-key"), "{warning}");
        assert!(!warning.contains("XERJ_API_KEY"), "{warning}");
    }

    /// A sibling directory that merely shares a prefix is not "inside": the
    /// comparison is per path component.
    #[test]
    fn a_sibling_with_a_shared_prefix_is_outside() {
        let found = key_at("/proj-other/data/admin.key", KeyOrigin::WorkingDirectory);
        assert!(found
            .foreign_warning_against(
                Some(std::path::Path::new("/proj")),
                "--api-key or XERJ_API_KEY"
            )
            .is_some());
    }

    /// Nothing to compare against means no claim either way — MCP hosts can
    /// start a server whose cwd was already deleted.
    #[test]
    fn an_unknown_working_directory_is_silent() {
        let found = key_at("/proj/data/admin.key", KeyOrigin::WorkingDirectory);
        assert_eq!(
            found.foreign_warning_against(None, "--api-key or XERJ_API_KEY"),
            None
        );
    }

    /// The candidates are resolved against the working directory and reported
    /// absolute, so the announcement names a path the reader can act on.
    #[test]
    fn a_working_directory_candidate_is_reported_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("data/admin.key");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "from-disk\n").unwrap();
        assert_eq!(
            read_admin_key(&key).map(|(k, p)| (k, p == key)),
            Some(("from-disk".to_string(), true))
        );
        assert_eq!(read_admin_key(&dir.path().join("missing/admin.key")), None);
    }
}
