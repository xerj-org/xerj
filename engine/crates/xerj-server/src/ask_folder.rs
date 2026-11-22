//! `xerj ask <folder> "<question>"` (#1800): index a local folder with the
//! `xerj autoindex` pipeline, then answer from it with `xerj search`.
//!
//! Composition, not a second pipeline:
//! - the node: an answering `--url` is used as is; a stopped loopback node is
//!   started again ([`xerj_common::localnode::ensure_for_url`]); with none
//!   recorded, a fresh one is booted the way `xerj brain` boots one
//!   ([`crate::brain::boot_server`]).
//! - indexing: the same binary's `autoindex <folder>` as a child process. Its
//!   stdout (plan, summary, `xerj-done`) goes to this command's stderr, so
//!   stdout carries only the answer. A re-run on an unchanged folder resumes
//!   from the autoindex journal and indexes nothing; `--no-reindex` skips the
//!   step for a follow-up question. The autoindex defaults are kept as they
//!   are: on 2026-11-22 its `--no-graph` route spent minutes sealing source
//!   snapshots at under 10 KB/s on a 6 MB folder the default route indexed
//!   in 162 s on the same machine.
//! - the answer: the same binary's `search` over the folder's own prefix, so
//!   ranking, `file:line` citations and the lexical-embedder note are the
//!   ones `xerj search` prints.
//!
//! Each folder gets its own prefix (`ask-<hash>-<name>`), because one prefix
//! is one source folder in autoindex.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use xerj_autoindex::esclient::Es;
use xerj_common::localauth;
use xerj_common::localnode;

pub const USAGE: &str = "usage: xerj ask <folder> \"<question>\" [options]

Indexes the folder (the `xerj autoindex` pipeline; a re-run only picks up
what changed), then answers from it with cited passages (`xerj search`).
The folder needs a path separator (./notes, ~/docs, /srv/data) unless the
question is a single quoted argument.

options:
  --url <node>        the node to index into (default: XERJ_URL, then the local
                      node; a stopped local node is started, and with none on
                      record a new one is booted)
  --data-dir <dir>    data dir for a node this command boots (default ~/.xerj/ask)
  --api-key <key>     node credential (default: XERJ_API_KEY, then the local
                      node's admin key)
  --prefix <p>        index prefix (default ask-<hash>-<folder name>)
  -k <n>              passages to return (default 5)
  --json              print the search response as JSON
  --fresh             rebuild the folder's index from scratch
  --no-reindex        answer from what is already indexed (a follow-up question)
  --yes               approve a large folder's indexing plan up front

exit codes: those of `xerj search` (0 answered, 1 nothing matched); 2 usage;
4 the folder is large enough that autoindex stopped to ask first (run the
printed `xerj autoindex` command, then ask again); otherwise the failing
step's code.";

/// One `xerj ask <folder>` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderAsk {
    pub root: PathBuf,
    pub question: String,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub prefix: Option<String>,
    pub k: Option<usize>,
    pub json: bool,
    pub fresh: bool,
    pub no_reindex: bool,
    pub yes: bool,
}

