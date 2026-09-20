//! `xerj share <index|folder>` — hand one indexed corpus to someone who has
//! nothing else: a link, a passcode, an expiry.
//!
//! This is the command-line face of `xerj-api::share` (`/_share`). It mints
//! nothing itself: the node creates the share, stores only digests, and hands
//! back the share id and passcode exactly once; this command formats them into
//! something a person can send.
//!
//! Composition, not reimplementation — the same rule `xerj brain` follows:
//! - **credentials** are resolved the way `xerj brain` resolves them:
//!   `--api-key`, then `XERJ_API_KEY`, then `<data-dir>/admin.key`. `/_share`
//!   is superuser-only, so the key has to be the node's admin key;
//! - a **folder** argument resolves to what `xerj brain <folder>` built for it:
//!   the brain named by `xerj_autoindex::derive_brain_name`, and the dataset
//!   indices that brain's meta document lists (`nodes_index`);
//! - `--tunnel` runs the operator's own `cloudflared` as a child process and
//!   reads the public hostname off its log. No tunnel code lives here.
//!
//! ## What the owner is told about the road to the guest
//!
//! The documents never leave this machine as a copy — that sentence is true in
//! every mode. *Who can read the traffic* is not the same in every mode, and
//! the first cut said "the guest's browser reads from this node" under
//! `--tunnel` too, where the browser talks to Cloudflare, which ends TLS and
//! can read the passcode, the guest key and every document opened (review of
//! PR #947). [`Reach`] is how the link gets to this node, and
//! [`render_created`] says what that means at the moment the owner is about to
//! send it — not only in the docs.
//!
//! ## The one refusal
//!
//! A node running with authentication off (`--insecure`, `auth.enabled =
//! false`) treats every request as the superuser. A "scoped, read-only" guest
//! key on such a node restricts nothing — the guest, and anyone else who can
//! reach the port, can already read and delete everything without it. So this
//! command refuses, in words, rather than print a link that promises a
//! boundary the node is not enforcing. The server refuses the same request
//! with `409`; the client-side probe exists so the refusal happens *before* a
//! tunnel is opened.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use xerj_autoindex::detect;
use xerj_autoindex::esclient::Es;

/// Where the guest page lives on the node. Must match the `url_path` the
/// server returns (`xerj-api::share::create_share`); the server's value wins
/// when it sends one.
const GUEST_PAGE_PATH: &str = "/_xerj-console/share";
/// Where a node listens unless told otherwise — the same default `xerj brain`
/// uses.
const DEFAULT_URL: &str = "http://localhost:9200";
/// How long `cloudflared` gets to print its hostname.
const TUNNEL_URL_TIMEOUT: Duration = Duration::from_secs(45);
/// After the hostname is known, how long to wait for the first edge
/// connection before printing the link anyway.
const TUNNEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

pub fn run_cli() -> i32 {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let cfg = match parse(args, std::env::var("XERJ_API_KEY").ok()) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            print_help();
            return 0;
        }
        Err(e) => {
            eprintln!("error: {e}\n");
            print_help();
            return 2;
        }
    };
    match run(cfg) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

pub fn print_help() {
    println!("{}", help_text(xerj_common::feedback::enabled()));
}

