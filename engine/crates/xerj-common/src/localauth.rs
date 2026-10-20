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

/// The admin key a local server wrote for itself, if we can find it.
///
/// Checked in the order a user is most likely to have created them: the
/// working directory's data dir (what the quickstart tells you to use), then
/// the documented package install location. Absent or unreadable is not an
/// error — the caller falls back to the actionable 401 message.
///
/// Returns the bare key (trimmed) and the file it came from, so the caller
/// can announce the path — never silently — and format the header per its
/// own wire convention (`ApiKey <key>`). Relative candidates are resolved
/// against the current directory before being returned: the announcement
/// must name a path the reader can act on even when the calling process was
/// spawned with an arbitrary cwd, which is exactly what MCP hosts do.
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
pub fn discover_local_admin_key_for_url(url: &str) -> Option<(String, PathBuf)> {
    let port = xerj_url_port(url);
    if let Some(pointer) = port_pointer(port) {
        if let Ok(text) = std::fs::read_to_string(&pointer) {
            let target = PathBuf::from(text.trim());
            if let Some(found) = read_admin_key(&target) {
                return Some(found);
            }
        }
    }
    discover_local_admin_key()
}

pub fn discover_local_admin_key() -> Option<(String, PathBuf)> {
    // Relative candidates resolve against the current directory (the "./"
    // prefix is dropped so the announced path reads cleanly). `./data` is the
    // quickstart; `./.xerj-data` is what `xerj search` / `xerj def` tell you
    // to start; `./xerj-data` is the same name without the dot.
    const CANDIDATES: &[&str] = &[
        "data/admin.key",
        ".xerj-data/admin.key",
        "xerj-data/admin.key",
        "/var/lib/xerj/admin.key",
    ];
    let cwd = std::env::current_dir().ok();
    let mut paths: Vec<PathBuf> = CANDIDATES
        .iter()
        .map(|c| {
            let p = PathBuf::from(c);
            match (&cwd, p.is_absolute()) {
                (Some(cwd), false) => cwd.join(p),
                _ => p,
            }
        })
        .collect();
    if let Some(home) = crate::home::home_dir() {
        paths.push(home.join(".xerj/brain/admin.key"));
        // llms.txt: `xerj --data-dir ~/xerj-data`.
        paths.push(home.join("xerj-data/admin.key"));
        paths.push(home.join(".xerj/admin.key"));
    }
    paths.into_iter().find_map(|p| read_admin_key(&p))
}