/// `~` and `~/…` against the home directory; anything else unchanged.
fn expand_home(arg: &str) -> PathBuf {
    if arg == "~" || arg.starts_with("~/") {
        if let Some(home) = xerj_common::home::home_dir() {
            return home.join(arg.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(arg)
}

/// True when the argument reads as a path, not a word of a question.
fn looks_like_path(arg: &str) -> bool {
    arg == "."
        || arg == ".."
        || arg.starts_with('~')
        || arg.contains('/')
        || arg.contains(std::path::MAIN_SEPARATOR)
}

/// The folder form of `xerj ask`, if `args` (what follows `ask`) is one.
///
/// `None` hands the arguments to the hub ask unchanged. The first argument
/// must be an existing directory, and it is only taken as the folder when it
/// reads as a path (`./src`, `~/notes`) or the question is one quoted
/// argument (`xerj ask notes "what changed"`). Without either, an unquoted
/// question whose first word happens to name a directory in the working
/// directory stays a hub question.
pub fn folder_request(args: &[String]) -> Option<Result<FolderAsk, String>> {
    let first = args.first()?;
    if first.starts_with('-') {
        return None;
    }
    let root = expand_home(first);
    if !root.is_dir() {
        return None;
    }
    let path_like = looks_like_path(first);
    match parse_rest(root, &args[1..]) {
        Ok((ask, words)) => match words {
            0 if path_like => Some(Err("a question is required".to_string())),
            0 => None,
            1 => Some(Ok(ask)),
            _ if path_like => Some(Ok(ask)),
            _ => None,
        },
        Err(e) if path_like => Some(Err(e)),
        Err(_) => None,
    }
}

/// Parse the options and question words after the folder. Returns the
/// request and how many question words there were.
fn parse_rest(root: PathBuf, args: &[String]) -> Result<(FolderAsk, usize), String> {
    let mut ask = FolderAsk {
        root,
        question: String::new(),
        url: None,
        api_key: None,
        data_dir: None,
        prefix: None,
        k: None,
        json: false,
        fresh: false,
        no_reindex: false,
        yes: false,
    };
    let mut words: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "-h" | "--help" => return Err("help".to_string()),
            "--url" => ask.url = Some(value("--url")?),
            "--api-key" => ask.api_key = Some(value("--api-key")?),
            "--data-dir" => ask.data_dir = Some(expand_home(&value("--data-dir")?)),
            "--prefix" => ask.prefix = Some(value("--prefix")?),
            "-k" => {
                let v = value("-k")?;
                match v.parse::<usize>() {
                    Ok(n) if n >= 1 => ask.k = Some(n),
                    _ => return Err(format!("-k must be a positive number, got '{v}'")),
                }
            }
            "--json" => ask.json = true,
            "--fresh" => ask.fresh = true,
            "--no-reindex" => ask.no_reindex = true,
            "--yes" => ask.yes = true,
            xerj_common::feedback::DISABLE_FLAG => {}
            f if f.starts_with('-') => {
                return Err(format!("unknown flag '{f}' for a folder question"))
            }
            word => words.push(word),
        }
        i += 1;
    }
    ask.question = words.join(" ");
    Ok((ask, words.len()))
}

/// `ask-<8 hex of the canonical path>-<folder name>`: one prefix per folder,
/// and no two folders' prefixes start one another (the hash is fixed width and
/// comes first).
pub fn default_prefix(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let digest = xerj_common::hub_evidence::sha256(canonical.to_string_lossy().as_bytes());
    let mut name = xerj_autoindex::dataset::sanitize_slug(
        &canonical
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    name.truncate(24);
    let name = name.trim_end_matches('-');
    if name.is_empty() {
        format!("ask-{}", &digest[..8])
    } else {
        format!("ask-{}-{name}", &digest[..8])
    }
}

fn default_data_dir() -> PathBuf {
    xerj_common::home::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".xerj")
        .join("ask")
}

/// Entry point for what [`folder_request`] matched: run the request, or
/// print usage (exit 0 for `--help`, 2 for a usage error).
pub fn run_cli(req: Result<FolderAsk, String>) -> i32 {
    let req = match req {
        Ok(req) if !req.question.trim().is_empty() => req,
        Ok(_) => {
            eprintln!("xerj ask: a question is required\n\n{USAGE}");
            return 2;
        }
        Err(e) if e == "help" => {
            println!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("xerj ask: {e}\n\n{USAGE}");
            return 2;
        }
    };
    match run(req) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xerj ask: {e:#}");
            1
        }
    }
}

/// The node `run` talks to, and the key it uses.
struct Node {
    url: String,
    api_key: Option<String>,
}