pub fn help_text(feedback: bool) -> String {
    format!(
        "xerj share — give someone read-only search over one indexed folder: a link, a\n\
         passcode, an expiry\n\
         \n\
         The guest opens the link, types the passcode, and gets a reading room over\n\
         that one index: search, highlighted snippets, a document view. Read-only.\n\
         The documents stay on this machine: XERJ copies them nowhere, and this node\n\
         answers the guest's browser. With --tunnel that traffic passes through\n\
         Cloudflare, which can read it, and what Cloudflare keeps of what it carries\n\
         is between you and Cloudflare (see --tunnel). Revoke any time.\n\
         \n\
         {}\
         USAGE:\n\
             xerj share <index|folder> [OPTIONS]   create a share\n\
             xerj share --list                     every share on this node\n\
             xerj share --revoke <handle>          end one (kills its guest keys)\n\
         \n\
         <index|folder>: an index name (`ax-notes`, or `a,b`), or the folder you gave\n\
         `xerj brain` — that resolves to the brain and indices it built.\n\
         \n\
         OPTIONS:\n\
             --expires <D>      lifetime: 30m, 24h, 7d … (default {}; longest 30d)\n\
             --max-claims <N>   how many times the link can be opened (default {},\n\
                                most {}). Each open mints its own guest key. One\n\
                                open is one browser tab: a guest who closes the tab\n\
                                needs another open to come back — give a few to\n\
                                someone who will read over several days\n\
             --passcode <CODE>  choose the passcode ({}–{} chars). Default: a generated\n\
                                xxxx-xxxx. A passcode typed here lands in your shell\n\
                                history; the generated one does not\n\
             --label <TEXT>     what the guest sees as the title (e.g. \"for Dana\")\n\
             --brain <NAME>     also share this brain's links (a folder argument sets\n\
                                this for you)\n\
             --index            treat the argument as an index name even when a folder\n\
                                of that name exists here\n\
             --tunnel           open a temporary public address with `cloudflared`, print\n\
                                the full guest link, run until Ctrl-C, then close the\n\
                                tunnel and revoke the share. Without `cloudflared`\n\
                                installed you get install steps and the local link.\n\
                                The tunnel is Cloudflare's: it ends TLS there, so\n\
                                Cloudflare can read what passes through — the\n\
                                passcode, the guest's key, the searches and every\n\
                                document the guest opens.\n\
                                If that is not acceptable, use --public-url with\n\
                                your own certificate\n\
             --keep             with --tunnel: leave the share active after Ctrl-C\n\
             --public-url <U>   the https:// address this node is reachable at (your own\n\
                                hostname or reverse proxy); the printed link uses it.\n\
                                http:// is accepted with a warning: the passcode and\n\
                                the guest's key would cross the network unencrypted\n\
             --url <U>          the node, scheme included (default http://localhost:9200)\n\
             --data-dir <PATH>  where the node's admin.key is (default ~/.xerj/brain)\n\
             --api-key <K>      the node's ADMIN key (or env XERJ_API_KEY). Shares can\n\
                                only be managed with the admin key\n\
             --json             machine-readable output\n\
             --disable-feedback do not print the feedback invitation above (env\n\
                                XERJ_DISABLE_FEEDBACK=true)\n\
             --help, -h         this help\n\
         \n\
         REFUSES against a node running with --insecure / auth off: every request is\n\
         already the superuser there, so a read-only guest key would restrict nothing.\n\
         \n\
         EXIT CODES: 0 ok; 1 refused or failed; 2 usage; 130 interrupted before the tunnel\n\
         was up (nothing was shared)\n",
        xerj_common::feedback::block(feedback),
        xerj_api::share::DEFAULT_EXPIRES_IN,
        xerj_api::share::DEFAULT_MAX_CLAIMS,
        xerj_api::share::MAX_MAX_CLAIMS,
        xerj_api::share::MIN_PASSCODE_CHARS,
        xerj_api::share::MAX_PASSCODE_CHARS,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Arguments
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Common {
    pub url: String,
    pub data_dir: Option<PathBuf>,
    pub api_key: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCfg {
    pub target: String,
    pub force_index: bool,
    pub brain: Option<String>,
    pub expires: Option<String>,
    pub max_claims: Option<u32>,
    pub passcode: Option<String>,
    pub label: Option<String>,
    pub tunnel: bool,
    pub keep: bool,
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create(CreateCfg),
    List,
    Revoke(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareCfg {
    pub common: Common,
    pub action: Action,
}

/// `env_key` is the ambient `XERJ_API_KEY`, passed in so tests do not depend
/// on (or race over) the process environment.
fn parse(args: Vec<String>, env_key: Option<String>) -> Result<Option<ShareCfg>, String> {
    let mut it = args.into_iter();
    let mut target: Option<String> = None;
    let mut list = false;
    let mut revoke: Option<String> = None;
    let mut url = DEFAULT_URL.to_string();
    let mut data_dir: Option<PathBuf> = None;
    let mut api_key = env_key.filter(|s| !s.is_empty());
    let mut json = false;
    let mut create = CreateCfg {
        target: String::new(),
        force_index: false,
        brain: None,
        expires: None,
        max_claims: None,
        passcode: None,
        label: None,
        tunnel: false,
        keep: false,
        public_url: None,
    };
    // Which create-only flags were seen, so `--list --expires 1h` is an error
    // rather than a silently ignored flag.
    let mut create_flags: Vec<&'static str> = Vec::new();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--revoke" => revoke = Some(it.next().ok_or("--revoke needs a share handle")?),
            "--expires" => {
                let spec = it.next().ok_or("--expires needs a value like 24h")?;
                let ms = xerj_api::share::parse_expires_in(&spec).ok_or_else(|| {
                    format!("--expires {spec}: use a number and a unit — 30m, 24h, 7d")
                })?;
                if !(xerj_api::share::MIN_EXPIRES_MS..=xerj_api::share::MAX_EXPIRES_MS)
                    .contains(&ms)
                {
                    return Err(format!("--expires {spec}: must be between 1s and 30d"));
                }
                create.expires = Some(spec);
                create_flags.push("--expires");
            }
            "--max-claims" => {
                let raw = it.next().ok_or("--max-claims needs a number")?;
                let n: u32 = raw
                    .parse()
                    .map_err(|_| format!("--max-claims {raw}: not a number"))?;
                if !(1..=xerj_api::share::MAX_MAX_CLAIMS).contains(&n) {
                    return Err(format!(
                        "--max-claims {n}: must be from 1 to {}",
                        xerj_api::share::MAX_MAX_CLAIMS
                    ));
                }
                create.max_claims = Some(n);
                create_flags.push("--max-claims");
            }
            "--passcode" => {
                let code = it.next().ok_or("--passcode needs a value")?;
                let n = code.trim().chars().count();
                if !(xerj_api::share::MIN_PASSCODE_CHARS..=xerj_api::share::MAX_PASSCODE_CHARS)
                    .contains(&n)
                {
                    return Err(format!(
                        "--passcode must be {}–{} characters",
                        xerj_api::share::MIN_PASSCODE_CHARS,
                        xerj_api::share::MAX_PASSCODE_CHARS
                    ));
                }
                create.passcode = Some(code.trim().to_string());
                create_flags.push("--passcode");
            }
            "--label" => {
                let label = it.next().ok_or("--label needs a value")?;
                if label.chars().count() > xerj_api::share::MAX_LABEL_CHARS {
                    return Err(format!(
                        "--label must be at most {} characters",
                        xerj_api::share::MAX_LABEL_CHARS
                    ));
                }
                create.label = Some(label);
                create_flags.push("--label");
            }
            "--brain" => {
                let name = it.next().ok_or("--brain needs a value")?;
                detect::validate_brain(&name)
                    .map_err(|reason| format!("--brain {name}: {reason}"))?;
                create.brain = Some(name);
                create_flags.push("--brain");
            }
            "--index" => {
                create.force_index = true;
                create_flags.push("--index");
            }
            "--tunnel" => {
                create.tunnel = true;
                create_flags.push("--tunnel");
            }
            "--keep" => {
                create.keep = true;
                create_flags.push("--keep");
            }
            "--public-url" => {
                let u = it.next().ok_or("--public-url needs a value")?;
                if !(u.starts_with("https://") || u.starts_with("http://")) {
                    return Err(format!(
                        "--public-url {u}: must start with https:// (or http://)"
                    ));
                }
                create.public_url = Some(u.trim_end_matches('/').to_string());
                create_flags.push("--public-url");
            }
            "--url" => {
                // Without a scheme the HTTP client fails before it connects,
                // and that used to be reported as "no xerj node answers" while
                // the node was up (review of PR #947).
                let u = it.next().ok_or("--url needs a value")?;
                if !(u.starts_with("http://") || u.starts_with("https://")) {
                    return Err(format!(
                        "--url {u}: include the scheme — for example http://{u}"
                    ));
                }
                url = u;
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(it.next().ok_or("--data-dir needs a value")?))
            }
            "--api-key" => api_key = Some(it.next().ok_or("--api-key needs a value")?),
            "--json" => json = true,
            // Read out of band by `xerj_common::feedback`; accepted here so it
            // is not "unknown".
            xerj_common::feedback::DISABLE_FLAG => {}
            "--help" | "-h" => return Ok(None),
            other if !other.starts_with('-') && target.is_none() => {
                target = Some(other.to_string())
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let common = Common {
        url: url.trim_end_matches('/').to_string(),
        data_dir,
        api_key,
        json,
    };

    let picked = [list, revoke.is_some(), target.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if picked == 0 {
        return Ok(None); // nothing asked for → help, like `xerj brain`
    }
    if picked > 1 {
        return Err("choose one: a target to share, --list, or --revoke <handle>".into());
    }
    if (list || revoke.is_some()) && !create_flags.is_empty() {
        return Err(format!(
            "{} only applies when creating a share",
            create_flags.join(", ")
        ));
    }
    if create.keep && !create.tunnel {
        return Err("--keep only applies with --tunnel".into());
    }
    if create.tunnel && create.public_url.is_some() {
        return Err(
            "--tunnel and --public-url are two answers to the same question; pick one".into(),
        );
    }

    let action = if list {
        Action::List
    } else if let Some(handle) = revoke {
        let handle = handle.trim().to_string();
        // Exactly the 12-hex handle. The 32-hex share id from a link would
        // work on the server, but it would travel in the DELETE's request
        // path, and the docs say the CLI never puts a share id in a URL.
        let hex = handle.chars().all(|c| c.is_ascii_hexdigit());
        if handle.len() == 32 && hex {
            // Not echoed: the error line may end up in a terminal log.
            return Err(
                "--revoke <share id>: that is the share id from the link, which is \
                 never sent in a URL — revoke by the handle `xerj share --list` prints"
                    .into(),
            );
        }
        if handle.len() != 12 || !hex {
            return Err(format!(
                "--revoke {handle}: a share handle is the 12 hex characters `xerj share \
                 --list` prints"
            ));
        }
        Action::Revoke(handle)
    } else {
        create.target = target.unwrap_or_default();
        Action::Create(create)
    };
    Ok(Some(ShareCfg { common, action }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure helpers (unit-tested)
// ─────────────────────────────────────────────────────────────────────────────

/// What the positional argument names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// One or more concrete index names.
    Indices(Vec<String>),
    /// A folder `xerj brain` indexed.
    Folder(PathBuf),
}

/// Decide what `arg` names. `is_dir` is injected so the rule is testable
/// without a filesystem.
///
/// An existing directory wins unless `--index` was passed: the command is
/// documented as "share the folder you gave `xerj brain`", and printing what
/// the folder resolved to (see [`run`]) makes a wrong guess visible before
/// anything is sent to anyone. Something that *looks* like a path but is not a
/// directory is an error, never an index name — `xerj share ./nots` must not
/// turn into a request for an index called `./nots`.
fn classify_target(
    arg: &str,
    force_index: bool,
    is_dir: impl Fn(&Path) -> bool,
) -> Result<Target, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("name an index or a folder to share".into());
    }
    let pathish =
        arg.contains('/') || arg.contains('\\') || arg.starts_with('.') || arg.starts_with('~');
    if force_index {
        if pathish {
            return Err(format!("--index {arg}: that is a path, not an index name"));
        }
    } else {
        if is_dir(Path::new(arg)) {
            return Ok(Target::Folder(PathBuf::from(arg)));
        }
        if pathish {
            return Err(format!(
                "{arg} is not a folder here. Pass the folder you gave `xerj brain`, or an \
                 index name (see `xerj autoindex map`)"
            ));
        }
    }
    let indices: Vec<String> = arg
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if indices.is_empty() {
        return Err("name an index or a folder to share".into());
    }
    Ok(Target::Indices(indices))
}

/// How the printed link reaches this node. It decides what the owner is told
/// about who else can read the traffic — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The node's own URL, and the host is loopback: only this machine.
    Loopback,
    /// The node's own URL, and the host is NOT loopback — a LAN or public
    /// address. The first cut called this "only this machine can reach" too,
    /// because it looked at the flags and not at the URL.
    Direct,
    /// `--public-url`: the owner's hostname or reverse proxy.
    PublicUrl,
    /// `--tunnel`: a Cloudflare quick tunnel, which terminates TLS.
    CloudflareTunnel,
}

impl Reach {
    /// The `via` field of `--json` output.
    fn as_str(self) -> &'static str {
        match self {
            Reach::Loopback => "loopback",
            Reach::Direct => "direct",
            Reach::PublicUrl => "public_url",
            Reach::CloudflareTunnel => "cloudflare_tunnel",
        }
    }
}

/// What a quick tunnel means for the traffic, in the words the owner reads
/// before sending the link. One constant: the human banner, `--json`
/// (`transit_notice`) and the tests all use it.
pub const CLOUDFLARE_TRANSIT_NOTICE: &str =
    "this link goes through Cloudflare. A quick tunnel ends TLS at Cloudflare, so Cloudflare \
     can read everything that passes through it: the passcode, the guest's key, the searches \
     and every document the guest opens. If that is not acceptable, \
     publish the node under your own certificate and use --public-url instead";

/// The host part of an `http(s)://host[:port]/…` URL, brackets kept.
fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = match hostport.strip_prefix('[') {
        // [::1]:9200
        Some(v6) => v6.split(']').next().unwrap_or(v6),
        None => hostport.rsplit_once(':').map_or(hostport, |(h, _)| h),
    };
    (!host.is_empty()).then_some(host)
}

/// Does `url` name this machine and nothing else?
fn is_loopback_url(url: &str) -> bool {
    match url_host(url) {
        Some("localhost") => true,
        Some(host) => host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// How the link for this create reaches the node.
fn reach_of(tunnel_up: bool, public_url: Option<&str>, node_url: &str) -> Reach {
    if tunnel_up {
        Reach::CloudflareTunnel
    } else if public_url.is_some() {
        Reach::PublicUrl
    } else if is_loopback_url(node_url) {
        Reach::Loopback
    } else {
        Reach::Direct
    }
}

/// The link a guest opens. The share id rides in the **fragment**: a browser
/// never sends a fragment to the server, so the page request carries no id and
/// neither does any `Referer`. The page's own script then sends the id in the
/// **body** of the claim `POST` (`/_share/claim`) — never in a path, which is
/// what an access log, a reverse proxy and a tunnel's edge would record.
pub fn guest_link(base_url: &str, url_path: Option<&str>, share_id: &str) -> String {
    let base = base_url.trim_end_matches('/');
    match url_path {
        // The node's own path wins — but only a console path whose fragment is
        // exactly this share id and that carries no query string. Anything
        // else falls back to the path this build knows.
        Some(p)
            if p.starts_with("/_xerj-console/")
                && !p.contains('?')
                && p.split_once('#').map(|(_, frag)| frag) == Some(share_id) =>
        {
            format!("{base}{p}")
        }
        _ => format!("{base}{GUEST_PAGE_PATH}#{share_id}"),
    }
}

/// Pull the public hostname out of one line of `cloudflared` output. The
/// banner wraps it in a box (`|  https://x-y-z.trycloudflare.com   |`); the
/// URL itself has no whitespace. Only a `trycloudflare.com` host counts —
/// the log also mentions `https://www.cloudflare.com/website-terms/` and
/// `https://developers.cloudflare.com/…`, which are not tunnels.
fn parse_tunnel_url(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some(at) = rest.find("https://") {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '|' || c == '"' || c == '\'')
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['/', '.', ',']);
        let host = url.trim_start_matches("https://");
        let host = host.split('/').next().unwrap_or(host);
        if host.ends_with(".trycloudflare.com")
            && host.len() > ".trycloudflare.com".len()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            return Some(format!("https://{host}"));
        }
        rest = &tail[8..];
    }
    None
}

/// Has `cloudflared` told us an edge connection is up?
fn tunnel_line_means_connected(line: &str) -> bool {
    line.contains("Registered tunnel connection") || line.contains("Connection registered")
}

/// (host, port) of a plain-http node URL — the only kind `--tunnel` fronts.
fn local_http_origin(url: &str) -> Result<(String, u16), String> {
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(format!(
            "--tunnel fronts a plain-http node on this machine (the default); {url} is not \
             one. For a TLS node, run your own `cloudflared tunnel --url {url} \
             --no-tls-verify` and pass its address as --public-url"
        ));
    };
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| format!("invalid port in --url {url}"))?,
        ),
        None => (hostport.to_string(), 9200),
    };
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]" | "::1") {
        return Err(format!(
            "--tunnel exposes a node on THIS machine; {url} is somewhere else. Run \
             `xerj share --tunnel` on the machine that holds the data"
        ));
    }
    Ok((host, port))
}

/// Exact install steps, per platform. Printed instead of failing when
/// `cloudflared` is absent.
fn cloudflared_install_help() -> &'static str {
    if cfg!(target_os = "macos") {
        "  install it:  brew install cloudflared"
    } else if cfg!(target_os = "windows") {
        "  install it:  winget install --id Cloudflare.cloudflared"
    } else {
        "  install it (no root needed):\n\
         \x20   mkdir -p ~/.local/bin && curl -fsSL -o ~/.local/bin/cloudflared \\\n\
         \x20     https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 \\\n\
         \x20     && chmod +x ~/.local/bin/cloudflared\n\
         \x20 (arm64: replace amd64 with arm64; Debian/Ubuntu/RHEL packages:\n\
         \x20  https://pkg.cloudflare.com)"
    }
}

/// Find `cloudflared`: `XERJ_CLOUDFLARED`, then `PATH`, then `~/.local/bin`
/// (where the install steps above put it, and which a non-login shell often
/// does not have on `PATH`).
fn find_cloudflared() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XERJ_CLOUDFLARED").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local").join("bin"));
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".xerj").join("brain")
}

/// `--api-key` / `XERJ_API_KEY` first, then `<data-dir>/admin.key` — the same
/// order `xerj brain` uses.
fn resolve_api_key(common: &Common) -> (Option<String>, PathBuf) {
    let key_path = common
        .data_dir
        .clone()
        .unwrap_or_else(default_data_dir)
        .join("admin.key");
    let key = common.api_key.clone().or_else(|| {
        std::fs::read_to_string(&key_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });
    (key, key_path)
}

/// The words for the refusal. One place, so the CLI probe and the server's
/// `409` say the same thing.
fn open_mode_refusal(url: &str) -> String {
    format!(
        "the node at {url} is running with authentication OFF (--insecure, or \
         auth.enabled = false).\n\
         \x20 A share is a read-only guest key. On a node with auth off every request is \
         already\n\
         \x20 the superuser, so that key would restrict nothing: the guest — and anyone \
         else who\n\
         \x20 can reach the port — could read, change and delete every index without it.\n\
         \x20 Restart the node without --insecure (auth is on by default; the admin key is \
         written\n\
         \x20 to <data_dir>/admin.key), then run this again."
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Running
// ─────────────────────────────────────────────────────────────────────────────

fn run(cfg: ShareCfg) -> Result<i32> {
    let common = &cfg.common;

    // 1. Is anything there, and is it enforcing anything? An unauthenticated
    //    `GET /` answers 200 only on an open node.
    let anon = Es::new(&common.url, None)?;
    match anon.get_status("/") {
        Ok(200) => bail!("{}", open_mode_refusal(&common.url)),
        Ok(401) | Ok(403) => {}
        Ok(status) => bail!(
            "something answers at {} but it does not look like a xerj node (GET / → HTTP \
             {status}). Point --url at your node",
            common.url
        ),
        Err(_) => bail!(
            "no xerj node answers at {}. Start one first — `xerj brain <folder>` boots a \
             node and indexes the folder — or pass --url",
            common.url
        ),
    }

    // 2. The admin key.
    let (api_key, key_path) = resolve_api_key(common);
    let Some(api_key) = api_key else {
        bail!(
            "no admin key. Shares are managed with the node's admin key: pass --api-key, set \
             XERJ_API_KEY, or point --data-dir at the node's data directory (looked for {})",
            key_path.display()
        );
    };
    let es = Es::new(&common.url, Some(api_key))?;

    match &cfg.action {
        Action::List => list(&es, common),
        Action::Revoke(handle) => revoke(&es, common, handle),
        Action::Create(create) => create_share(&es, common, create),
    }
}

/// Turn a non-2xx from `/_share` into words.
fn explain(status: u16, body: &Value, common: &Common) -> anyhow::Error {
    let reason = body
        .pointer("/error/reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    match status {
        401 => anyhow!(
            "the node at {} rejected the key. Shares need the node's ADMIN key — the one in \
             <data_dir>/admin.key",
            common.url
        ),
        403 => anyhow!(
            "that key is not the admin key. A scoped or minted key cannot create, list or \
             revoke shares — otherwise it could widen its own reach. Use the key in \
             <data_dir>/admin.key"
        ),
        409 => anyhow!("{}", open_mode_refusal(&common.url)),
        404 if !reason.is_empty() => anyhow!("{reason}"),
        404 | 405 => anyhow!(
            "the node at {} has no share links (`/_share` is not there) — it is an older \
             xerj. Upgrade the node, then run this again",
            common.url
        ),
        _ if !reason.is_empty() => anyhow!("{reason} (HTTP {status})"),
        _ => anyhow!(
            "unexpected HTTP {status} from {}/_share: {body}",
            common.url
        ),
    }
}

fn list(es: &Es, common: &Common) -> Result<i32> {
    let (status, body) = es.request_json("GET", "/_share", None)?;
    if status != 200 {
        return Err(explain(status, &body, common));
    }
    if common.json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(0);
    }
    let shares = body
        .get("shares")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if shares.is_empty() {
        println!("no shares on this node. Create one: xerj share <index|folder>");
        return Ok(0);
    }
    println!(
        "{:<14} {:<10} {:<8} {:<22} {:<28} LABEL",
        "HANDLE", "STATUS", "CLAIMS", "EXPIRES", "INDEX"
    );
    for s in &shares {
        let g = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("-").to_string();
        let claims = format!(
            "{}/{}",
            s.get("claims").and_then(Value::as_u64).unwrap_or(0),
            s.get("max_claims").and_then(Value::as_u64).unwrap_or(0)
        );
        println!(
            "{:<14} {:<10} {:<8} {:<22} {:<28} {}",
            g("handle"),
            g("status"),
            claims,
            g("expires_at"),
            g("index"),
            g("label")
        );
    }
    println!(
        "\nrevoke one: xerj share --revoke <HANDLE>{}",
        connection_args(common)
    );
    Ok(0)
}

fn revoke(es: &Es, common: &Common, handle: &str) -> Result<i32> {
    let (status, body) = es.request_json("DELETE", &format!("/_share/{handle}"), None)?;
    if status == 404 {
        bail!("no share with handle {handle} on this node (see `xerj share --list`)");
    }
    if status != 200 {
        return Err(explain(status, &body, common));
    }
    if common.json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(0);
    }
    let n = body
        .get("keys_invalidated")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!(
        "✓ share {handle} revoked — {n} guest key{} invalidated. Anyone who had opened it \
         loses access on their next request.",
        if n == 1 { "" } else { "s" }
    );
    Ok(0)
}

/// What a folder argument resolved to.
struct Resolved {
    indices: Vec<String>,
    brain: Option<String>,
    /// A line for the owner: how the argument became these indices.
    how: Option<String>,
}

/// What the brain-meta lookup for a folder argument came back with.
#[derive(Debug, PartialEq)]
enum BrainMeta {
    Found(Value),
    /// The node answered and there is no such brain.
    NotIndexed,
    /// The node did not answer the question — most often `401`/`403`, a wrong
    /// or non-admin key.
    Refused,
}

/// Read a `GET /{edges}/_doc/__brain_meta__` answer.
///
/// This went through `Es::get_doc`, which turns *every* body without
/// `found: true` into "no document" — including a `401`. So a wrong key with a
/// folder argument was reported as "<folder> has not been indexed … run `xerj
/// brain` first", sending the owner to re-index a folder that was indexed all
/// along, while the same key with an index argument got the right sentence
/// (review of PR #947). Only a `404`, or a `200` that says `found: false`,
/// means the brain is not there.
fn brain_meta(status: u16, body: &Value) -> BrainMeta {
    match status {
        200 if body.get("found").and_then(Value::as_bool) == Some(true) => body
            .get("_source")
            .cloned()
            .map_or(BrainMeta::NotIndexed, BrainMeta::Found),
        200 | 404 => BrainMeta::NotIndexed,
        _ => BrainMeta::Refused,
    }
}

fn resolve_target(es: &Es, common: &Common, create: &CreateCfg) -> Result<Resolved> {
    let target = classify_target(&create.target, create.force_index, |p| p.is_dir())
        .map_err(|e| anyhow!(e))?;
    match target {
        Target::Indices(indices) => Ok(Resolved {
            indices,
            brain: create.brain.clone(),
            how: None,
        }),
        Target::Folder(folder) => {
            let brain = match &create.brain {
                Some(b) => b.clone(),
                None => {
                    let derived = xerj_autoindex::derive_brain_name(&folder);
                    detect::validate_brain(&derived).map_err(|reason| {
                        anyhow!(
                            "folder name '{derived}' is not usable as a brain name ({reason}) — \
                             pass the one you used with --brain <name>"
                        )
                    })?;
                    derived
                }
            };
            let edges_index = detect::edges_index_name(&brain);
            let (status, body) = es
                .request_json(
                    "GET",
                    &format!("/{edges_index}/_doc/{}", detect::BRAIN_META_ID),
                    None,
                )
                .with_context(|| format!("look up brain '{brain}'"))?;
            let meta = match brain_meta(status, &body) {
                BrainMeta::Found(meta) => meta,
                BrainMeta::NotIndexed => bail!(
                    "{} has not been indexed on this node: there is no brain named '{brain}'. \
                     Run `xerj brain {}` first, or pass an index name",
                    folder.display(),
                    folder.display()
                ),
                BrainMeta::Refused => return Err(explain(status, &body, common)),
            };
            let indices: Vec<String> = meta
                .get("nodes_index")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            if indices.is_empty() {
                bail!(
                    "brain '{brain}' does not record which indices hold its documents (it \
                     predates that field). Share by index name instead — `xerj autoindex \
                     map` lists them"
                );
            }
            let how = format!(
                "{} → brain '{brain}' → {}",
                folder.display(),
                indices.join(", ")
            );
            Ok(Resolved {
                indices,
                brain: Some(brain),
                how: Some(how),
            })
        }
    }
}

fn create_share(es: &Es, common: &Common, create: &CreateCfg) -> Result<i32> {
    let resolved = resolve_target(es, common, create)?;
    if let Some(how) = &resolved.how {
        eprintln!("sharing {how}");
    }

    // The tunnel comes up BEFORE the share exists, so a link is never printed
    // for an address that is not there, and a tunnel that cannot start costs
    // nothing but a message.
    let mut tunnel: Option<Tunnel> = None;
    let mut tunnel_note: Option<String> = None;
    // Listening before the tunnel exists — see `Interrupts`.
    let interrupts = create.tunnel.then(Interrupts::listen);
    if create.tunnel {
        let (_, port) = local_http_origin(&common.url).map_err(|e| anyhow!(e))?;
        match find_cloudflared() {
            None => {
                tunnel_note = Some(format!(
                    "--tunnel needs `cloudflared`, which is not installed (looked on PATH and \
                     in ~/.local/bin).\n{}\n  then run this command again. Until then the \
                     link above only works on this machine.",
                    cloudflared_install_help()
                ));
            }
            Some(bin) => {
                eprintln!("opening a tunnel with {} …", bin.display());
                let opened = Tunnel::open(
                    &bin,
                    port,
                    interrupts.as_ref().expect("listening when --tunnel is set"),
                );
                match opened {
                    Ok(t) => tunnel = Some(t),
                    Err(OpenError::Interrupted(why)) => {
                        eprintln!("{why} before the tunnel was up — nothing was shared.");
                        return Ok(130);
                    }
                    Err(OpenError::Failed(e)) => {
                        tunnel_note = Some(format!(
                            "the tunnel did not come up: {e:#}\n  the link above only works on \
                             this machine."
                        ))
                    }
                }
            }
        }
    }

    let mut body = json!({ "index": resolved.indices });
    if let Some(b) = &resolved.brain {
        body["brain"] = json!(b);
    }
    if let Some(e) = &create.expires {
        body["expires_in"] = json!(e);
    }
    if let Some(n) = create.max_claims {
        body["max_claims"] = json!(n);
    }
    if let Some(p) = &create.passcode {
        body["passcode"] = json!(p);
    }
    if let Some(l) = &create.label {
        body["label"] = json!(l);
    }
    let (status, resp) = es.request_json("POST", "/_share", Some(&body))?;
    if status != 200 {
        return Err(explain(status, &resp, common));
    }
    let share_id = resp
        .get("share_id")
        .and_then(Value::as_str)
        .context("the node's response carries no share_id")?;
    let handle = resp.get("handle").and_then(Value::as_str).unwrap_or("");
    let url_path = resp.get("url_path").and_then(Value::as_str);

    let base = tunnel
        .as_ref()
        .map(|t| t.public_url.clone())
        .or_else(|| create.public_url.clone())
        .unwrap_or_else(|| common.url.clone());
    let link = guest_link(&base, url_path, share_id);
    let reach = reach_of(tunnel.is_some(), create.public_url.as_deref(), &common.url);

    if common.json {
        let mut out = resp.clone();
        out["link"] = json!(link);
        out["local_only"] = json!(reach == Reach::Loopback);
        out["via"] = json!(reach.as_str());
        if reach == Reach::CloudflareTunnel {
            out["transit_notice"] = json!(CLOUDFLARE_TRANSIT_NOTICE);
        }
        out["revoke"] = json!(format!(
            "xerj share --revoke {handle}{}",
            connection_args(common)
        ));
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render_created(&resp, &link, reach, common));
    }
    if let Some(note) = &tunnel_note {
        eprintln!("\n{note}");
    }

    let Some(tunnel) = tunnel else {
        return Ok(0);
    };
    if resp
        .pointer("/claim_limiter/source_address")
        .and_then(Value::as_str)
        == Some("peer")
    {
        eprintln!(
            "\nnote: through a tunnel every guest reaches this node from 127.0.0.1, so the \
             audit log\n      records that address for them. To record each guest's real \
             address, add\n        trusted_proxies = [\"127.0.0.1\", \"::1\"]\n      under \
             [server] in the node's config and restart it. The passcode lockout\n      \
             ({} guesses per share per hour) holds either way.",
            xerj_api::share::SHARE_PER_HOUR
        );
    }
    eprintln!(
        "\ntunnel is up — keep this running while your guest reads. Ctrl-C closes the tunnel{}.",
        if create.keep {
            " (the share stays active: --keep)"
        } else {
            " and revokes the share"
        }
    );
    let why = tunnel.wait(interrupts.as_ref().expect("listening when --tunnel is set"));
    eprintln!("\n{why} — closing the tunnel.");
    if why == CLOUDFLARED_STOPPED {
        // Nobody asked it to stop: its own last words are the only reason.
        let tail = tunnel_log_tail(&tunnel.log);
        if !tail.is_empty() {
            eprintln!("{}", tail.trim_start_matches('\n'));
        }
    }
    drop(tunnel);
    if create.keep {
        eprintln!(
            "the share is still active on this node until it expires. End it: xerj share \
             --revoke {handle}{}",
            connection_args(common)
        );
        return Ok(0);
    }
    match es.request_json("DELETE", &format!("/_share/{handle}"), None) {
        Ok((200, body)) => eprintln!(
            "share {handle} revoked — {} guest key(s) invalidated.",
            body.get("keys_invalidated")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ),
        Ok((status, _)) => eprintln!(
            "could not revoke share {handle} (HTTP {status}). Do it by hand: xerj share \
             --revoke {handle}{}",
            connection_args(common)
        ),
        Err(e) => eprintln!(
            "could not revoke share {handle} ({e:#}). Do it by hand: xerj share --revoke \
             {handle}{}",
            connection_args(common)
        ),
    }
    Ok(0)
}