fn resolve_node(req: &FolderAsk) -> Result<Node> {
    let url = req
        .url
        .clone()
        .or_else(|| std::env::var("XERJ_URL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(localnode::default_url)
        .trim_end_matches('/')
        .to_string();
    let mut booted_dir: Option<PathBuf> = None;
    if !localnode::url_serving(&url) {
        if !localauth::url_is_loopback(&url) {
            bail!("no xerj node answers at {url}, and only a local node can be started from here");
        }
        if localnode::autostart_disabled() {
            bail!(
                "no xerj node answers at {url} and {} is set, so none is started",
                localnode::NO_AUTOSTART_ENV
            );
        }
        // An explicit --data-dir means "this node", so the recorded one for the
        // port is not started in its place.
        let restarted = if req.data_dir.is_none() {
            match localnode::ensure_for_url(&url) {
                Ok(Some(started)) => {
                    eprintln!("xerj ask: {}", started.sentence());
                    true
                }
                Ok(None) => localnode::url_serving(&url),
                Err(reason) => {
                    eprintln!("xerj ask: {reason}");
                    false
                }
            }
        } else {
            false
        };
        if !restarted {
            let data_dir = req.data_dir.clone().unwrap_or_else(default_data_dir);
            let port = localauth::xerj_url_port(&url);
            let booted = crate::brain::boot_server(&data_dir, port, "ask")?;
            eprintln!(
                "xerj ask: booted a xerj node (pid {}) on port {port}; data: {}, log: {}",
                booted.pid,
                data_dir.display(),
                booted.log_path.display()
            );
            crate::brain::wait_ready(&Es::new(&url, None)?, &booted)?;
            booted_dir = Some(data_dir);
        }
    }
    let api_key = req
        .api_key
        .clone()
        .or_else(|| std::env::var("XERJ_API_KEY").ok().filter(|s| !s.is_empty()))
        .or_else(|| {
            booted_dir.as_ref().and_then(|dir| {
                std::fs::read_to_string(dir.join("admin.key"))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        })
        .or_else(|| {
            localauth::url_is_loopback(&url)
                .then(|| localauth::discover_local_admin_key_for_url_with_origin(&url))
                .flatten()
                .map(|found| found.key)
        });
    Ok(Node { url, api_key })
}

/// The same binary, with the node's key in its environment rather than on
/// its command line.
fn child(exe: &Path, node: &Node) -> Command {
    let mut command = Command::new(exe);
    command.env_remove("XERJ_URL");
    match &node.api_key {
        Some(key) => command.env("XERJ_API_KEY", key),
        None => command.env_remove("XERJ_API_KEY"),
    };
    command
}

fn run(req: FolderAsk) -> Result<i32> {
    let t0 = Instant::now();
    let root = req
        .root
        .canonicalize()
        .with_context(|| format!("read folder {}", req.root.display()))?;
    let prefix = req.prefix.clone().unwrap_or_else(|| default_prefix(&root));
    if let Some(reason) = xerj_autoindex::dataset::index_prefix_error(&prefix) {
        bail!("{reason}");
    }
    let node = resolve_node(&req)?;
    let exe = std::env::current_exe().context("locate the xerj binary")?;
    let t_node = t0.elapsed();
    if req.no_reindex && req.fresh {
        bail!("--no-reindex and --fresh contradict each other");
    }
    if !req.no_reindex {
        if let Some(code) = index_folder(&exe, &node, &root, &prefix, &req)? {
            return Ok(code);
        }
    }
    let t_index = t0.elapsed();

    let mut search = child(&exe, &node);
    search
        .arg("search")
        .args(["--url", &node.url, "--prefix", &prefix]);
    if let Some(k) = req.k {
        search.args(["-k", &k.to_string()]);
    }
    if req.json {
        search.arg("--json");
    }
    search.arg(&req.question);
    let status = search
        .stdin(Stdio::null())
        .status()
        .context("run xerj search")?;
    let total = t0.elapsed();
    eprintln!(
        "xerj ask: time to first answer {:.2}s (node {:.2}s, index {:.2}s, search {:.2}s)",
        total.as_secs_f64(),
        t_node.as_secs_f64(),
        (t_index - t_node).as_secs_f64(),
        (total - t_index).as_secs_f64()
    );
    Ok(status.code().unwrap_or(1))
}

/// Run autoindex over the folder. `Some(code)` stops the ask with that exit
/// code; `None` means the index is searchable.
fn index_folder(
    exe: &Path,
    node: &Node,
    root: &Path,
    prefix: &str,
    req: &FolderAsk,
) -> Result<Option<i32>> {
    eprintln!(
        "xerj ask: indexing {} into {} (prefix {prefix})",
        root.display(),
        node.url
    );
    let mut index = child(exe, node);
    index
        .arg("autoindex")
        .arg(root)
        .args(["--url", &node.url, "--prefix", prefix]);
    if req.fresh {
        index.arg("--fresh");
    }
    if req.yes {
        index.arg("--yes");
    }
    let status = index
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .stderr(Stdio::inherit())
        .status()
        .context("run xerj autoindex")?;
    match status.code() {
        // 3 is completed-with-junk and friends: the index is searchable.
        Some(0) | Some(3) => Ok(None),
        Some(4) => {
            eprintln!(
                "xerj ask: autoindex stopped to ask before indexing this folder (see above). \
                 Approve it with --yes on this command, or index it first with: \
                 xerj autoindex {} --url {} --prefix {prefix} --yes",
                root.display(),
                node.url
            );
            Ok(Some(4))
        }
        code => {
            eprintln!("xerj ask: indexing failed ({status}); nothing to answer from.");
            Ok(Some(code.unwrap_or(1)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_path_like_folder_takes_an_unquoted_question() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().to_string_lossy().to_string();
        let req = folder_request(&s(&[&folder, "how", "does", "it", "work", "-k", "3"]))
            .unwrap()
            .unwrap();
        assert_eq!(req.question, "how does it work");
        assert_eq!(req.k, Some(3));
        assert_eq!(req.root, PathBuf::from(&folder));
    }

    #[test]
    fn a_missing_directory_or_a_flag_first_stays_a_hub_question() {
        assert!(folder_request(&s(&["/no/such/dir/xerj-ask", "q"])).is_none());
        assert!(folder_request(&s(&["--corpus", "lz4", "frame"])).is_none());
        assert!(folder_request(&s(&["how does lz4 work"])).is_none());
    }

    #[test]
    fn a_path_like_folder_with_no_question_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().to_string_lossy().to_string();
        assert_eq!(
            folder_request(&s(&[&folder])),
            Some(Err("a question is required".to_string()))
        );
        // A hub flag after a folder is refused, not silently ignored.
        let err = folder_request(&s(&[&folder, "q", "--corpus", "x"]))
            .unwrap()
            .unwrap_err();
        assert!(err.contains("--corpus"), "{err}");
    }

    #[test]
    fn default_prefix_is_a_valid_index_prefix_and_differs_per_folder() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let named = a.path().join("My Notes_2026");
        std::fs::create_dir(&named).unwrap();
        let p1 = default_prefix(&named);
        let p2 = default_prefix(b.path());
        assert!(
            p1.starts_with("ask-") && p1.ends_with("-my-notes-2026"),
            "{p1}"
        );
        assert_ne!(p1, p2);
        assert!(!p2.starts_with(&format!("{p1}-")) && !p1.starts_with(&format!("{p2}-")));
        for p in [&p1, &p2] {
            assert_eq!(xerj_autoindex::dataset::index_prefix_error(p), None, "{p}");
            assert_eq!(&xerj_autoindex::dataset::sanitize_prefix(p), p);
        }
    }

    #[test]
    fn help_after_the_folder_prints_usage() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().to_string_lossy().to_string();
        let req = folder_request(&s(&[&folder, "--help"])).unwrap();
        assert_eq!(req, Err("help".to_string()));
    }
}