/// The connection flags a follow-up command has to repeat to reach the same
/// node: `--url` and `--data-dir`, each only when it is not the default. Never
/// `--api-key` — a key is not something to echo back into a terminal
/// scrollback, and `--data-dir` (or `XERJ_API_KEY`) already supplies it.
fn connection_args(common: &Common) -> String {
    let mut out = String::new();
    if common.url != DEFAULT_URL {
        out.push_str(&format!(" --url {}", sh_quote(&common.url)));
    }
    if let Some(dir) = &common.data_dir {
        out.push_str(&format!(
            " --data-dir {}",
            sh_quote(&dir.display().to_string())
        ));
    }
    out
}

/// `arg` as one shell word, for a command the owner is meant to paste.
///
/// `xerj brain "case files"` printed `xerj share /…/case files --url …`, and
/// pasting it failed with `unknown argument: files` (review of PR #947). An
/// argument made only of characters no shell treats specially is left alone,
/// so the common case still reads cleanly; anything else is quoted — single
/// quotes on unix (`'` itself as `'\''`), double quotes on Windows, where
/// `cmd.exe` and PowerShell both accept them around a path.
pub(crate) fn sh_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | ':' | ',' | '+' | '=' | '@' | '%')
        });
    if plain {
        return arg.to_string();
    }
    if cfg!(windows) {
        // A Windows path legitimately holds `\` and `:`; neither needs more
        // than the surrounding quotes.
        let bare_ok = !arg.is_empty()
            && arg.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | ':')
            });
        return if bare_ok {
            arg.to_string()
        } else {
            format!("\"{}\"", arg.replace('"', "\\\""))
        };
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// The human-readable result of a create. A value, so tests can read it.
fn render_created(resp: &Value, link: &str, reach: Reach, common: &Common) -> String {
    let node_url = common.url.as_str();
    let g = |k: &str| resp.get(k).and_then(Value::as_str).unwrap_or("");
    let mut what = format!("index {}", g("index"));
    if !g("brain").is_empty() {
        what.push_str(&format!(" · brain {} (its links)", g("brain")));
    }
    let claims = resp.get("max_claims").and_then(Value::as_u64).unwrap_or(1);
    let mut out = String::new();
    out.push_str(&format!(
        "\n✓ share created{}\n",
        if g("label").is_empty() {
            String::new()
        } else {
            format!(" — \"{}\"", g("label"))
        }
    ));
    out.push_str(&format!("  shares:    {what} — read-only\n"));
    out.push_str(&format!("  link:      {link}\n"));
    out.push_str(&format!(
        "  passcode:  {}     (send it separately from the link)\n",
        g("passcode")
    ));
    out.push_str(&format!(
        "  expires:   {} · can be opened {claims} time{}{}\n",
        g("expires_at"),
        if claims == 1 { "" } else { "s" },
        // sessionStorage is per tab: a closed tab is a spent open. Say so
        // where the number is, not only in the docs.
        if claims == 1 {
            " — one browser tab; --max-claims <N> for a guest who will come back"
        } else {
            ""
        }
    ));
    out.push_str(&format!(
        "  revoke:    xerj share --revoke {}{}\n",
        g("handle"),
        connection_args(common)
    ));
    out.push_str("  the link and passcode are shown once — the node keeps only their hashes.\n");
    let plain_http = link.starts_with("http://");
    match reach {
        Reach::CloudflareTunnel => {
            // NOT "the guest's browser reads from this node": it reads from
            // Cloudflare, which reads from this node.
            out.push_str("  your documents stay on this machine — XERJ copies them nowhere.\n");
            out.push_str(&format!(
                "\n  {}.\n",
                wrap(CLOUDFLARE_TRANSIT_NOTICE, 76, "  ")
            ));
        }
        Reach::Loopback => {
            out.push_str(
                "  your documents stay on this machine; the guest's browser reads from this \
                 node.\n",
            );
            out.push_str(&format!(
                "\n  this link points at {node_url}, which only this machine can reach.\n\
                 \x20 for someone elsewhere: `xerj share … --tunnel` (temporary public address \
                 through\n\
                 \x20 Cloudflare), or --public-url https://<your-hostname> if the node is \
                 already published.\n"
            ));
        }
        Reach::Direct | Reach::PublicUrl => {
            if reach == Reach::PublicUrl {
                // The browser talks to whatever serves that address — the
                // owner's reverse proxy, or a CDN in front of it — not
                // necessarily to this node directly.
                out.push_str(
                    "  your documents stay on this machine; the guest's browser reaches this \
                     node through the\n\
                     \x20 address above, and whoever ends TLS for that address can read what \
                     passes through it.\n",
                );
            } else {
                out.push_str(
                    "  your documents stay on this machine; the guest's browser reads from \
                     this node.\n",
                );
            }
            if plain_http {
                out.push_str(
                    "\n  this link is plain http: the passcode, the guest's key and every \
                     document the guest\n\
                     \x20 opens cross the network unencrypted, readable by anyone on the path. \
                     Use it on a\n\
                     \x20 network you trust, or put the node behind https and pass that address \
                     as --public-url.\n",
                );
            }
        }
    }
    out
}

/// Greedy word wrap at `width`, continuation lines prefixed with `indent`.
fn wrap(text: &str, width: usize, indent: &str) -> String {
    let mut out = String::new();
    let mut line = 0usize;
    for word in text.split_whitespace() {
        let n = word.chars().count();
        if line > 0 && line + 1 + n > width {
            out.push('\n');
            out.push_str(indent);
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += n;
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// The tunnel
// ─────────────────────────────────────────────────────────────────────────────

enum TunnelEvent {
    Url(String),
    Connected,
    Exited,
}

/// Ctrl-C (and, on unix, SIGTERM / SIGHUP) as a channel.
///
/// Installed BEFORE `cloudflared` is spawned. The tunnel runs in its own
/// process group so that it is closed by us, in order; the price is that a
/// signal which kills this command by default action would orphan it. With the
/// listener in place first, there is no window in which that can happen.
///
/// `xerj share` runs inside the binary's tokio runtime (on a blocking thread),
/// so the portable signal futures are available — no handler of our own, and
/// Ctrl-C works on Windows.
struct Interrupts(mpsc::Receiver<&'static str>);

impl Interrupts {
    fn listen() -> Self {
        let (tx, rx) = mpsc::channel::<&'static str>();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let tx_c = tx.clone();
            handle.spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = tx_c.send("Ctrl-C");
                }
            });
            #[cfg(unix)]
            for (kind, name) in [
                (tokio::signal::unix::SignalKind::terminate(), "SIGTERM"),
                (tokio::signal::unix::SignalKind::hangup(), "terminal closed"),
            ] {
                let tx_s = tx.clone();
                handle.spawn(async move {
                    if let Ok(mut sig) = tokio::signal::unix::signal(kind) {
                        if sig.recv().await.is_some() {
                            let _ = tx_s.send(name);
                        }
                    }
                });
            }
        }
        Interrupts(rx)
    }

    fn fired(&self) -> Option<&'static str> {
        self.0.try_recv().ok()
    }
}

struct Tunnel {
    child: Child,
    public_url: String,
    events: mpsc::Receiver<TunnelEvent>,
    log: TunnelLog,
}

/// The last lines `cloudflared` wrote. Its output is not shown while it works
/// — it is a wall of connection chatter — but when it dies on its own those
/// lines are the only explanation there is, and the first cut threw them away:
/// the owner saw "cloudflared stopped" and a revoked share, and nothing else
/// (review of PR #947, a real quick tunnel that dropped after two minutes).
type TunnelLog = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>;
/// How many of them are kept.
const TUNNEL_LOG_LINES: usize = 20;

/// `\n  cloudflared's last lines:\n    …` — or nothing, if it never wrote any.
fn tunnel_log_tail(log: &TunnelLog) -> String {
    let lines = match log.lock() {
        Ok(l) => l,
        Err(poisoned) => poisoned.into_inner(),
    };
    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n  cloudflared's last lines:");
    for line in lines.iter() {
        out.push_str("\n    ");
        out.push_str(line);
    }
    out
}

/// Why a tunnel did not open. The two cases end differently: an interrupt
/// stops the command with nothing shared; a failure falls back to the local
/// link.
enum OpenError {
    Interrupted(&'static str),
    Failed(anyhow::Error),
}

/// How often the wait loops look up from the tunnel's log to check for a
/// signal.
const TICK: Duration = Duration::from_millis(200);
/// What [`Tunnel::wait`] returns when the tunnel went away by itself.
const CLOUDFLARED_STOPPED: &str = "cloudflared stopped";

impl Tunnel {
    /// Spawn `cloudflared tunnel --url http://127.0.0.1:<port>` and wait for
    /// its hostname. `127.0.0.1`, not `localhost`: the node binds IPv4
    /// loopback by default and `localhost` may resolve to `::1` first.
    fn open(bin: &Path, port: u16, interrupts: &Interrupts) -> Result<Self, OpenError> {
        let failed = |msg: String| OpenError::Failed(anyhow!(msg));
        let mut command = Command::new(bin);
        command
            .args(["tunnel", "--no-autoupdate", "--url"])
            .arg(format!("http://127.0.0.1:{port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group: a terminal Ctrl-C goes to the foreground
        // group, and the tunnel must be closed by us, in order — after the
        // wait loop has noticed — not race us to the exit.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        // Belt and braces on Linux: if this command dies in a way no handler
        // sees (SIGKILL, a panic=abort), the kernel terminates the tunnel.
        // The "parent" here is the spawning *thread*, which is the thread that
        // runs this whole command, so it lives exactly as long as we do.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: `prctl` is async-signal-safe and touches no memory the
            // parent shares; nothing else runs between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| failed(format!("start {}: {e}", bin.display())))?;

        let (tx, rx) = mpsc::channel::<TunnelEvent>();
        let log = TunnelLog::default();
        let mut readers = Vec::new();
        // cloudflared logs to stderr; read stdout too so neither pipe can fill
        // and stall it.
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_reader(err, tx.clone(), log.clone()));
        }
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_reader(out, tx.clone(), log.clone()));
        }
        // When both pipes close, the process is gone.
        std::thread::spawn(move || {
            for r in readers {
                let _ = r.join();
            }
            let _ = tx.send(TunnelEvent::Exited);
        });

        // From here on `tunnel` owns the child: every early return below drops
        // it, and `Drop` closes the tunnel.
        let mut tunnel = Tunnel {
            child,
            public_url: String::new(),
            events: rx,
            log,
        };
        let deadline = std::time::Instant::now() + TUNNEL_URL_TIMEOUT;
        loop {
            if let Some(why) = interrupts.fired() {
                return Err(OpenError::Interrupted(why));
            }
            match tunnel.events.recv_timeout(TICK) {
                Ok(TunnelEvent::Url(u)) => {
                    tunnel.public_url = u;
                    break;
                }
                Ok(TunnelEvent::Connected) => {}
                Ok(TunnelEvent::Exited) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(failed(format!(
                        "cloudflared exited before printing an address{}",
                        tunnel_log_tail(&tunnel.log)
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if std::time::Instant::now() > deadline {
                        return Err(failed(format!(
                            "cloudflared printed no address within {}s (is this machine online?)",
                            TUNNEL_URL_TIMEOUT.as_secs()
                        )));
                    }
                }
            }
        }
        // The hostname is printed before the first edge connection registers;
        // a link opened in that gap gets Cloudflare's 530. Wait for the
        // connection, but not forever — the link is still the right link.
        let deadline = std::time::Instant::now() + TUNNEL_CONNECT_TIMEOUT;
        loop {
            if let Some(why) = interrupts.fired() {
                return Err(OpenError::Interrupted(why));
            }
            match tunnel.events.recv_timeout(TICK) {
                Ok(TunnelEvent::Connected) => break,
                Ok(TunnelEvent::Url(_)) => {}
                Ok(TunnelEvent::Exited) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(failed(format!(
                        "cloudflared exited while connecting{}",
                        tunnel_log_tail(&tunnel.log)
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if std::time::Instant::now() > deadline {
                        eprintln!(
                            "cloudflared has an address but has not confirmed a connection \
                             yet; the link may take a few seconds to start answering."
                        );
                        break;
                    }
                }
            }
        }
        Ok(tunnel)
    }

    /// Block until a signal arrives or `cloudflared` goes away. Returns why.
    fn wait(&self, interrupts: &Interrupts) -> &'static str {
        loop {
            if let Some(why) = interrupts.fired() {
                return why;
            }
            match self.events.recv_timeout(TICK) {
                Ok(TunnelEvent::Exited) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return CLOUDFLARED_STOPPED
                }
                _ => {}
            }
        }
    }
}

impl Drop for Tunnel {
    /// The tunnel never outlives this command: terminate, give it a moment to
    /// deregister from the edge, then kill.
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: plain `kill(2)` on a pid we spawned and still own.
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            for _ in 0..30 {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_reader<R: std::io::Read + Send + 'static>(
    pipe: R,
    tx: mpsc::Sender<TunnelEvent>,
    log: TunnelLog,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut sent_url = false;
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            remember_tunnel_line(&log, &line);
            if !sent_url {
                if let Some(u) = parse_tunnel_url(&line) {
                    sent_url = true;
                    let _ = tx.send(TunnelEvent::Url(u));
                    continue;
                }
            }
            if tunnel_line_means_connected(&line) {
                let _ = tx.send(TunnelEvent::Connected);
            }
        }
    })
}

/// Keep `line` as one of the last [`TUNNEL_LOG_LINES`]; long lines are cut.
fn remember_tunnel_line(log: &TunnelLog, line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    let kept: String = line.chars().take(300).collect();
    let mut lines = match log.lock() {
        Ok(l) => l,
        Err(poisoned) => poisoned.into_inner(),
    };
    if lines.len() == TUNNEL_LOG_LINES {
        lines.pop_front();
    }
    lines.push_back(kept);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn create_of(cfg: ShareCfg) -> CreateCfg {
        match cfg.action {
            Action::Create(c) => c,
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_is_help_not_an_error() {
        assert_eq!(parse(args(&[]), None), Ok(None));
        assert_eq!(parse(args(&["--help"]), None), Ok(None));
        assert_eq!(parse(args(&["notes", "-h"]), None), Ok(None));
    }

    #[test]
    fn a_bare_target_takes_every_default_from_the_server() {
        let cfg = parse(args(&["ax-notes"]), None).unwrap().unwrap();
        assert_eq!(cfg.common.url, "http://localhost:9200");
        assert_eq!(cfg.common.api_key, None);
        assert!(!cfg.common.json);
        let c = create_of(cfg);
        assert_eq!(c.target, "ax-notes");
        // Unset, not defaulted here: the server owns the defaults (24h, one
        // claim, a generated passcode), so the two cannot drift apart.
        assert_eq!(c.expires, None);
        assert_eq!(c.max_claims, None);
        assert_eq!(c.passcode, None);
        assert!(!c.tunnel && !c.keep && !c.force_index);
    }

    #[test]
    fn every_create_flag_lands_where_it_should() {
        let cfg = parse(
            args(&[
                "~/mail",
                "--expires",
                "7d",
                "--max-claims",
                "3",
                "--passcode",
                "correct horse",
                "--label",
                "for Dana",
                "--brain",
                "mail",
                "--tunnel",
                "--keep",
                "--url",
                "http://localhost:9510/",
                "--api-key",
                "k",
                "--data-dir",
                "/d",
                "--json",
            ]),
            Some("from-env".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            cfg.common.url, "http://localhost:9510",
            "trailing slash trimmed"
        );
        assert_eq!(
            cfg.common.api_key.as_deref(),
            Some("k"),
            "--api-key beats XERJ_API_KEY"
        );
        assert_eq!(cfg.common.data_dir, Some(PathBuf::from("/d")));
        assert!(cfg.common.json);
        let c = create_of(cfg);
        assert_eq!(c.expires.as_deref(), Some("7d"));
        assert_eq!(c.max_claims, Some(3));
        assert_eq!(c.passcode.as_deref(), Some("correct horse"));
        assert_eq!(c.label.as_deref(), Some("for Dana"));
        assert_eq!(c.brain.as_deref(), Some("mail"));
        assert!(c.tunnel && c.keep);
    }

    #[test]
    fn the_environment_key_is_used_when_no_flag_names_one() {
        let cfg = parse(args(&["--list"]), Some("env-key".into()))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.common.api_key.as_deref(), Some("env-key"));
        let cfg = parse(args(&["--list"]), Some(String::new()))
            .unwrap()
            .unwrap();
        assert_eq!(
            cfg.common.api_key, None,
            "an empty XERJ_API_KEY is no key, not an empty key"
        );
    }

    #[test]
    fn values_are_validated_before_anything_is_sent() {
        for bad in [
            vec!["i", "--expires", "24"],      // no unit
            vec!["i", "--expires", "1w"],      // unknown unit
            vec!["i", "--expires", "31d"],     // past the ceiling
            vec!["i", "--expires", "0s"],      // below the floor
            vec!["i", "--max-claims", "0"],    // at least one
            vec!["i", "--max-claims", "51"],   // at most 50
            vec!["i", "--max-claims", "many"], // a number
            vec!["i", "--passcode", "short"],  // 5 chars
            vec!["i", "--brain", "Bad Name"],  // not a brain slug
            vec!["i", "--public-url", "example.com"],
            vec!["i", "--expires"], // missing value
            vec!["i", "--wat"],     // unknown flag
            vec!["i", "j"],         // two targets
        ] {
            assert!(parse(args(&bad), None).is_err(), "{bad:?} must be refused");
        }
        let long_label = "x".repeat(xerj_api::share::MAX_LABEL_CHARS + 1);
        assert!(parse(args(&["i", "--label", &long_label]), None).is_err());
    }

    #[test]
    fn list_revoke_and_create_are_mutually_exclusive() {
        assert_eq!(
            parse(args(&["--list"]), None).unwrap().unwrap().action,
            Action::List
        );
        assert_eq!(
            parse(args(&["--revoke", "1a2b3c4d5e6f"]), None)
                .unwrap()
                .unwrap()
                .action,
            Action::Revoke("1a2b3c4d5e6f".into())
        );
        assert!(parse(args(&["--list", "notes"]), None).is_err());
        assert!(parse(args(&["--list", "--revoke", "abc"]), None).is_err());
        // A create-only flag beside --list would be silently ignored; refuse.
        assert!(parse(args(&["--list", "--expires", "1h"]), None).is_err());
        assert!(parse(args(&["--revoke", "abc", "--tunnel"]), None).is_err());
        // A handle is hex. A pasted link or a path is a mistake worth naming.
        assert!(parse(args(&["--revoke", "not-hex!"]), None).is_err());
        // The share id is refused, and the error does not echo it back.
        let id = "0123456789abcdef0123456789abcdef";
        let err = parse(args(&["--revoke", id]), None).unwrap_err();
        assert!(err.contains("never sent in a URL"), "{err}");
        assert!(!err.contains(id), "{err}");
        assert!(parse(args(&["--revoke", "1a2b3c"]), None).is_err());
        assert!(parse(args(&["--revoke"]), None).is_err());
    }

    #[test]
    fn keep_and_public_url_only_make_sense_in_their_own_mode() {
        assert!(parse(args(&["i", "--keep"]), None).is_err());
        assert!(parse(
            args(&["i", "--tunnel", "--public-url", "https://x.example"]),
            None
        )
        .is_err());
        let c = create_of(
            parse(args(&["i", "--public-url", "https://x.example/"]), None)
                .unwrap()
                .unwrap(),
        );
        assert_eq!(c.public_url.as_deref(), Some("https://x.example"));
    }

    #[test]
    fn an_existing_folder_is_a_folder_and_everything_else_is_an_index() {
        let dirs = |p: &Path| p == Path::new("notes") || p == Path::new("./mail");
        assert_eq!(
            classify_target("notes", false, dirs),
            Ok(Target::Folder(PathBuf::from("notes")))
        );
        assert_eq!(
            classify_target("./mail", false, dirs),
            Ok(Target::Folder(PathBuf::from("./mail")))
        );
        // --index forces the other reading of an ambiguous name.
        assert_eq!(
            classify_target("notes", true, dirs),
            Ok(Target::Indices(vec!["notes".into()]))
        );
        assert_eq!(
            classify_target("ax-mail, ax-pdf", false, dirs),
            Ok(Target::Indices(vec!["ax-mail".into(), "ax-pdf".into()]))
        );
        // A path that is not a folder is never quietly treated as an index.
        assert!(classify_target("./nots", false, dirs).is_err());
        assert!(classify_target("~/gone", false, dirs).is_err());
        assert!(classify_target("/abs/gone", false, dirs).is_err());
        assert!(classify_target("./mail", true, dirs).is_err());
        assert!(classify_target("  ", false, dirs).is_err());
        assert!(classify_target(",", false, dirs).is_err());
    }

    #[test]
    fn the_share_id_rides_in_the_fragment() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            guest_link("http://localhost:9200/", None, id),
            format!("http://localhost:9200/_xerj-console/share#{id}")
        );
        // The server's own path wins when it sends one…
        assert_eq!(
            guest_link(
                "https://a-b.trycloudflare.com",
                Some(&format!("/_xerj-console/share#{id}")),
                id
            ),
            format!("https://a-b.trycloudflare.com/_xerj-console/share#{id}")
        );
        // …but only a path with a fragment: an id in the path or the query
        // would be sent to the server and written to every log on the way.
        for hostile in ["/_xerj-console/share?id=x", "https://evil.example/#x", ""] {
            let link = guest_link("http://h", Some(hostile), id);
            assert_eq!(link, format!("http://h/_xerj-console/share#{id}"));
        }
        let link = guest_link("http://h", None, id);
        let (before, after) = link.split_once('#').unwrap();
        assert!(!before.contains(id), "{link}");
        assert_eq!(after, id);
    }

    #[test]
    fn the_tunnel_hostname_is_read_out_of_cloudflareds_banner() {
        let banner = "2026-09-18T07:41:02Z INF |  https://shiny-rain-forest-42.trycloudflare.com                                 |";
        assert_eq!(
            parse_tunnel_url(banner).as_deref(),
            Some("https://shiny-rain-forest-42.trycloudflare.com")
        );
        // The other https:// lines in the same log are not tunnels.
        for other in [
            "INF Thank you for trying Cloudflare Tunnel. Doing so, without a Cloudflare account, is a quick way to experiment and try it out. However, be aware that these account-less Tunnels have no uptime guarantee, are subject to the Cloudflare Online Services Terms of Use (https://www.cloudflare.com/website-terms/)",
            "INF Requesting new quick Tunnel on trycloudflare.com...",
            "see https://developers.cloudflare.com/cloudflare-one/connections/connect-apps",
            "https://trycloudflare.com",
            "https://.trycloudflare.com",
            "https://evil.example/?x=.trycloudflare.com",
        ] {
            assert_eq!(parse_tunnel_url(other), None, "{other}");
        }
        // Two URLs on one line: the tunnel is found past the first.
        assert_eq!(
            parse_tunnel_url(
                "terms https://www.cloudflare.com/x then https://a-b.trycloudflare.com/ ok"
            )
            .as_deref(),
            Some("https://a-b.trycloudflare.com")
        );
        assert!(tunnel_line_means_connected(
            "INF Registered tunnel connection connIndex=0 connection=… location=ams01 protocol=quic"
        ));
        assert!(!tunnel_line_means_connected("INF Starting metrics server"));
    }

    #[test]
    fn a_tunnel_only_fronts_a_plain_http_node_on_this_machine() {
        assert_eq!(
            local_http_origin("http://localhost:9510"),
            Ok(("localhost".into(), 9510))
        );
        assert_eq!(
            local_http_origin("http://127.0.0.1"),
            Ok(("127.0.0.1".into(), 9200))
        );
        assert!(local_http_origin("https://localhost:9200").is_err());
        assert!(local_http_origin("http://search.internal:9200").is_err());
        assert!(local_http_origin("http://localhost:notaport").is_err());
    }

    #[test]
    fn the_created_banner_says_what_is_shared_and_how_to_end_it() {
        let resp = json!({
            "handle": "1a2b3c4d5e6f", "label": "for Dana", "index": "ax-mail,ax-pdf",
            "brain": "mail", "expires_at": "2026-09-19T08:00:00Z", "max_claims": 1,
            "passcode": "k7mq-2xhd",
        });
        let default_node = Common {
            url: DEFAULT_URL.to_string(),
            data_dir: None,
            api_key: Some("never-printed".into()),
            json: false,
        };
        let local = render_created(
            &resp,
            "http://localhost:9200/_xerj-console/share#id",
            Reach::Loopback,
            &default_node,
        );
        assert!(local.contains("for Dana"));
        assert!(local.contains("ax-mail,ax-pdf"));
        assert!(local.contains("brain mail"));
        assert!(local.contains("read-only"));
        assert!(local.contains("k7mq-2xhd"));
        assert!(local.contains("2026-09-19T08:00:00Z"));
        assert!(local.contains("xerj share --revoke 1a2b3c4d5e6f"));
        assert!(local.contains("stay on this machine"));
        assert!(
            local.contains("--tunnel"),
            "a localhost link must say it is local-only"
        );
        assert!(
            local.contains("xerj share --revoke 1a2b3c4d5e6f\n"),
            "{local}"
        );
        let public = render_created(
            &resp,
            "https://a.trycloudflare.com/_xerj-console/share#id",
            Reach::CloudflareTunnel,
            &default_node,
        );
        assert!(!public.contains("only this machine can reach"));
        // One open is one browser tab; the banner says so next to the number.
        assert!(local.contains("one browser tab"), "{local}");
        assert!(local.contains("--max-claims"), "{local}");
        // On a non-default node the revoke line has to be pasteable: the same
        // --url and --data-dir, and never the key.
        let custom = Common {
            url: "http://localhost:9510".into(),
            data_dir: Some(PathBuf::from("/srv/xerj")),
            api_key: Some("never-printed".into()),
            json: false,
        };
        let out = render_created(
            &resp,
            "http://localhost:9510/_xerj-console/share#id",
            Reach::Loopback,
            &custom,
        );
        assert!(
            out.contains(
                "xerj share --revoke 1a2b3c4d5e6f --url http://localhost:9510 --data-dir /srv/xerj\n"
            ),
            "{out}"
        );
        assert!(!out.contains("never-printed"));
    }

    fn banner(link: &str, reach: Reach, node_url: &str) -> String {
        let resp = json!({
            "handle": "1a2b3c4d5e6f", "index": "ax-mail",
            "expires_at": "2026-09-19T08:00:00Z", "max_claims": 3, "passcode": "k7mq-2xhd",
        });
        let common = Common {
            url: node_url.to_string(),
            data_dir: None,
            api_key: None,
            json: false,
        };
        render_created(&resp, link, reach, &common)
    }

    /// Review of PR #947 (blocker): under `--tunnel` the banner said "the
    /// guest's browser reads from this node" and never said "Cloudflare",
    /// while every byte — passcode and guest key included — went through
    /// Cloudflare's TLS termination. The owner reads this banner, not the docs,
    /// in the second before sending the link.
    #[test]
    fn a_tunnel_link_says_cloudflare_can_read_the_traffic() {
        let out = banner(
            "https://a-b-c.trycloudflare.com/_xerj-console/share#id",
            Reach::CloudflareTunnel,
            DEFAULT_URL,
        );
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("goes through Cloudflare"), "{out}");
        assert!(flat.contains("ends TLS at Cloudflare"), "{out}");
        for crosses in [
            "the passcode",
            "the guest's key",
            "every document the guest opens",
        ] {
            assert!(flat.contains(crosses), "must name {crosses:?}: {out}");
        }
        assert!(
            flat.contains("--public-url"),
            "must name the alternative: {out}"
        );
        // What Cloudflare keeps is Cloudflare's business; the notice does not
        // promise anything about it. "nothing is uploaded or stored elsewhere"
        // sat one line above the notice and promised exactly that, which is the
        // retention claim f152538f removed from the notice itself.
        assert!(!flat.contains("stored there"), "{out}");
        for retention in ["or stored", "stored anywhere", "stored elsewhere"] {
            assert!(
                !flat.contains(retention),
                "the banner promises what a third party retains ({retention:?}): {out}"
            );
        }
        assert!(
            !flat.contains("browser reads from this node"),
            "in tunnel mode the browser reads from Cloudflare: {out}"
        );
        assert!(flat.contains("stay on this machine"), "{out}");
        assert!(out.lines().all(|l| l.chars().count() <= 100), "{out}");
        // No other mode mentions a third party that is not there.
        let local = banner(
            "http://localhost:9200/_xerj-console/share#id",
            Reach::Loopback,
            DEFAULT_URL,
        );
        assert!(!local.contains("ends TLS at Cloudflare"), "{local}");
        assert!(local.contains("browser reads from this node"), "{local}");
        // Behind --public-url the browser talks to whatever serves that
        // address (a reverse proxy, maybe a CDN), so the banner does not say
        // it reads from this node — it says who can read the traffic.
        let public = banner(
            "https://files.example.org/_xerj-console/share#id",
            Reach::PublicUrl,
            DEFAULT_URL,
        );
        let flat_public = public.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(!public.contains("ends TLS at Cloudflare"), "{public}");
        assert!(
            !flat_public.contains("browser reads from this node"),
            "{public}"
        );
        assert!(
            flat_public.contains("whoever ends TLS for that address can read"),
            "{public}"
        );
        assert!(public.lines().all(|l| l.chars().count() <= 100), "{public}");
        // `--help` says it where `--tunnel` is described, and no longer says
        // the guest's browser "talks to this node" without qualification.
        let help = help_text(false);
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("Cloudflare can read what passes through"),
            "{help}"
        );
        assert!(!flat.contains("browser talks to this node"), "{help}");
    }

    /// Review of PR #947 (minor): "only this machine can reach" was printed for
    /// any `--url`, and `--public-url http://…` printed a cleartext link with
    /// no warning although the docs call it the https:// address.
    #[test]
    fn the_banner_is_about_the_address_the_link_really_has() {
        assert_eq!(
            reach_of(false, None, "http://localhost:9200"),
            Reach::Loopback
        );
        assert_eq!(
            reach_of(false, None, "http://127.0.0.1:9510"),
            Reach::Loopback
        );
        assert_eq!(
            reach_of(false, None, "http://127.8.9.1:9510"),
            Reach::Loopback
        );
        assert_eq!(reach_of(false, None, "http://[::1]:9200"), Reach::Loopback);
        assert_eq!(
            reach_of(false, None, "http://192.168.1.20:9200"),
            Reach::Direct
        );
        assert_eq!(
            reach_of(false, None, "https://search.example.org"),
            Reach::Direct
        );
        assert_eq!(
            reach_of(false, None, "http://localhost.evil.example:9200"),
            Reach::Direct
        );
        assert_eq!(
            reach_of(false, Some("https://x.example"), "http://localhost:9200"),
            Reach::PublicUrl
        );
        assert_eq!(
            reach_of(true, None, "http://localhost:9200"),
            Reach::CloudflareTunnel
        );

        let lan = banner(
            "http://192.168.1.20:9200/_xerj-console/share#id",
            Reach::Direct,
            "http://192.168.1.20:9200",
        );
        assert!(!lan.contains("only this machine can reach"), "{lan}");
        assert!(lan.contains("plain http"), "{lan}");
        assert!(lan.contains("unencrypted"), "{lan}");

        let cleartext = banner(
            "http://plain.example/_xerj-console/share#id",
            Reach::PublicUrl,
            DEFAULT_URL,
        );
        assert!(cleartext.contains("plain http"), "{cleartext}");
        assert!(cleartext.contains("the passcode"), "{cleartext}");
        let tls = banner(
            "https://files.example.org/_xerj-console/share#id",
            Reach::PublicUrl,
            DEFAULT_URL,
        );
        assert!(!tls.contains("plain http"), "{tls}");
        // More than one open: no one-tab hint to read past.
        assert!(!tls.contains("one browser tab"), "{tls}");
    }

    /// Review of PR #947 (major): with a folder argument a wrong or non-admin
    /// key was reported as "<folder> has not been indexed … run `xerj brain`
    /// first" — `Es::get_doc` reads a `401` as "no document".
    #[test]
    fn a_refused_brain_lookup_is_a_key_problem_not_a_missing_brain() {
        let unauthorized =
            json!({"error": {"type": "security_exception", "reason": "x"}, "status": 401});
        assert_eq!(brain_meta(401, &unauthorized), BrainMeta::Refused);
        assert_eq!(brain_meta(403, &json!({})), BrainMeta::Refused);
        assert_eq!(brain_meta(500, &Value::Null), BrainMeta::Refused);
        assert_eq!(
            brain_meta(404, &json!({"found": false})),
            BrainMeta::NotIndexed
        );
        assert_eq!(brain_meta(404, &Value::Null), BrainMeta::NotIndexed);
        assert_eq!(
            brain_meta(200, &json!({"found": false})),
            BrainMeta::NotIndexed
        );
        assert_eq!(
            brain_meta(
                200,
                &json!({"found": true, "_source": {"nodes_index": "ax-docs"}})
            ),
            BrainMeta::Found(json!({"nodes_index": "ax-docs"}))
        );
        // …and a refusal becomes the sentence about the key, never about
        // indexing.
        let common = Common {
            url: "http://localhost:9200".into(),
            data_dir: None,
            api_key: None,
            json: false,
        };
        for status in [401, 403] {
            let text = format!("{:#}", explain(status, &unauthorized, &common));
            assert!(text.to_lowercase().contains("admin key"), "{text}");
            assert!(!text.contains("has not been indexed"), "{text}");
        }
    }

    /// Review of PR #947 (minor): `--url localhost:9613` was reported as "no
    /// xerj node answers" while the node was up.
    #[test]
    fn a_url_without_a_scheme_is_a_usage_error_that_says_what_to_type() {
        let err = parse(args(&["notes", "--url", "localhost:9613"]), None).unwrap_err();
        assert!(err.contains("http://localhost:9613"), "{err}");
        assert!(parse(args(&["notes", "--url", "https://node.example"]), None).is_ok());
        assert_eq!(url_host("http://localhost:9200/x"), Some("localhost"));
        assert_eq!(url_host("https://[::1]:9200"), Some("::1"));
        assert_eq!(url_host("http://10.0.0.7"), Some("10.0.0.7"));
        assert_eq!(url_host("localhost:9200"), None);
    }

    #[test]
    #[cfg(not(windows))]
    fn pasted_follow_up_commands_survive_spaces_and_quotes() {
        assert_eq!(sh_quote("/home/u/notes"), "/home/u/notes");
        assert_eq!(sh_quote("http://localhost:9510"), "http://localhost:9510");
        assert_eq!(sh_quote("/tmp/case files"), "'/tmp/case files'");
        assert_eq!(sh_quote("it's here"), "'it'\\''s here'");
        assert_eq!(sh_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(sh_quote(""), "''");
        let common = Common {
            url: "http://localhost:9510".into(),
            data_dir: Some(PathBuf::from("/srv/my data")),
            api_key: None,
            json: false,
        };
        assert_eq!(
            connection_args(&common),
            " --url http://localhost:9510 --data-dir '/srv/my data'"
        );
    }

    /// Review of PR #947 (minor): when `cloudflared` died on its own the owner
    /// was told "cloudflared stopped" and nothing else.
    #[test]
    fn cloudflareds_last_lines_are_kept_for_when_it_dies() {
        let log = TunnelLog::default();
        assert_eq!(tunnel_log_tail(&log), "");
        for i in 0..(TUNNEL_LOG_LINES + 5) {
            remember_tunnel_line(&log, &format!("INF line {i}  "));
        }
        remember_tunnel_line(&log, "");
        remember_tunnel_line(&log, &format!("ERR {}", "x".repeat(1000)));
        let tail = tunnel_log_tail(&log);
        assert!(tail.contains("cloudflared's last lines"), "{tail}");
        assert!(
            !tail.contains("INF line 0\n"),
            "oldest lines are dropped: {tail}"
        );
        assert!(!tail.contains("INF line 5\n"), "{tail}");
        assert!(
            tail.contains(&format!("INF line {}", TUNNEL_LOG_LINES + 4)),
            "{tail}"
        );
        assert!(tail.contains("ERR xxx"), "{tail}");
        assert_eq!(
            tail.lines().filter(|l| l.starts_with("    ")).count(),
            TUNNEL_LOG_LINES
        );
        assert!(
            tail.lines().all(|l| l.chars().count() <= 304),
            "long lines are cut"
        );
    }

    /// `xerj-api` cannot depend on `xerj-autoindex`, so it carries its own copy
    /// of the catalog's name for the "cannot be shared" rule. They must agree.
    #[test]
    fn the_unshareable_catalog_is_the_catalog_autoindex_writes() {
        assert_eq!(
            xerj_api::share::CATALOG_INDEX,
            xerj_autoindex::catalog::CATALOG_INDEX
        );
    }

    #[test]
    fn the_refusal_names_the_reason_not_just_the_rule() {
        let text = open_mode_refusal("http://localhost:9200");
        assert!(text.contains("authentication OFF"));
        assert!(text.contains("--insecure"));
        assert!(text.contains("restrict nothing"));
        assert!(text.contains("admin.key"));
    }

    #[test]
    fn help_documents_every_flag_and_the_refusal() {
        let help = help_text(false);
        for flag in [
            "--expires",
            "--max-claims",
            "--passcode",
            "--label",
            "--brain",
            "--index",
            "--tunnel",
            "--keep",
            "--public-url",
            "--url",
            "--data-dir",
            "--api-key",
            "--json",
            "--list",
            "--revoke",
        ] {
            assert!(help.contains(flag), "help must document {flag}");
        }
        assert!(help.contains("REFUSES"));
        assert!(help.contains("Read-only"));
        assert!(help.contains("stay on this machine"));
        assert!(help.contains("one browser tab"), "{help}");
    }

    #[test]
    fn status_codes_become_sentences() {
        let common = Common {
            url: "http://localhost:9200".into(),
            data_dir: None,
            api_key: None,
            json: false,
        };
        let text = |status, body: Value| format!("{:#}", explain(status, &body, &common));
        assert!(text(403, json!({})).contains("not the admin key"));
        assert!(text(401, json!({})).contains("ADMIN key"));
        assert!(text(409, json!({})).contains("authentication OFF"));
        assert!(text(404, json!({})).contains("older"));
        assert!(text(405, json!({})).contains("older"));
        assert_eq!(
            text(404, json!({"error": {"reason": "no such index [nope]"}})),
            "no such index [nope]"
        );
        assert!(text(400, json!({"error": {"reason": "bad thing"}})).contains("bad thing"));
    }
}
