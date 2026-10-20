//! Corpus Hub v2 — node resolution, the automatic local node, and a blocking
//! HTTP client to a xerj node (plan §2 "Node for the client", §5 "Node").
//!
//! `build-pack`, `pull`, `def` and `ask` all talk to a running node: the builder
//! snapshots into one, `pull` restores into one, and `def`/`ask` query one. This
//! module resolves WHICH node ([`resolve_url`], the same `--url` / `XERJ_URL` /
//! `localhost:9200` order `xc.rs` uses), gives a small blocking client
//! ([`HubNode`]) over it, and — plan §5 — starts a node of its own when none
//! answers ([`ensure_node`]) and stops exactly that node ([`stop_node`]).
//!
//! The automatic node follows these rules, each of them a line in the plan or in
//! its "do not" list, or something a review reproduced going wrong without it:
//!
//! 1. **Never start a node on a port that answers.** The resolved URL is probed
//!    first; only a silent loopback URL leads to a spawn, and the port is chosen
//!    from ones that are bindable *now* (`xerj --port P` also claims `P+1` and
//!    `P+2`, so all three are checked).
//! 2. **Record what was started, all of it, the moment it exists.** Its PID, the
//!    port it was told to bind and the operating system's own start time for the
//!    process go to `<hub>/node.{port,start,pid}` (each written by rename, the PID
//!    last, so a reader that sees the PID sees the rest) *before* the starter
//!    waits for the node to boot. A starter that is killed, times out or fails
//!    mid-boot therefore never loses track of a live node; the next caller finds
//!    the record and waits for that node instead of starting another.
//! 3. **Never start a second node over a live one.** A recorded PID that is the
//!    node (or cannot be told apart from it) blocks a spawn: the caller waits for
//!    it to answer, or is told to run `xerj hub stop`. A node this tool started and
//!    then cannot use is killed, never left running unrecorded.
//! 4. **The node that answers is the node we started.** `pick_free_port` releases
//!    its sockets before the node binds, so something else can take the port in
//!    between. A healthy answer is only believed when `GET /_nodes` reports the
//!    PID of the child we spawned (`process.id`); otherwise the starter keeps
//!    waiting, and a node that dies because its port was taken is retried on a
//!    fresh port.
//! 5. **Stop only what was recorded, and be sure each time.** [`stop_node`]
//!    signals the recorded PID and nothing else. "Is this still the node?" is a
//!    command-line match *and* the recorded start time, and it is asked again
//!    immediately before SIGKILL: after SIGTERM the PID can have exited and been
//!    given to anything (or the node can have `exec`ed into another program), and
//!    a process that is no longer the node counts as the node being gone — it is
//!    never signalled again.
//! 6. **One starter at a time.** An OS file lock ([`crate::flock`]) serialises
//!    concurrent `xerj ask` runs; it belongs to the process holding it, so a
//!    killed starter never leaves a lock behind.
//!
//! [`HubNode`] implements [`XcHttp`] so `def::lookup` runs over it unchanged, and
//! it adds the extra verbs (`PUT`/`POST` with an arbitrary path, a readiness
//! poll) the builder and the pull path need.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use xerj_common::localauth::{discover_local_admin_key_for_url, url_is_loopback};
use xerj_common::xccode::XcHttp;

use crate::flock::{held_by, FileLock, LockError};

/// `--url` flag > `XERJ_URL` > `http://localhost:9200`, exactly as `xc.rs`
/// resolves it for the reference-coding read commands (a hub `pull`/`def`
/// honours `XERJ_URL` the same way `corpus index` does).
pub fn resolve_url(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("XERJ_URL").ok().filter(|u| !u.is_empty()))
        .unwrap_or_else(|| "http://localhost:9200".to_string())
}

/// A blocking-client builder for talking to the node at `url`.
///
/// A loopback node is never reached through a proxy. `HTTP_PROXY` is how a
/// corporate machine gets *out*; honouring it for `127.0.0.1` sends the health
/// probe to the proxy, and the proxy's own `502 Bad Gateway` page then reads as
/// "an HTTP answer, so a node is running" — the hub would decline to start the
/// node it needs, or talk to a proxy believing it is one. A remote `--url` keeps
/// the system proxy settings.
fn client_builder(url: &str) -> reqwest::blocking::ClientBuilder {
    let builder = reqwest::blocking::Client::builder();
    if url_is_loopback(url) {
        builder.no_proxy()
    } else {
        builder
    }
}

/// A blocking HTTP client to one xerj node. Holds the resolved base URL and the
/// `Authorization: ApiKey …` value (if any), reusing one `reqwest` client.
pub struct HubNode {
    base: String,
    auth: Option<String>,
    client: reqwest::blocking::Client,
}

impl HubNode {
    /// Connect to `url`, resolving the API key the way `xc.rs` does:
    /// `XERJ_API_KEY` wins; otherwise, for a loopback URL, fall back to the
    /// node's on-disk admin key (announced, loopback-only). An `--insecure`
    /// node needs no key and none is sent.
    pub fn connect(url: &str) -> Result<Self> {
        Self::connect_with_key(url, None)
    }

    /// Connect with an explicit API key (overrides discovery). `api_key: None`
    /// uses the same `XERJ_API_KEY` / loopback-admin-key fallback as
    /// [`HubNode::connect`].
    pub fn connect_with_key(url: &str, api_key: Option<String>) -> Result<Self> {
        let base = url.trim_end_matches('/').to_string();
        let key = match api_key {
            Some(k) => Some(k),
            None => match std::env::var("XERJ_API_KEY").ok().filter(|k| !k.is_empty()) {
                Some(k) => Some(k),
                None if url_is_loopback(&base) => {
                    discover_local_admin_key_for_url(&base).map(|(k, path)| {
                        eprintln!(
                            "xerj hub: no --api-key/XERJ_API_KEY given; using the admin key at {}",
                            path.display()
                        );
                        k
                    })
                }
                None => None,
            },
        };
        let client = client_builder(&base)
            .timeout(Duration::from_secs(300))
            .build()
            .context("build HTTP client")?;
        Ok(HubNode {
            base,
            auth: key.map(|k| format!("ApiKey {k}")),
            client,
        })
    }

    /// Connect to a node this tool manages itself ([`ensure_node`] starts it
    /// with `--insecure`), sending no credential. Unlike [`HubNode::connect`] it
    /// never reaches for a locally discoverable admin key: that key belongs to
    /// some other node, and announcing it on every `xerj ask` would be noise.
    pub fn connect_insecure(url: &str) -> Result<Self> {
        let client = client_builder(url)
            .timeout(Duration::from_secs(300))
            .build()
            .context("build HTTP client")?;
        Ok(HubNode {
            base: url.trim_end_matches('/').to_string(),
            auth: None,
            client,
        })
    }

    /// The resolved base URL (no trailing slash) — for diagnostics and the
    /// state ledger's `url` field.
    pub fn url(&self) -> &str {
        &self.base
    }

    /// One request returning `(status, parsed-body)`. A non-JSON body comes
    /// back as `Value::String`. Transport failure (not an HTTP status) is an
    /// `Err`; the caller classifies HTTP statuses itself.
    fn send(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
        let url = format!("{}{}", self.base, path);
        let m = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| format!("not an HTTP method: {method}"))?;
        let mut req = self.client.request(m, &url);
        if let Some(h) = &self.auth {
            req = req.header("Authorization", h);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .map_err(|e| format!("no response from {url}: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().map_err(|e| format!("read {url}: {e}"))?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((status, v))
    }

    /// A request that must return 2xx, else an `anyhow` error carrying the
    /// status and body. Used by the builder/pull control-plane calls (create
    /// index, bulk, snapshot, restore).
    pub fn request_ok(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let (status, v) = self
            .send(method, path, body)
            .map_err(|e| anyhow!("{method} {path}: {e}"))?;
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            bail!("{method} {path} -> HTTP {status}: {v}")
        }
    }

    /// `POST` a raw NDJSON `_bulk` body (content-type `application/x-ndjson`)
    /// and require 2xx. The builder uses this to index the defs documents;
    /// `request_ok`'s `.json()` cannot send the newline-delimited bulk format.
    pub fn bulk_ndjson(&self, path: &str, ndjson: String) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-ndjson")
            .body(ndjson);
        if let Some(h) = &self.auth {
            req = req.header("Authorization", h);
        }
        let resp = req.send().with_context(|| format!("bulk {url}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            bail!("bulk {path} -> HTTP {status}: {v}")
        }
    }

    /// Poll `GET /_cluster/health` until the node answers (any HTTP status that
    /// is not a transport error means it is up), up to `timeout`. Returns the
    /// last health body. It lets `build-pack` fail fast with a clear message
    /// when the node is not reachable. (The automatic node waits on its own
    /// stricter check, `wait_until_healthy`: HTTP 200 and a green/yellow
    /// status from the very process it started, and it stops early if that
    /// process has exited.)
    pub fn wait_until_reachable(&self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let last_err = match self.send("GET", "/_cluster/health", None) {
                Ok((status, v)) if (200..500).contains(&status) => return Ok(v),
                Ok((status, v)) => format!("HTTP {status}: {v}"),
                Err(e) => e,
            };
            if Instant::now() >= deadline {
                bail!("node at {} not reachable: {last_err}", self.base);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl XcHttp for HubNode {
    fn get_mapping(&self, path: &str) -> Result<Value, String> {
        match self.send("GET", path, None) {
            Ok((status, v)) if (200..300).contains(&status) => Ok(v),
            Ok((404, _)) => Err("__404__".to_string()),
            Ok((status, v)) => Err(format!("HTTP {status}: {v}")),
            Err(e) => Err(e),
        }
    }
    fn cat_indices_json(&self, pattern: &str) -> Result<Vec<String>, String> {
        let path = format!("/_cat/indices/{pattern}?format=json&h=index");
        match self.send("GET", &path, None) {
            // A wildcard matching no index is "none", not an error.
            Ok((404, _)) => Ok(Vec::new()),
            Ok((status, v)) if (200..300).contains(&status) => v
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|r| r.get("index").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .ok_or_else(|| "non-array _cat reply".to_string()),
            Ok((status, v)) => Err(format!("HTTP {status}: {v}")),
            Err(e) => Err(e),
        }
    }
    fn search(&self, index: &str, body: &Value) -> Result<Value, String> {
        match self.send("POST", &format!("/{index}/_search"), Some(body)) {
            Ok((status, v)) if (200..300).contains(&status) => Ok(v),
            Ok((404, _)) => Err("__404__".to_string()),
            Ok((status, v)) => Err(format!("HTTP {status}: {v}")),
            Err(e) => Err(e),
        }
    }
}

// ── The automatic local node (plan §5 "Node") ────────────────────────────────

/// The first ES-compat port an automatic node may take. `xerj --port P` also
/// claims `P+1` (native REST) and `P+2` (gRPC), so a candidate needs all three.
pub const FIRST_AUTO_PORT: u16 = 9300;
/// Candidate base ports tried (stepping by 3) before giving up.
const AUTO_PORT_TRIES: u16 = 100;
/// How long a freshly started node gets to report a healthy cluster (plan §5:
/// "wait for green (max 60 s)").
pub const NODE_BOOT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a second `xerj ask` waits for the first one to finish starting the
/// node before it gives up (boot timeout plus slack).
const NODE_LOCK_WAIT: Duration = Duration::from_secs(90);
/// How many fresh port triples a start tries when the node it launched lost the
/// race for its port (something else bound it between the check and the bind).
const START_ATTEMPTS: u32 = 3;
/// After SIGTERM, how long `hub stop` waits for the node to go before it asks
/// again with SIGKILL, and how long it waits for that.
const TERM_GRACE: Duration = Duration::from_secs(10);
const KILL_GRACE: Duration = Duration::from_secs(3);

const NODE_PID_FILE: &str = "node.pid";
const NODE_PORT_FILE: &str = "node.port";
const NODE_START_FILE: &str = "node.start";
const NODE_LOG_FILE: &str = "node.log";
const NODE_CONFIG_FILE: &str = "node.toml";
const NODE_DATA_DIR: &str = "node";
const NODE_LOCK_FILE: &str = "node.lock";

/// How to start "the xerj binary": the executable and any arguments that come
/// before the node's own flags. Production uses [`NodeLauncher::current`]
/// (`current_exe()`, no prefix); a test supplies its own pair so a cargo test
/// binary is never started with server flags it would reject.
#[derive(Debug, Clone)]
pub struct NodeLauncher {
    pub exe: PathBuf,
    pub pre_args: Vec<String>,
}

impl NodeLauncher {
    /// The running binary, i.e. the `xerj` the user invoked.
    pub fn current() -> Result<Self> {
        Ok(NodeLauncher {
            exe: std::env::current_exe().context("locate the xerj binary")?,
            pre_args: Vec::new(),
        })
    }

    /// A [`Command`] for the executable with the prefix arguments applied.
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.exe);
        c.args(&self.pre_args);
        c
    }
}

/// The node `ask`/`pull` will talk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNode {
    pub url: String,
    /// True when the node is the one this tool starts and records
    /// (`--insecure`, so no credential is sent); false for a user-supplied or
    /// default-URL node.
    pub managed: bool,
    /// True when THIS call started it (as opposed to finding it running).
    pub started_now: bool,
}

impl ResolvedNode {
    /// Connect with the right credential policy for this node.
    pub fn connect(&self) -> Result<HubNode> {
        if self.managed {
            HubNode::connect_insecure(&self.url)
        } else {
            HubNode::connect(&self.url)
        }
    }
}

/// A health probe's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub http_status: u16,
    /// `status` from the `/_cluster/health` body (`green`/`yellow`/`red`), when
    /// the body was JSON and carried one.
    pub status: Option<String>,
}

/// `GET {url}/_cluster/health` with a short timeout. `None` means nothing
/// answered (refused, timed out, not HTTP). ANY HTTP response counts as a live
/// node — a key-protected node answers 401 and must still not be started over.
/// (A loopback URL is probed directly, never through `HTTP_PROXY`.)
pub fn probe(url: &str, timeout: Duration) -> Option<Health> {
    let client = client_builder(url)
        .timeout(timeout)
        .connect_timeout(timeout)
        .build()
        .ok()?;
    let resp = client
        .get(format!("{}/_cluster/health", url.trim_end_matches('/')))
        .send()
        .ok()?;
    let http_status = resp.status().as_u16();
    let status = resp
        .text()
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("status").and_then(Value::as_str).map(str::to_string));
    Some(Health {
        http_status,
        status,
    })
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Who answered at a node URL, judged by the PID the answerer reports for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Responder {
    /// `GET /_nodes` reported the process we asked about.
    Is,
    /// Something answered, but not as that process (another PID, or no PID).
    IsNot(Option<u32>),
    /// Nothing usable answered.
    Silent,
}

/// Is the node answering at `url` the process `pid`? A xerj node reports its own
/// PID in `GET /_nodes` (`nodes.<id>.process.id`). A bare "health is green"
/// cannot tell the node we started from whatever else holds the port — the port
/// was only checked free *before* the node bound it.
fn confirm_responder(url: &str, pid: u32, timeout: Duration) -> Responder {
    let Ok(client) = client_builder(url)
        .timeout(timeout)
        .connect_timeout(timeout)
        .build()
    else {
        return Responder::Silent;
    };
    let Ok(resp) = client
        .get(format!("{}/_nodes", url.trim_end_matches('/')))
        .send()
    else {
        return Responder::Silent;
    };
    if !resp.status().is_success() {
        return Responder::IsNot(None);
    }
    let Ok(body) = resp.json::<Value>() else {
        return Responder::IsNot(None);
    };
    let pids: Vec<u32> = body
        .get("nodes")
        .and_then(Value::as_object)
        .map(|nodes| {
            nodes
                .values()
                .filter_map(|n| n.pointer("/process/id").and_then(Value::as_u64))
                .filter_map(|p| u32::try_from(p).ok())
                .collect()
        })
        .unwrap_or_default();
    if pids.contains(&pid) {
        Responder::Is
    } else {
        Responder::IsNot(pids.first().copied())
    }
}

/// The node data directory this tool starts a node on: `<hub>/node`.
pub fn node_data_dir(hub_home: &Path) -> PathBuf {
    hub_home.join(NODE_DATA_DIR)
}

/// Resolve the node to use, starting one if none answers (plan §5 "Node").
///
/// 1. `explicit` (`--url`) > `XERJ_URL` > `http://localhost:9200`. If that
///    answers, it is used — a node that answers is never started over.
/// 2. If it is silent and is NOT a loopback address, error: starting a local
///    node cannot satisfy a request for a remote one, and silently substituting
///    would hide the outage.
/// 3. Otherwise, a node this tool started earlier (see the module docs) is reused
///    when it is still the node and answers; if it is the node but has not
///    answered *yet* (it is booting, or stalled) it is waited for — a second node
///    is never started over it; if it is no longer the node, its record is
///    cleared.
/// 4. Otherwise start `exe --insecure --bind 127.0.0.1 --port <free ≥ 9300>
///    --data-dir <hub>/node --config <hub>/node.toml`, detached, record it, and
///    wait for a healthy cluster (≤ 60 s).
///
/// `--bind 127.0.0.1` and `--config` are additions to the plan's command line:
/// the node runs with auth off, so the bind must not be steerable from the
/// environment (`XERJ_BIND_ADDRESS`); and the snapshot-repo allowlist in
/// `node.toml` is what lets the node accept `<hub>/packs/…` as a restore
/// location — without it the engine refuses any repository outside the node's
/// own data dir and `xerj hub pull` could never restore into this node.
pub fn ensure_node(
    hub_home: &Path,
    explicit: Option<&str>,
    launcher: &NodeLauncher,
) -> Result<ResolvedNode> {
    ensure_node_with(hub_home, explicit, launcher, NODE_BOOT_TIMEOUT)
}

/// [`ensure_node`] with the boot timeout spelled out (tests shorten it).
fn ensure_node_with(
    hub_home: &Path,
    explicit: Option<&str>,
    launcher: &NodeLauncher,
    boot_timeout: Duration,
) -> Result<ResolvedNode> {
    let url = resolve_url(explicit);
    if probe(&url, PROBE_TIMEOUT).is_some() {
        return Ok(ResolvedNode {
            url,
            managed: false,
            started_now: false,
        });
    }
    if !url_is_loopback(&url) {
        bail!(
            "no node answers at {url}; it is not a local address, so no local node was started \
             (pass --url for a running node, or unset XERJ_URL to use the automatic one)"
        );
    }

    let mut waiting_since: Option<Instant> = None;
    loop {
        match recorded_state(hub_home) {
            Recorded::Healthy(node) => return Ok(node),
            Recorded::Unconfirmed(rec) => bail!("{}", unconfirmed_message(hub_home, &rec)),
            Recorded::Booting(rec) => {
                // A node of ours is alive and has not answered (yet). Starting a
                // second one over it would fight for the data directory — and
                // forgetting this one would orphan it — so wait for it.
                let port = rec
                    .port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".into());
                let since = match waiting_since {
                    Some(since) => since,
                    None => {
                        eprintln!(
                            "xerj hub: the hub node (pid {}) is running but not answering on \
                             port {port} yet; waiting up to {}s for it rather than starting \
                             another",
                            rec.pid,
                            boot_timeout.as_secs()
                        );
                        *waiting_since.insert(Instant::now())
                    }
                };
                if since.elapsed() >= boot_timeout {
                    bail!(
                        "the hub node (pid {}) is running but has not answered on port {port} for \
                         {}s, so no second node was started over it; `xerj hub stop` stops it \
                         (log: {})",
                        rec.pid,
                        boot_timeout.as_secs(),
                        hub_home.join(NODE_LOG_FILE).display()
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Recorded::Nothing => {}
        }

        // Nothing recorded: start one — serialised, so concurrent callers cannot
        // both launch a node on the same data directory.
        let lock = NodeLock::acquire(hub_home, NODE_LOCK_WAIT)?;
        // Read the record again under the lock: whoever held it may have started
        // a node (or be starting one) while this process waited.
        if matches!(recorded_state(hub_home), Recorded::Nothing) {
            return start_node(hub_home, launcher, &url, boot_timeout);
        }
        drop(lock);
    }
}

fn unconfirmed_message(hub_home: &Path, rec: &NodeRecord) -> String {
    if rec.port.is_none() {
        format!(
            "a hub node is running (pid {}) but its port was not recorded, so it cannot be \
             reached and no second node was started over it; `xerj hub stop` stops it",
            rec.pid
        )
    } else {
        format!(
            "cannot confirm that pid {} is the hub node (this platform or its permissions do \
             not expose the process command line or start time), so no second node was started \
             over it. If it is the node, stop it yourself; either way delete {} once it is gone",
            rec.pid,
            hub_home.join(NODE_PID_FILE).display()
        )
    }
}

/// What the hub's record says about the node it started earlier.
enum Recorded {
    /// Nothing is recorded, or what was recorded is gone (its files are cleared).
    Nothing,
    /// The node is running, is the process we recorded, and answers.
    Healthy(ResolvedNode),
    /// The node is running but does not answer (yet): booting, or stalled.
    Booting(NodeRecord),
    /// A process holds the record that cannot be told apart from the node on this
    /// platform. Never replaced, never signalled.
    Unconfirmed(NodeRecord),
}

/// Read the record and judge it. Files left by a node that is gone (or whose PID
/// now belongs to something else) are removed here, so `xerj hub status` never
/// reports a dead node.
fn recorded_state(hub_home: &Path) -> Recorded {
    let Some(rec) = read_record(hub_home) else {
        return Recorded::Nothing;
    };
    match identify_record(hub_home, &rec) {
        Identity::Gone | Identity::NotOurs => {
            clear_record_if(hub_home, &rec);
            Recorded::Nothing
        }
        Identity::Unknown => Recorded::Unconfirmed(rec),
        Identity::Ours => {
            let Some(port) = rec.port else {
                return Recorded::Unconfirmed(rec);
            };
            let url = node_url(port);
            // The port answering is not enough: while the node boots, something
            // else may hold the port it was told to bind.
            match confirm_responder(&url, rec.pid, PROBE_TIMEOUT) {
                Responder::Is => Recorded::Healthy(ResolvedNode {
                    url,
                    managed: true,
                    started_now: false,
                }),
                Responder::IsNot(_) | Responder::Silent => Recorded::Booting(rec),
            }
        }
    }
}

fn node_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Why one start attempt failed.
enum StartError {
    /// The node exited because the port it was told to bind was already taken.
    /// A fresh port may work.
    PortTaken(anyhow::Error),
    /// Anything else. Retrying would only repeat it.
    Fatal(anyhow::Error),
}

/// Start the node (caller holds the start lock and has seen no live record).
fn start_node(
    hub_home: &Path,
    launcher: &NodeLauncher,
    silent_url: &str,
    boot_timeout: Duration,
) -> Result<ResolvedNode> {
    let data_dir = node_data_dir(hub_home);
    // Never start a second node over a live one, and never overwrite a live record:
    // the caller has just seen nothing live recorded, and this is the last line of
    // defence for the next caller who forgets to look.
    if let Some(rec) = read_record(hub_home) {
        if matches!(
            identify_record(hub_home, &rec),
            Identity::Ours | Identity::Unknown
        ) {
            bail!(
                "a hub node (pid {}) is recorded and is, or may be, still running; not starting \
                 another over it — `xerj hub stop` stops it",
                rec.pid
            );
        }
    }
    fs::create_dir_all(&data_dir).with_context(|| format!("create {}", data_dir.display()))?;
    let config = write_node_config(hub_home)?;
    // What is left is debris: a record of a node that is gone, or orphan port/start
    // files from a starter that died before it wrote the PID.
    clear_record(hub_home);

    let mut from = first_port();
    for attempt in 1..=START_ATTEMPTS {
        let port = pick_free_port(from, AUTO_PORT_TRIES).ok_or_else(|| {
            anyhow!(
                "no free port for a local node: tried {} blocks of three from {from}",
                AUTO_PORT_TRIES
            )
        })?;
        match start_node_on(
            hub_home,
            launcher,
            silent_url,
            &data_dir,
            &config,
            port,
            boot_timeout,
        ) {
            Ok(node) => return Ok(node),
            Err(StartError::PortTaken(e)) if attempt < START_ATTEMPTS => {
                let why = format!("{e:#}");
                eprintln!(
                    "xerj hub: port {port} was taken by another process before the node could \
                     bind it; trying a different port (attempt {} of {START_ATTEMPTS}; {})",
                    attempt + 1,
                    why.lines().next().unwrap_or_default()
                );
                from = port.saturating_add(3);
            }
            Err(StartError::PortTaken(e) | StartError::Fatal(e)) => return Err(e),
        }
    }
    bail!("could not start a local node: every port tried was taken before the node could bind it")
}

/// The first base port a start tries: [`FIRST_AUTO_PORT`].
#[cfg(not(test))]
fn first_port() -> u16 {
    FIRST_AUTO_PORT
}

/// In the unit-test binary every start gets a block of ports of its own. The tests
/// run in parallel in one process and nearly all of them start "a node"; two of them
/// picking the same free triple is exactly the race [`wait_until_healthy`] exists to
/// catch (the stand-in that bound it first would answer for the other test's node),
/// and not what those tests are about. (Always above [`FIRST_AUTO_PORT`], so never
/// 9200 and never 9297-9299.)
#[cfg(test)]
fn first_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(FIRST_AUTO_PORT + 100);
    NEXT.fetch_add(30, Ordering::SeqCst)
}

/// One attempt: spawn on `port`, record it at once, wait until THAT process
/// answers.
fn start_node_on(
    hub_home: &Path,
    launcher: &NodeLauncher,
    silent_url: &str,
    data_dir: &Path,
    config: &Path,
    port: u16,
    boot_timeout: Duration,
) -> Result<ResolvedNode, StartError> {
    let log = hub_home.join(NODE_LOG_FILE);
    // The log is appended to across starts; only what this attempt writes may be
    // used to explain this attempt's failure.
    let log_from = fs::metadata(&log).map(|m| m.len()).unwrap_or(0);

    eprintln!(
        "xerj hub: no node answers at {silent_url}; starting a local node on port {port} \
         (data: {}, log: {})",
        data_dir.display(),
        log.display()
    );
    let args = vec![
        "--insecure".to_string(),
        "--bind".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--data-dir".to_string(),
        data_dir.to_string_lossy().into_owned(),
        "--config".to_string(),
        config.to_string_lossy().into_owned(),
    ];
    let child = spawn_detached(launcher, &args, &log)
        .with_context(|| format!("start the local node ({})", launcher.exe.display()))
        .map_err(StartError::Fatal)?;
    // From here on this function owns a live process: every way out of it that is
    // not "the node is up and recorded" ends that process (a panic included).
    let mut guard = ChildGuard::new(child);
    let pid = guard.child().id();

    // Recorded the moment the process exists, and all of it together: PID, port
    // and start time. A starter that dies, times out or fails after this point
    // leaves a node the next caller can find.
    let record = NodeRecord {
        pid,
        port: Some(port),
        start: proc_start_token(pid),
    };
    if let Err(e) = write_record(hub_home, &record) {
        drop(guard); // kill + reap the child first,
        clear_record_for(hub_home, pid); // then drop whatever of the record was written
        return Err(StartError::Fatal(e.context("record the local node")));
    }

    let url = node_url(port);
    match wait_until_healthy(guard.child(), &url, boot_timeout, &log, log_from) {
        Ok(()) => {
            guard.keep();
            eprintln!("xerj hub: node ready on port {port} (pid {pid})");
            Ok(ResolvedNode {
                url,
                managed: true,
                started_now: true,
            })
        }
        Err(failure) => {
            drop(guard);
            clear_record_for(hub_home, pid);
            let err = anyhow!("{}", failure.message);
            Err(if failure.port_taken {
                StartError::PortTaken(err)
            } else {
                StartError::Fatal(err)
            })
        }
    }
}

/// Ends (and reaps) a node this process spawned unless [`ChildGuard::keep`] is
/// called. A `Child` dropped on its own is neither killed nor waited for.
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        ChildGuard { child: Some(child) }
    }

    fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("child present until kept")
    }

    /// The node is up and recorded: let it outlive this process, which is the
    /// point of a detached node. Dropping a `Child` neither kills nor waits for it
    /// (the OS reparents it when this process exits).
    fn keep(mut self) {
        drop(self.child.take());
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // We spawned it and have not reaped it, so the PID cannot have been
            // recycled: this is stopping a process we own, not a recorded PID.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Why a node did not become healthy.
#[derive(Debug)]
struct BootFailure {
    message: String,
    /// It exited because the port it was told to bind was taken.
    port_taken: bool,
}

/// Poll until the process we spawned reports a healthy cluster: `/_cluster/health`
/// is 200 with status green (yellow is accepted too: a single node with an
/// unassigned replica is usable) **and** the answer comes from `child` itself
/// (`GET /_nodes` reports its PID). Gives up if the child exits or `timeout`
/// passes. An answer from some other process on the port is never accepted: the
/// child then dies binding it, which this reports as a taken port.
fn wait_until_healthy(
    child: &mut Child,
    url: &str,
    timeout: Duration,
    log: &Path,
    log_from: u64,
) -> Result<(), BootFailure> {
    let deadline = Instant::now() + timeout;
    let mut foreign: Option<String> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let tail = log_tail_since(log, log_from, 20);
                return Err(BootFailure {
                    port_taken: looks_like_port_taken(&tail),
                    message: format!(
                        "the local node exited ({status}) before becoming healthy; its log ({}):\n{tail}",
                        log.display()
                    ),
                });
            }
            Ok(None) => {}
            Err(e) => {
                return Err(BootFailure {
                    port_taken: false,
                    message: format!("cannot check the local node process: {e}"),
                })
            }
        }
        if let Some(h) = probe(url, PROBE_TIMEOUT) {
            if h.http_status == 200 && matches!(h.status.as_deref(), Some("green" | "yellow")) {
                match confirm_responder(url, child.id(), PROBE_TIMEOUT) {
                    Responder::Is => return Ok(()),
                    Responder::IsNot(Some(other)) => {
                        foreign = Some(format!("a different process (pid {other})"));
                    }
                    Responder::IsNot(None) => {
                        foreign = Some("a process that does not report its pid".to_string());
                    }
                    Responder::Silent => {}
                }
            }
        }
        if Instant::now() >= deadline {
            let log_tail = log_tail_since(log, log_from, 20);
            return Err(BootFailure {
                port_taken: false,
                message: match foreign {
                    Some(who) => format!(
                        "port {} is answered by {who}, not by the local node this tool started \
                         (pid {}); its log ({}):\n{log_tail}",
                        url.rsplit(':').next().unwrap_or("?"),
                        child.id(),
                        log.display()
                    ),
                    None => format!(
                        "the local node did not report a healthy cluster within {}s; its log ({}):\n{log_tail}",
                        timeout.as_secs(),
                        log.display()
                    ),
                },
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Does a node's log say it could not bind its port? Covers the wording of the
/// three platforms' `EADDRINUSE`.
fn looks_like_port_taken(log_tail: &str) -> bool {
    let t = log_tail.to_ascii_lowercase();
    t.contains("address already in use")
        || t.contains("address in use")
        || t.contains("addrinuse")
        || t.contains("os error 98") // Linux
        || t.contains("os error 48") // macOS / BSD
        || t.contains("os error 10048") // Windows
        || t.contains("only one usage of each socket address")
}

/// The last `lines` lines the log gained after byte `from`.
fn log_tail_since(path: &Path, from: u64, lines: usize) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            let start = usize::try_from(from).unwrap_or(usize::MAX).min(bytes.len());
            let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
            let all: Vec<&str> = text.lines().collect();
            all[all.len().saturating_sub(lines)..].join("\n")
        }
        Err(e) => format!("(log unreadable: {e})"),
    }
}

/// Write `<hub>/node.toml`: the one setting the node needs beyond its flags — a
/// snapshot-repo allowlist naming `<hub>/packs`, where `xerj hub pull` extracts
/// the snapshot it asks the node to restore. Rewritten on every start.
fn write_node_config(hub_home: &Path) -> Result<PathBuf> {
    let packs = hub_home.join("packs");
    fs::create_dir_all(&packs).with_context(|| format!("create {}", packs.display()))?;
    let path = hub_home.join(NODE_CONFIG_FILE);
    // serde_json string escaping is valid TOML basic-string escaping for the
    // characters a path can contain (the same trick `xerj brain` uses).
    let packs_toml = serde_json::to_string(&packs.to_string_lossy())?;
    write_file(
        &path,
        &format!(
            "# written by `xerj ask` each time it starts the hub node\n\
             [limits]\n\
             snapshot_repo_allowlist = [{packs_toml}]\n"
        ),
    )?;
    Ok(path)
}

fn write_file(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

/// Write `body` to `path` by writing a sibling temp file and renaming it over,
/// so a reader sees the old file or the whole new one, never half of it.
fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    let tmp = path.with_file_name(name);
    let result = fs::write(&tmp, body)
        .with_context(|| format!("write {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path)
                .with_context(|| format!("rename {} over {}", tmp.display(), path.display()))
        });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The first base port ≥ `start` (stepping by 3) whose three consecutive ports
/// are all bindable on loopback right now.
fn pick_free_port(start: u16, tries: u16) -> Option<u16> {
    let mut base = start;
    for _ in 0..tries {
        if base > u16::MAX - 2 {
            return None;
        }
        // Hold all three at once so the check is not three independent races.
        let held: Vec<_> = (0..3u16)
            .filter_map(|off| TcpListener::bind(("127.0.0.1", base + off)).ok())
            .collect();
        if held.len() == 3 {
            return Some(base);
        }
        base += 3;
    }
    None
}

/// Spawn `launcher` with `args`, detached: own process group (own session on
/// Windows), stdin closed, stdout and stderr appended to `log`. The child
/// outlives this process and never receives its Ctrl-C. Shared with the
/// background pull in `ask`.
pub(crate) fn spawn_detached(
    launcher: &NodeLauncher,
    args: &[String],
    log: &Path,
) -> Result<Child> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open {}", log.display()))?;
    let err = out.try_clone()?;
    let mut cmd = launcher.command();
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    detach(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("spawn {}", launcher.exe.display()))
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    cmd.creation_flags(0x0000_0008 | 0x0000_0200);
}

#[cfg(not(any(unix, windows)))]
fn detach(_cmd: &mut Command) {}

/// Serialises starting the node: an exclusive OS lock on `<hub>/node.lock`, held
/// from "nothing is recorded" to "the node is recorded and answering". It belongs
/// to the process holding it ([`crate::flock`]), so a starter killed mid-boot
/// releases it by dying — there is nothing stale to steal and no steal race.
struct NodeLock {
    _lock: FileLock,
}

impl NodeLock {
    fn acquire(hub_home: &Path, wait: Duration) -> Result<NodeLock> {
        fs::create_dir_all(hub_home).with_context(|| format!("create {}", hub_home.display()))?;
        let path = hub_home.join(NODE_LOCK_FILE);
        match FileLock::acquire(&path, wait) {
            Ok(lock) => Ok(NodeLock { _lock: lock }),
            Err(LockError::Timeout { holder }) => bail!(
                "another xerj process has been starting the hub node for over {}s (lock {}{}); \
                 the lock is released when that process exits, so retry once it has finished",
                wait.as_secs(),
                path.display(),
                held_by(holder)
            ),
            Err(LockError::Io(e)) => {
                Err(e).with_context(|| format!("take the start lock {}", path.display()))
            }
        }
    }
}

// ── the record: pid + port + start time ──────────────────────────────────────

/// What the hub records about the node it started: the process, the port it was
/// told to bind, and when the operating system says the process started (an
/// opaque token; see [`proc_start_token`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeRecord {
    pub(crate) pid: u32,
    pub(crate) port: Option<u16>,
    /// `None` where the platform could not give a start time at spawn.
    pub(crate) start: Option<String>,
}

/// Write the record: port and start time first, the PID **last**. Each is written
/// by rename, so a reader never sees half a file, and because `node.pid` is the
/// commit marker a reader that finds a PID also finds the port and start time that
/// belong to it. (A crash before the PID leaves orphan port/start files, which the
/// next start — under the start lock — clears; readers never delete them, since
/// they cannot tell an orphan from a record that is about to be committed.)
fn write_record(hub_home: &Path, rec: &NodeRecord) -> Result<()> {
    let port = rec.port.context("a node record needs a port")?;
    write_atomic(&hub_home.join(NODE_PORT_FILE), &format!("{port}\n"))?;
    match &rec.start {
        Some(token) => write_atomic(&hub_home.join(NODE_START_FILE), &format!("{token}\n"))?,
        None => {
            let _ = fs::remove_file(hub_home.join(NODE_START_FILE));
        }
    }
    write_atomic(&hub_home.join(NODE_PID_FILE), &format!("{}\n", rec.pid))
}

/// The recorded node, if `node.pid` holds a PID.
fn read_record(hub_home: &Path) -> Option<NodeRecord> {
    let pid = read_pid(hub_home)?;
    Some(NodeRecord {
        pid,
        port: read_trimmed(&hub_home.join(NODE_PORT_FILE)).and_then(|s| s.parse().ok()),
        start: read_trimmed(&hub_home.join(NODE_START_FILE)),
    })
}

fn read_pid(hub_home: &Path) -> Option<u32> {
    read_trimmed(&hub_home.join(NODE_PID_FILE))?.parse().ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Remove the record: the PID first, so the record stops existing at once and
/// what is left are only orphans.
fn clear_record(hub_home: &Path) {
    let _ = fs::remove_file(hub_home.join(NODE_PID_FILE));
    let _ = fs::remove_file(hub_home.join(NODE_PORT_FILE));
    let _ = fs::remove_file(hub_home.join(NODE_START_FILE));
}

/// [`clear_record`], but only when the record is still `pid`'s — a failing starter
/// must never delete a record somebody else has since written.
fn clear_record_for(hub_home: &Path, pid: u32) {
    if read_pid(hub_home).is_none_or(|recorded| recorded == pid) {
        clear_record(hub_home);
    }
}

/// [`clear_record`], but only when the record on disk is still exactly `rec` — the
/// one a caller examined and found to be a node that is gone. Examining a record and
/// deleting it are two steps, and between them a starter may have replaced it with
/// the record of the node it just started; deleting THAT is the hub losing track of
/// a live node. (The check and the delete are still two steps. What is left is the
/// time between two syscalls rather than an identity check that can take 175 ms.)
fn clear_record_if(hub_home: &Path, rec: &NodeRecord) {
    if read_record(hub_home).as_ref() == Some(rec) {
        clear_record(hub_home);
    }
}

// ── who is that process? ─────────────────────────────────────────────────────

/// What a PID is, relative to the node this tool started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Identity {
    /// Running, its command line names this hub's node data directory, and (when
    /// one was recorded) it started when the node did.
    Ours,
    /// Running, but it is not the node we started (a recycled PID, a decoy, a
    /// node that `exec`ed into another program).
    NotOurs,
    /// No such process (or only a zombie awaiting its parent's `wait`).
    Gone,
    /// Cannot tell on this platform / with these permissions. Never signalled.
    Unknown,
}

/// Identify `pid` by its command line alone: does it contain every needle? On
/// Linux the command line is `/proc/<pid>/cmdline`; elsewhere on unix it is `ps`;
/// on Windows only the image name is available, so the needle check is replaced
/// by "the image is a xerj binary". Good enough to notice a duplicate background
/// pull; **not** enough to signal on — see [`identify_started`].
pub(crate) fn identify(pid: u32, needles: &[&str]) -> Identity {
    identify_started(pid, needles, None)
}

/// [`identify`], and — when `expected_start` is given — the process must also
/// have started when the recorded one did ([`proc_start_token`]). That is what
/// tells the node we started from a different process that has since been given
/// its PID: a recycled PID has a different start time whatever its command line
/// says. A start time that cannot be read for a process that should have one is
/// `Unknown`, never `Ours`.
pub(crate) fn identify_started(
    pid: u32,
    needles: &[&str],
    expected_start: Option<&str>,
) -> Identity {
    match proc_state(pid) {
        ProcState::Gone | ProcState::Zombie => Identity::Gone,
        ProcState::Unknown => Identity::Unknown,
        ProcState::Running => {
            if let Some(want) = expected_start {
                match proc_start_token(pid) {
                    Some(got) if start_tokens_match(want, &got) => {}
                    Some(_) => return Identity::NotOurs,
                    None => return Identity::Unknown,
                }
            }
            // A process that has only just exec'd is not reliably readable: for a
            // few milliseconds `/proc/<pid>/cmdline` is empty, shows the PARENT's
            // command line (the child still shares its address space), or is a
            // splice of both. Measured: reading it right after `spawn` returned
            // mis-identified a live, correct node as "not ours" in roughly one
            // run in ten under load. So a match is believed at once, but a
            // mismatch (or an empty read) must persist across several samples
            // before it counts.
            let mut saw_other = false;
            for attempt in 0..IDENTIFY_SAMPLES {
                match proc_cmdline(pid) {
                    Some(cmd) if identity_matches(&cmd, needles) => return Identity::Ours,
                    Some(_) => saw_other = true,
                    None => {}
                }
                if attempt + 1 < IDENTIFY_SAMPLES {
                    std::thread::sleep(IDENTIFY_GAP);
                }
            }
            if saw_other {
                Identity::NotOurs
            } else {
                Identity::Unknown
            }
        }
    }
}

/// Does the start token recorded for the node describe the same process as the one
/// read now? Equal tokens do. Linux tokens are `<boot id>:<ticks>`; if only one of
/// the two readers could see the boot id (a sandboxed `/proc/sys`), the ticks decide
/// — a start time that merely *looked* different would have the hub conclude its
/// node is gone and orphan it, which is worse than a boot id it cannot compare.
fn start_tokens_match(recorded: &str, current: &str) -> bool {
    if recorded == current {
        return true;
    }
    match (split_boot_token(recorded), split_boot_token(current)) {
        (Some((_, ticks)), None) => ticks == current,
        (None, Some((_, ticks))) => ticks == recorded,
        _ => false,
    }
}

/// `<36-char boot id>:<ticks>` → `(boot id, ticks)`.
fn split_boot_token(token: &str) -> Option<(&str, &str)> {
    let (boot, ticks) = token.rsplit_once(':')?;
    let boot_ok = boot.len() == 36 && boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    let ticks_ok = !ticks.is_empty() && ticks.bytes().all(|b| b.is_ascii_digit());
    (boot_ok && ticks_ok).then_some((boot, ticks))
}

/// How many times [`identify`] reads a running process's command line before
/// believing a mismatch, and the pause between reads (~175 ms in all, paid only
/// when the process is NOT the one being looked for).
const IDENTIFY_SAMPLES: u32 = 8;
const IDENTIFY_GAP: Duration = Duration::from_millis(25);

#[cfg(not(windows))]
fn identity_matches(cmdline: &str, needles: &[&str]) -> bool {
    needles.iter().all(|n| cmdline.contains(n))
}

#[cfg(windows)]
fn identity_matches(image: &str, _needles: &[&str]) -> bool {
    image.to_ascii_lowercase().contains("xerj")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))] // `tasklist` has no zombie state
enum ProcState {
    Running,
    Zombie,
    Gone,
    Unknown,
}

/// `(state, starttime)` from the contents of `/proc/<pid>/stat`.
///
/// The line is `pid (comm) S ppid …` and `comm` may contain spaces and
/// parentheses, so everything after the LAST `)` is the fixed-format tail: its
/// first field is the state (field 3 of `proc(5)`), and `starttime` — clock ticks
/// since boot, field 22 — is the 20th.
#[cfg(target_os = "linux")]
fn parse_proc_stat(stat: &str) -> Option<(char, u64)> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let starttime = fields.nth(18)?.parse().ok()?;
    Some((state, starttime))
}

#[cfg(target_os = "linux")]
fn proc_state(pid: u32) -> ProcState {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => match parse_proc_stat(&s) {
            Some(('Z' | 'X', _)) => ProcState::Zombie,
            Some(_) => ProcState::Running,
            None => ProcState::Unknown,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No such entry means "no such process" only when /proc exists at all;
            // in a chroot or sandbox without it, a live node would read as gone.
            if Path::new("/proc/self/stat").exists() {
                ProcState::Gone
            } else {
                ProcState::Unknown
            }
        }
        Err(_) => ProcState::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn proc_cmdline(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split(|b| *b == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// When the operating system says `pid` started, as an opaque token that differs
/// between two processes that share a PID. Linux: the boot id plus
/// `/proc/<pid>/stat` field 22 (ticks since boot), so a recycled PID — even one
/// handed out after a reboot — never matches. `None` when it cannot be read.
#[cfg(target_os = "linux")]
fn proc_start_token(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, ticks) = parse_proc_stat(&stat)?;
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(match boot_id {
        Some(boot) => format!("{boot}:{ticks}"),
        None => ticks.to_string(),
    })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn proc_state(pid: u32) -> ProcState {
    match Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
    {
        Ok(out) if out.status.success() => {
            let stat = String::from_utf8_lossy(&out.stdout);
            match stat.trim().chars().next() {
                Some('Z') => ProcState::Zombie,
                Some(_) => ProcState::Running,
                None => ProcState::Gone,
            }
        }
        // `ps -p` exits non-zero when the PID does not exist.
        Ok(_) => ProcState::Gone,
        Err(_) => ProcState::Unknown,
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn proc_cmdline(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// macOS and the BSDs: `ps -o lstart=` is the process start time to the second
/// (`Tue Oct 14 13:55:12 2026`). The C locale keeps the spelling stable.
#[cfg(all(unix, not(target_os = "linux")))]
fn proc_start_token(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(windows)]
fn tasklist_row(pid: u32) -> Result<Option<String>, ()> {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map_err(|_| ())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    Ok(text
        .lines()
        .find(|l| l.starts_with('"'))
        .map(str::to_string))
}

#[cfg(windows)]
fn proc_state(pid: u32) -> ProcState {
    match tasklist_row(pid) {
        Ok(Some(_)) => ProcState::Running,
        Ok(None) => ProcState::Gone,
        Err(()) => ProcState::Unknown,
    }
}

#[cfg(windows)]
fn proc_cmdline(pid: u32) -> Option<String> {
    tasklist_row(pid).ok().flatten()
}

/// Windows: the process creation time as 100 ns ticks (`FILETIME`), from
/// PowerShell. Unverified on a Windows host in this review; if it cannot be read
/// the record carries no start time, and [`identify_record`] then refuses to call
/// the process the node (an image name alone is not an identity).
#[cfg(windows)]
fn proc_start_token(pid: u32) -> Option<String> {
    let script = format!("(Get-Process -Id {pid} -ErrorAction Stop).StartTime.ToFileTimeUtc()");
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then_some(s)
}

#[cfg(not(any(unix, windows)))]
fn proc_state(_pid: u32) -> ProcState {
    ProcState::Unknown
}

#[cfg(not(any(unix, windows)))]
fn proc_cmdline(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(unix, windows)))]
fn proc_start_token(_pid: u32) -> Option<String> {
    None
}

/// The needle that makes a PID "the hub node we started": its command line
/// carries `--data-dir <hub>/node` (the two arguments adjacent), which only the
/// node this tool launched has (on Windows only the image name is checked — see
/// [`identify`]).
fn node_needles(hub_home: &Path) -> Vec<String> {
    vec![format!(
        "--data-dir {}",
        node_data_dir(hub_home).to_string_lossy()
    )]
}

/// Is the recorded process still the node? Command line **and** start time.
fn identify_record(hub_home: &Path, rec: &NodeRecord) -> Identity {
    let needles = node_needles(hub_home);
    let refs: Vec<&str> = needles.iter().map(String::as_str).collect();
    let identity = identify_started(rec.pid, &refs, rec.start.as_deref());
    // Windows can only read an image name from `tasklist`, and another xerj on
    // the machine (the user's own server, another hub's node) has the same one.
    // Without a recorded start time there is nothing to tell them apart, so the
    // process is never called the node.
    #[cfg(windows)]
    {
        if identity == Identity::Ours && rec.start.is_none() {
            return Identity::Unknown;
        }
    }
    identity
}

/// Remove the record when the recorded process is gone or is no longer the node
/// we started. An `Unknown` identity leaves the files alone, and so does a record
/// with no PID: port/start files without one are either debris (the next start
/// clears it) or a record being committed right now, and a reader cannot tell.
pub fn reap_stale(hub_home: &Path) {
    let Some(rec) = read_record(hub_home) else {
        return;
    };
    if matches!(
        identify_record(hub_home, &rec),
        Identity::Gone | Identity::NotOurs
    ) {
        clear_record_if(hub_home, &rec);
    }
}

// ── stopping ─────────────────────────────────────────────────────────────────

/// What [`stop_node`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// No `node.pid` — nothing was started by this tool.
    NothingRecorded,
    /// The recorded process was already gone (or its PID now belongs to another
    /// program); the stale record was removed and nothing was signalled.
    StaleRemoved { pid: u32 },
    /// The recorded node was stopped.
    Stopped { pid: u32 },
}

/// Stop the node this tool started: ONLY the PID in `<hub>/node.pid`, and only
/// after confirming the process is still that node. Never matches by name,
/// never signals a process group, never touches a node it did not record
/// (plan §11).
pub fn stop_node(hub_home: &Path) -> Result<StopOutcome> {
    stop_node_with(hub_home, TERM_GRACE, KILL_GRACE)
}

/// [`stop_node`] with the two waits spelled out (tests shorten them).
fn stop_node_with(
    hub_home: &Path,
    term_grace: Duration,
    kill_grace: Duration,
) -> Result<StopOutcome> {
    let Some(rec) = read_record(hub_home) else {
        return Ok(StopOutcome::NothingRecorded);
    };
    let pid = rec.pid;
    match identify_record(hub_home, &rec) {
        Identity::Gone | Identity::NotOurs => {
            clear_record_if(hub_home, &rec);
            Ok(StopOutcome::StaleRemoved { pid })
        }
        Identity::Unknown => bail!(
            "cannot confirm that pid {pid} is the hub node (this platform or its permissions \
             do not expose the process command line or start time); not signalling it. If it \
             is the node, stop it yourself and delete {}",
            hub_home.join(NODE_PID_FILE).display()
        ),
        Identity::Ours => {
            terminate(pid)?;
            // Not gone yet? Wait, then escalate once — but only if the PID is
            // STILL the node. Ten seconds is long enough for the node to have
            // exited and the PID to belong to anything, or for the node to have
            // exec'ed into something else; a process that is no longer the node
            // is not ours to kill, and not being the node counts as the node
            // being gone.
            if !wait_gone(hub_home, &rec, term_grace) {
                match identify_record(hub_home, &rec) {
                    Identity::Ours => {
                        kill_hard(pid)?;
                        if !wait_gone(hub_home, &rec, kill_grace) {
                            bail!(
                                "pid {pid} (the hub node) did not exit after SIGTERM and SIGKILL"
                            );
                        }
                    }
                    Identity::Gone | Identity::NotOurs => {}
                    Identity::Unknown => bail!(
                        "pid {pid} (the hub node) did not exit after SIGTERM, and it can no \
                         longer be confirmed to be the node, so SIGKILL was not sent. If it is \
                         still running, stop it yourself and delete {}",
                        hub_home.join(NODE_PID_FILE).display()
                    ),
                }
            }
            clear_record_if(hub_home, &rec);
            Ok(StopOutcome::Stopped { pid })
        }
    }
}

/// Has the node gone? True when the recorded PID is no process, a zombie, or a
/// process that is not the node any more.
fn wait_gone(hub_home: &Path, rec: &NodeRecord, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if matches!(
            identify_record(hub_home, rec),
            Identity::Gone | Identity::NotOurs
        ) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn terminate(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGTERM)
}

#[cfg(unix)]
fn kill_hard(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGKILL)
}

#[cfg(unix)]
fn send_signal(pid: u32, sig: libc::c_int) -> Result<()> {
    // A single PID — never `-pid` (a whole process group).
    // SAFETY: plain syscall with integer arguments; `pid` was read from the
    // hub's own pid file and identity-checked immediately before.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(()) // exited between the identity check and the signal
        } else {
            Err(err).with_context(|| format!("signal pid {pid}"))
        }
    }
}

#[cfg(windows)]
fn terminate(pid: u32) -> Result<()> {
    taskkill(pid)
}

#[cfg(windows)]
fn kill_hard(pid: u32) -> Result<()> {
    taskkill(pid)
}

#[cfg(windows)]
fn taskkill(pid: u32) -> Result<()> {
    // `/F` without `/T`: exactly this PID, not its children.
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run taskkill")?;
    if status.success() {
        Ok(())
    } else {
        bail!("taskkill /PID {pid} failed ({status})")
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate(_pid: u32) -> Result<()> {
    bail!("stopping a process is not supported on this platform")
}

#[cfg(not(any(unix, windows)))]
fn kill_hard(_pid: u32) -> Result<()> {
    bail!("stopping a process is not supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    // `resolve_url` mutates process env, which is global; serialise the env
    // cases so they do not race other tests in the crate.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn url_resolution_prefers_flag_then_env_then_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Flag wins over everything.
        std::env::set_var("XERJ_URL", "http://env:1");
        assert_eq!(resolve_url(Some("http://flag:1")), "http://flag:1");
        // Env is used when no flag.
        assert_eq!(resolve_url(None), "http://env:1");
        // Default when neither.
        std::env::remove_var("XERJ_URL");
        assert_eq!(resolve_url(None), "http://localhost:9200");
        // An empty XERJ_URL is ignored (falls through to the default).
        std::env::set_var("XERJ_URL", "");
        assert_eq!(resolve_url(None), "http://localhost:9200");
        std::env::remove_var("XERJ_URL");
    }

    #[test]
    fn connect_trims_trailing_slash_and_sets_no_auth_without_a_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XERJ_API_KEY");
        // A non-loopback URL never reaches for a local admin key, so no auth
        // header is set when none was given.
        let n = HubNode::connect("http://example.invalid:9200/").unwrap();
        assert_eq!(n.url(), "http://example.invalid:9200");
        assert!(n.auth.is_none());
    }

    #[test]
    fn explicit_key_becomes_an_apikey_header() {
        let n = HubNode::connect_with_key("http://example.invalid:9200", Some("sekret".into()))
            .unwrap();
        assert_eq!(n.auth.as_deref(), Some("ApiKey sekret"));
    }

    // ── the automatic node ──────────────────────────────────────────────────

    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A stand-in node on an ephemeral loopback port: answers every request with
    /// whatever `handler` returns for the request path. The threads live until the
    /// test binary exits.
    fn fake_server(
        handler: impl Fn(&str) -> (&'static str, String) + Send + Sync + 'static,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = std::sync::Arc::new(handler);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let handler = handler.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 2048];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/");
                    let (status, body) = handler(path);
                    let reply = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = conn.write_all(reply.as_bytes());
                });
            }
        });
        port
    }

    /// Answers every request with `status` and the given JSON body.
    fn fake_node(status: &'static str, body: &'static str) -> u16 {
        fake_server(move |_| (status, body.to_string()))
    }

    /// A healthy node whose `GET /_nodes` reports `pid` as its own process.
    fn fake_node_with_pid(pid: u32) -> u16 {
        fake_server(move |path| {
            if path.starts_with("/_nodes") {
                (
                    "200 OK",
                    format!(r#"{{"nodes":{{"n1":{{"process":{{"id":{pid}}}}}}}}}"#),
                )
            } else {
                ("200 OK", r#"{"status":"green"}"#.to_string())
            }
        })
    }

    /// A launcher that must never be used.
    fn forbidden_launcher() -> NodeLauncher {
        NodeLauncher {
            exe: PathBuf::from("/nonexistent/xerj-must-not-be-spawned"),
            pre_args: vec![],
        }
    }

    /// A loopback port with nothing on it.
    fn silent_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn silent_url() -> String {
        format!("http://127.0.0.1:{}", silent_port())
    }

    #[test]
    fn a_node_that_answers_is_used_and_nothing_is_started() {
        let hub = tempfile::tempdir().unwrap();
        let port = fake_node("200 OK", r#"{"status":"green"}"#);
        let url = format!("http://127.0.0.1:{port}");
        let r = ensure_node(hub.path(), Some(&url), &forbidden_launcher()).unwrap();
        assert_eq!(r.url, url);
        assert!(!r.managed && !r.started_now);
        assert!(!hub.path().join("node.pid").exists());
    }

    #[test]
    fn a_key_protected_node_that_answers_401_still_counts_as_running() {
        let hub = tempfile::tempdir().unwrap();
        let port = fake_node("401 Unauthorized", r#"{"error":"auth"}"#);
        let url = format!("http://127.0.0.1:{port}");
        let r = ensure_node(hub.path(), Some(&url), &forbidden_launcher()).unwrap();
        assert_eq!(r.url, url, "never started over");
        let h = probe(&url, Duration::from_secs(2)).unwrap();
        assert_eq!(h.http_status, 401);
    }

    #[test]
    fn probe_reads_the_cluster_status_and_reports_silence_as_none() {
        let port = fake_node("200 OK", r#"{"status":"yellow","number_of_nodes":1}"#);
        let h = probe(&format!("http://127.0.0.1:{port}/"), Duration::from_secs(2)).unwrap();
        assert_eq!(h.http_status, 200);
        assert_eq!(h.status.as_deref(), Some("yellow"));
        // A port nothing listens on: bind then drop to find one.
        let closed = silent_port();
        assert!(probe(
            &format!("http://127.0.0.1:{closed}"),
            Duration::from_secs(1)
        )
        .is_none());
    }

    #[test]
    fn a_silent_remote_url_is_an_error_not_a_local_node() {
        let hub = tempfile::tempdir().unwrap();
        let err = ensure_node(
            hub.path(),
            Some("http://nonexistent.invalid:9200"),
            &forbidden_launcher(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a local address"), "{err}");
        assert!(!hub.path().join("node.pid").exists());
    }

    #[test]
    fn free_port_search_skips_a_busy_triple() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = held.local_addr().unwrap().port();
        let found = pick_free_port(busy, 50).expect("some free triple above");
        // Candidates step by three from the start, so a busy start is passed over.
        assert!(found > busy, "the busy base port is skipped");
        assert_eq!((found - busy) % 3, 0);
        // The search is bounded by the top of the port range and by `tries`.
        assert!(
            pick_free_port(u16::MAX - 1, 5).is_none(),
            "no room above the top"
        );
        assert!(pick_free_port(busy, 0).is_none(), "no tries, no port");
        // The default floor is the plan's: never 9200, never 9297-9299.
        assert_eq!(FIRST_AUTO_PORT, 9300);
    }

    #[test]
    fn the_node_config_is_accepted_by_the_real_config_loader_and_allows_the_packs_dir() {
        let hub = tempfile::tempdir().unwrap();
        let path = write_node_config(hub.path()).unwrap();
        let cfg = xerj_common::config::Config::load(&path).expect("node.toml loads");
        let packs = hub.path().join("packs");
        assert_eq!(
            cfg.limits.snapshot_repo_allowlist,
            [packs.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn the_start_lock_serialises_starters_and_is_free_the_moment_its_holder_lets_go() {
        let hub = tempfile::tempdir().unwrap();
        let first = NodeLock::acquire(hub.path(), Duration::from_secs(1)).unwrap();
        let err = NodeLock::acquire(hub.path(), Duration::from_millis(300))
            .err()
            .expect("held lock times out")
            .to_string();
        assert!(err.contains("another xerj process"), "{err}");
        assert!(
            err.contains(&hub.path().join(NODE_LOCK_FILE).display().to_string()),
            "the message names the lock: {err}"
        );
        assert!(
            err.contains(&format!("held by pid {}", std::process::id())),
            "and its holder: {err}"
        );
        drop(first);
        NodeLock::acquire(hub.path(), Duration::from_secs(1)).expect("free once released");
    }

    #[test]
    fn a_start_lock_that_cannot_be_taken_fails_instead_of_spinning() {
        // E7: a "stale" lock the old code could not remove sent it round a loop
        // that skipped both the deadline check and the sleep — 100% CPU forever.
        let hub = tempfile::tempdir().unwrap();
        let path = hub.path().join(NODE_LOCK_FILE);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("occupied"), b"x").unwrap();
        let t0 = Instant::now();
        let err = NodeLock::acquire(hub.path(), Duration::from_secs(30))
            .err()
            .expect("cannot be taken")
            .to_string();
        assert!(err.contains("start lock"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn connect_insecure_sends_no_credential() {
        let n = HubNode::connect_insecure("http://127.0.0.1:1/").unwrap();
        assert_eq!(n.url(), "http://127.0.0.1:1");
        assert!(n.auth.is_none());
    }

    #[test]
    fn the_record_round_trips_and_the_pid_is_the_commit_marker() {
        let hub = tempfile::tempdir().unwrap();
        let rec = NodeRecord {
            pid: 4242,
            port: Some(9411),
            start: Some("boot:777".into()),
        };
        write_record(hub.path(), &rec).unwrap();
        assert_eq!(read_record(hub.path()), Some(rec.clone()));
        // The plain-number files other readers (`hub status`) already parse.
        assert_eq!(
            fs::read_to_string(hub.path().join("node.pid")).unwrap(),
            "4242\n"
        );
        assert_eq!(
            fs::read_to_string(hub.path().join("node.port")).unwrap(),
            "9411\n"
        );
        // No temp files left behind.
        let mut names: Vec<String> = fs::read_dir(hub.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["node.pid", "node.port", "node.start"]);

        // Port and start time without a PID are not a record yet ...
        fs::remove_file(hub.path().join("node.pid")).unwrap();
        assert_eq!(read_record(hub.path()), None);
        // ... and a reader that sees no PID must not delete them: they may belong
        // to a record being committed right now.
        reap_stale(hub.path());
        assert!(hub.path().join("node.port").exists());
        assert!(hub.path().join("node.start").exists());
        // The next start (under the lock) clears the debris.
        clear_record(hub.path());
        assert!(!hub.path().join("node.port").exists());
    }

    #[test]
    fn a_failing_starter_only_clears_its_own_record() {
        let hub = tempfile::tempdir().unwrap();
        let rec = |pid| NodeRecord {
            pid,
            port: Some(9411),
            start: None,
        };
        write_record(hub.path(), &rec(11)).unwrap();
        clear_record_for(hub.path(), 99); // somebody else's record: untouched
        assert_eq!(read_pid(hub.path()), Some(11));
        clear_record_for(hub.path(), 11);
        assert_eq!(read_pid(hub.path()), None);
    }

    #[test]
    fn a_dead_nodes_record_is_cleared_only_if_it_is_still_the_record_that_was_examined() {
        // Examining a record and deleting it are two steps. If a starter replaced
        // it with the record of the node it just started in between, deleting that
        // one is the hub losing track of a live node.
        let hub = tempfile::tempdir().unwrap();
        let old = NodeRecord {
            pid: 11,
            port: Some(9411),
            start: Some("t1".into()),
        };
        let new = NodeRecord {
            pid: 22,
            port: Some(9414),
            start: Some("t2".into()),
        };
        write_record(hub.path(), &new).unwrap();
        clear_record_if(hub.path(), &old);
        assert_eq!(
            read_record(hub.path()),
            Some(new.clone()),
            "the new record stays"
        );
        clear_record_if(hub.path(), &new);
        assert_eq!(read_record(hub.path()), None, "the examined one goes");
    }

    #[test]
    fn a_node_record_needs_a_port_to_be_written() {
        let hub = tempfile::tempdir().unwrap();
        let rec = NodeRecord {
            pid: 1,
            port: None,
            start: None,
        };
        assert!(write_record(hub.path(), &rec).is_err());
        assert!(!hub.path().join("node.pid").exists());
    }

    #[test]
    fn start_tokens_compare_by_ticks_when_only_one_side_could_read_the_boot_id() {
        let boot = "1312a4f3-8f1a-4eb2-bc1b-048faa1cd5e9";
        let other_boot = "ffffffff-8f1a-4eb2-bc1b-048faa1cd5e9";
        let full = format!("{boot}:40311205");
        // Same process, same reader.
        assert!(start_tokens_match(&full, &full));
        // A different start time is a different process, whatever the boot id says.
        assert!(!start_tokens_match(&full, &format!("{boot}:40311206")));
        // Another boot: a PID from before a reboot is not the node.
        assert!(!start_tokens_match(
            &full,
            &format!("{other_boot}:40311205")
        ));
        // One side without the boot id (a sandboxed /proc/sys): the ticks decide, in
        // both directions — it must not read as "a different process".
        assert!(start_tokens_match(&full, "40311205"));
        assert!(start_tokens_match("40311205", &full));
        assert!(!start_tokens_match(&full, "40311206"));
        assert!(!start_tokens_match("40311206", &full));
        // Other platforms' tokens are opaque: equal or different, nothing in between.
        assert!(start_tokens_match(
            "Tue Oct 14 13:55:12 2026",
            "Tue Oct 14 13:55:12 2026"
        ));
        assert!(!start_tokens_match(
            "Tue Oct 14 13:55:12 2026",
            "Tue Oct 14 13:55:13 2026"
        ));
        assert!(!start_tokens_match(
            "132450000000000000",
            "132450000000000001"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_stat_parsing_survives_a_command_name_with_spaces_and_parens() {
        // `pid (comm) S ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt
        //  utime stime cutime cstime priority nice threads itrealvalue starttime …`
        let stat = "1234 (my (odd) name) S 1 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 3 0 987654 1000 100";
        assert_eq!(parse_proc_stat(stat), Some(('S', 987_654)));
        let zombie = "55 (x) Z 1 55 55 0 -1 4 0 0 0 0 0 0 0 0 20 0 1 0 42 0 0";
        assert_eq!(parse_proc_stat(zombie), Some(('Z', 42)));
        assert_eq!(parse_proc_stat("garbage"), None);
        assert_eq!(
            parse_proc_stat("1 (x) S 1 2"),
            None,
            "too short to hold starttime"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_process_has_a_stable_start_token_that_differs_between_processes() {
        let me = std::process::id();
        let a = proc_start_token(me).expect("this process has a start time");
        assert_eq!(proc_start_token(me), Some(a.clone()), "stable");
        // The token's resolution is the kernel's clock tick (10 ms): two processes
        // that start within one tick share a token — harmless for telling a recycled
        // PID from the node (PIDs do not recycle inside 10 ms), but a test that
        // starts one right after another must leave a tick between them.
        std::thread::sleep(Duration::from_millis(50));
        let mut other = Command::new("sleep").arg("5").spawn().unwrap();
        let b = proc_start_token(other.id()).expect("a child has one too");
        assert_ne!(a, b);
        other.kill().unwrap();
        other.wait().unwrap();
        assert_eq!(
            proc_start_token(other.id()),
            None,
            "a reaped process has none"
        );
    }

    // The process-level tests drive real child processes through `sh`, so they
    // are unix-only; the Windows paths are compiled but not exercised here.
    #[cfg(unix)]
    mod processes {
        use super::*;
        use std::process::Child;

        /// A long-lived `sh` whose command line carries `--data-dir <dir>` — what
        /// `identify_node` looks for. The trailing `; true` keeps `sh` itself
        /// alive (a lone command may be exec'd, replacing the command line).
        fn spawn_node_like(dir: &Path) -> Child {
            Command::new("sh")
                .args(["-c", "sleep 30; true", "node", "--data-dir"])
                .arg(dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }

        /// A process of the same program but a different command line — the
        /// "dummy process named like the binary" that must survive.
        fn spawn_decoy() -> Child {
            Command::new("sh")
                .args([
                    "-c",
                    "sleep 30; true",
                    "decoy",
                    "--data-dir",
                    "/elsewhere/node",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        }

        fn alive(child: &mut Child) -> bool {
            child.try_wait().unwrap().is_none()
        }

        /// Record `pid` the way a starter does, start time included.
        fn record(hub: &Path, pid: u32, port: Option<u16>) {
            let rec = NodeRecord {
                pid,
                port: Some(port.unwrap_or(9301)),
                start: proc_start_token(pid),
            };
            write_record(hub, &rec).unwrap();
            if port.is_none() {
                let _ = fs::remove_file(hub.join(NODE_PORT_FILE));
            }
        }

        /// Record `pid` with no start time (what an older record, or a platform
        /// that cannot read one, looks like): the command line alone decides.
        fn record_without_start(hub: &Path, pid: u32) {
            fs::write(hub.join(NODE_PID_FILE), format!("{pid}\n")).unwrap();
            fs::write(hub.join(NODE_PORT_FILE), "9301\n").unwrap();
        }

        fn identify_node(hub: &Path, pid: u32) -> Identity {
            read_record(hub)
                .filter(|r| r.pid == pid)
                .map(|r| identify_record(hub, &r))
                .unwrap_or_else(|| {
                    let needles = node_needles(hub);
                    let refs: Vec<&str> = needles.iter().map(String::as_str).collect();
                    identify(pid, &refs)
                })
        }

        fn quick_stop(hub: &Path) -> Result<StopOutcome> {
            stop_node_with(hub, Duration::from_millis(400), Duration::from_secs(3))
        }

        #[test]
        fn stop_signals_exactly_the_recorded_pid_and_spares_a_look_alike() {
            let hub = tempfile::tempdir().unwrap();
            let dir = node_data_dir(hub.path());
            let mut node = spawn_node_like(&dir);
            let mut decoy = spawn_decoy();
            record(hub.path(), node.id(), Some(9301));
            assert_eq!(identify_node(hub.path(), node.id()), Identity::Ours);
            assert_eq!(identify_node(hub.path(), decoy.id()), Identity::NotOurs);

            let outcome = stop_node(hub.path()).unwrap();
            assert_eq!(outcome, StopOutcome::Stopped { pid: node.id() });
            // The recorded process is dead (reap it) ...
            let status = node.wait().unwrap();
            assert!(!status.success(), "terminated by a signal: {status:?}");
            // ... the look-alike is untouched, and the records are gone.
            assert!(alive(&mut decoy), "the decoy must survive `hub stop`");
            assert!(!hub.path().join(NODE_PID_FILE).exists());
            assert!(!hub.path().join(NODE_PORT_FILE).exists());
            assert!(!hub.path().join(NODE_START_FILE).exists());
            decoy.kill().unwrap();
            decoy.wait().unwrap();
        }

        #[test]
        fn a_recycled_pid_is_never_signalled() {
            let hub = tempfile::tempdir().unwrap();
            // The record names a live process that is NOT this hub's node.
            let mut stranger = spawn_decoy();
            record(hub.path(), stranger.id(), Some(9301));
            let outcome = stop_node(hub.path()).unwrap();
            assert_eq!(outcome, StopOutcome::StaleRemoved { pid: stranger.id() });
            assert!(
                alive(&mut stranger),
                "an unrelated process must not be killed"
            );
            assert!(
                !hub.path().join(NODE_PID_FILE).exists(),
                "the stale record goes"
            );
            stranger.kill().unwrap();
            stranger.wait().unwrap();
        }

        #[test]
        fn a_pid_that_started_at_another_time_is_not_the_node_whatever_its_command_line_says() {
            // PID reuse as the OS sees it: the command line looks exactly right
            // (here it IS a node-like process), but the process is not the one
            // that was recorded — its start time differs.
            let hub = tempfile::tempdir().unwrap();
            let mut lookalike = spawn_node_like(&node_data_dir(hub.path()));
            let rec = NodeRecord {
                pid: lookalike.id(),
                port: Some(9301),
                start: Some("a-different-incarnation".into()),
            };
            write_record(hub.path(), &rec).unwrap();
            assert_eq!(identify_record(hub.path(), &rec), Identity::NotOurs);

            let outcome = stop_node(hub.path()).unwrap();
            assert_eq!(
                outcome,
                StopOutcome::StaleRemoved {
                    pid: lookalike.id()
                }
            );
            assert!(
                alive(&mut lookalike),
                "a process with the right command line and the wrong start time lives"
            );
            lookalike.kill().unwrap();
            lookalike.wait().unwrap();
        }

        #[test]
        fn the_start_time_of_the_real_node_matches_so_it_is_still_stopped() {
            let hub = tempfile::tempdir().unwrap();
            let mut node = spawn_node_like(&node_data_dir(hub.path()));
            record(hub.path(), node.id(), Some(9301));
            let rec = read_record(hub.path()).unwrap();
            assert!(rec.start.is_some(), "a start time was recorded");
            assert_eq!(identify_record(hub.path(), &rec), Identity::Ours);
            assert!(matches!(
                quick_stop(hub.path()).unwrap(),
                StopOutcome::Stopped { .. }
            ));
            node.wait().unwrap();
        }

        #[test]
        fn a_node_that_execs_into_another_program_after_sigterm_is_not_sigkilled() {
            // E3. Between the first identity check and the SIGKILL escalation the
            // recorded PID stops being the node: here the node `exec`s `sleep 600`
            // when it is told to terminate — same PID, a different program, i.e. an
            // unrelated process that now owns the number in `node.pid`. The old
            // `hub stop` kept polling for "Gone", saw "not ours" forever, and after
            // ten seconds SIGKILLed whatever held the PID.
            let hub = tempfile::tempdir().unwrap();
            let dir = node_data_dir(hub.path());
            let mut node = Command::new("sh")
                .args([
                    "-c",
                    r#"trap 'exec sleep 600' TERM; while :; do sleep 0.2; done"#,
                    "node",
                    "--data-dir",
                ])
                .arg(&dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            // Let the shell install its trap before TERM arrives.
            std::thread::sleep(Duration::from_millis(300));
            record(hub.path(), node.id(), Some(9301));
            assert_eq!(identify_node(hub.path(), node.id()), Identity::Ours);

            let t0 = Instant::now();
            // A generous TERM grace: the answer must come from "it is no longer the
            // node", not from the wait running out and SIGKILL following.
            let outcome =
                stop_node_with(hub.path(), Duration::from_secs(10), Duration::from_secs(3))
                    .unwrap();
            assert_eq!(outcome, StopOutcome::Stopped { pid: node.id() });
            assert!(
                t0.elapsed() < Duration::from_secs(9),
                "returned as soon as the node was no longer the node, not after the grace: {:?}",
                t0.elapsed()
            );
            // The process that took over the PID is still running: nobody killed it.
            assert!(
                alive(&mut node),
                "the program that replaced the node must not be SIGKILLed"
            );
            let cmd = fs::read(format!("/proc/{}/cmdline", node.id()))
                .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
                .unwrap_or_default();
            if Path::new("/proc/self/stat").exists() {
                assert!(
                    cmd.contains("sleep 600"),
                    "it really is `sleep 600` now: {cmd:?}"
                );
            }
            assert!(!hub.path().join(NODE_PID_FILE).exists(), "record cleared");
            node.kill().unwrap();
            node.wait().unwrap();
        }

        #[test]
        fn a_node_that_ignores_sigterm_is_escalated_to_sigkill_while_it_is_still_the_node() {
            let hub = tempfile::tempdir().unwrap();
            let dir = node_data_dir(hub.path());
            let mut node = Command::new("sh")
                .args([
                    "-c",
                    "trap '' TERM; while :; do sleep 0.1; done",
                    "node",
                    "--data-dir",
                ])
                .arg(&dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(300));
            record(hub.path(), node.id(), Some(9301));

            let outcome = quick_stop(hub.path()).unwrap();
            assert_eq!(outcome, StopOutcome::Stopped { pid: node.id() });
            let status = node.wait().unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
            assert!(!hub.path().join(NODE_PID_FILE).exists());
        }

        #[test]
        fn stop_with_nothing_recorded_or_a_dead_pid_is_a_clean_no_op() {
            let hub = tempfile::tempdir().unwrap();
            assert_eq!(stop_node(hub.path()).unwrap(), StopOutcome::NothingRecorded);

            let mut gone = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
            let pid = gone.id();
            gone.wait().unwrap();
            record_without_start(hub.path(), pid);
            assert_eq!(identify_node(hub.path(), pid), Identity::Gone);
            assert_eq!(
                stop_node(hub.path()).unwrap(),
                StopOutcome::StaleRemoved { pid }
            );
            assert!(!hub.path().join(NODE_PID_FILE).exists());
        }

        #[test]
        fn a_zombie_counts_as_gone() {
            let hub = tempfile::tempdir().unwrap();
            // Exits at once; not `wait`ed, so it stays a zombie until reaped.
            let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(identify_node(hub.path(), child.id()), Identity::Gone);
            child.wait().unwrap();
        }

        #[test]
        fn identify_matches_every_needle_in_the_command_line() {
            let me = std::process::id();
            assert_eq!(identify(me, &["xerj_hub"]), Identity::Ours);
            assert_eq!(
                identify(me, &["xerj_hub", "no-such-argument"]),
                Identity::NotOurs
            );
        }

        #[test]
        fn status_forgets_a_dead_node_but_keeps_a_live_one() {
            let hub = tempfile::tempdir().unwrap();
            let mut dead = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
            let dead_pid = dead.id();
            dead.wait().unwrap();
            record_without_start(hub.path(), dead_pid);
            reap_stale(hub.path());
            assert!(!hub.path().join(NODE_PID_FILE).exists());
            assert!(!hub.path().join(NODE_PORT_FILE).exists());

            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            record(hub.path(), live.id(), Some(9301));
            reap_stale(hub.path());
            assert!(hub.path().join(NODE_PID_FILE).exists());
            live.kill().unwrap();
            live.wait().unwrap();
        }

        #[test]
        fn a_recorded_live_node_that_answers_is_reused_not_restarted() {
            let hub = tempfile::tempdir().unwrap();
            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            let port = fake_node_with_pid(live.id());
            record(hub.path(), live.id(), Some(port));
            // The default-ish URL is silent (a closed loopback port), so the
            // recorded node is the one used.
            let r = ensure_node(hub.path(), Some(&silent_url()), &forbidden_launcher()).unwrap();
            assert_eq!(r.url, format!("http://127.0.0.1:{port}"));
            assert!(r.managed && !r.started_now);
            live.kill().unwrap();
            live.wait().unwrap();
        }

        #[test]
        fn something_else_on_the_recorded_port_is_not_taken_for_the_recorded_node() {
            // The node is alive but has not bound its port yet, and another process
            // answers there: it is a booting node to wait for, not a healthy one.
            let hub = tempfile::tempdir().unwrap();
            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            let impostor = fake_node_with_pid(live.id() + 1_000_000);
            record(hub.path(), live.id(), Some(impostor));
            let err = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &forbidden_launcher(),
                Duration::from_millis(600),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("no second node was started"), "{err}");
            assert!(alive(&mut live) && hub.path().join(NODE_PID_FILE).exists());
            live.kill().unwrap();
            live.wait().unwrap();
        }

        #[test]
        fn a_slow_node_is_waited_for_not_replaced() {
            // E5/E4: a node of ours is alive but does not answer within the probe
            // timeout (a stall, a slow boot, a starter that died mid-boot). The old
            // code could not see it without a recorded port, started a second node
            // over its data directory, then deleted the first one's record.
            let hub = tempfile::tempdir().unwrap();
            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            let port = silent_port();
            record(hub.path(), live.id(), Some(port));
            let pid = live.id();
            // It starts answering a little later.
            let late = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(700));
                let l = TcpListener::bind(("127.0.0.1", port)).expect("port still free");
                for conn in l.incoming().take(64) {
                    let Ok(mut conn) = conn else { continue };
                    let mut buf = [0u8; 2048];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let body = if req.contains("/_nodes") {
                        format!(r#"{{"nodes":{{"n":{{"process":{{"id":{pid}}}}}}}}}"#)
                    } else {
                        r#"{"status":"green"}"#.to_string()
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = conn.write_all(reply.as_bytes());
                }
            });
            let r = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &forbidden_launcher(), // starting a second node would fail the test
                Duration::from_secs(20),
            )
            .unwrap();
            assert_eq!(r.url, format!("http://127.0.0.1:{port}"));
            assert!(r.managed && !r.started_now, "adopted, not started");
            assert!(alive(&mut live));
            assert_eq!(read_pid(hub.path()), Some(live.id()), "record untouched");
            live.kill().unwrap();
            live.wait().unwrap();
            drop(late); // the listener thread ends with the test binary
        }

        #[test]
        fn a_live_node_that_never_answers_blocks_a_second_start_and_keeps_its_record() {
            let hub = tempfile::tempdir().unwrap();
            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            record(hub.path(), live.id(), Some(silent_port()));
            let t0 = Instant::now();
            let err = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &forbidden_launcher(),
                Duration::from_millis(700),
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("xerj hub stop"),
                "tells the user what to do: {err}"
            );
            assert!(err.contains(&live.id().to_string()), "names the pid: {err}");
            assert!(t0.elapsed() < Duration::from_secs(10));
            assert!(alive(&mut live), "the live node was not touched");
            assert_eq!(read_pid(hub.path()), Some(live.id()), "its record survives");
            // `hub stop` still finds and stops it — the wedge the old code left.
            assert!(matches!(
                quick_stop(hub.path()).unwrap(),
                StopOutcome::Stopped { .. }
            ));
            live.wait().unwrap();
        }

        #[test]
        fn start_node_itself_refuses_to_run_over_a_live_record() {
            // Whatever the caller checked, spawning is guarded again where it happens:
            // a live record is never overwritten and a second node never started.
            let hub = tempfile::tempdir().unwrap();
            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            record(hub.path(), live.id(), Some(silent_port()));
            let before = read_record(hub.path()).unwrap();
            let err = start_node(
                hub.path(),
                &forbidden_launcher(), // spawning would fail loudly with a different error
                &silent_url(),
                Duration::from_millis(200),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("not starting another"), "{err}");
            assert_eq!(
                read_record(hub.path()),
                Some(before),
                "the record is intact"
            );
            assert!(alive(&mut live));
            live.kill().unwrap();
            live.wait().unwrap();

            // A record of a node that is gone is debris: it is replaced.
            let mut dead = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
            let dead_pid = dead.id();
            dead.wait().unwrap();
            let hub2 = tempfile::tempdir().unwrap();
            record_without_start(hub2.path(), dead_pid);
            let err = start_node(
                hub2.path(),
                &forbidden_launcher(),
                &silent_url(),
                Duration::from_millis(200),
            )
            .unwrap_err();
            assert!(
                format!("{err:#}").contains("start the local node"),
                "got as far as trying to spawn: {err:#}"
            );
            assert!(!hub2.path().join(NODE_PID_FILE).exists());
        }

        #[test]
        fn a_record_without_a_port_still_blocks_a_second_start_and_is_stoppable() {
            let hub = tempfile::tempdir().unwrap();
            let mut live = spawn_node_like(&node_data_dir(hub.path()));
            record(hub.path(), live.id(), None);
            let err = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &forbidden_launcher(),
                Duration::from_millis(300),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("port was not recorded"), "{err}");
            assert!(matches!(
                quick_stop(hub.path()).unwrap(),
                StopOutcome::Stopped { .. }
            ));
            live.wait().unwrap();
        }

        #[test]
        fn a_node_that_dies_while_booting_leaves_no_record_and_shows_its_log() {
            let hub = tempfile::tempdir().unwrap();
            // `sh -c 'echo boom; exit 3' sh <node flags…>` — exits straight away.
            let launcher = NodeLauncher {
                exe: PathBuf::from("sh"),
                pre_args: vec!["-c".into(), "echo boom-in-log; exit 3".into(), "sh".into()],
            };
            let err = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &launcher,
                Duration::from_secs(10),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("exited"), "{err}");
            assert!(
                err.contains("boom-in-log"),
                "the node's log is shown: {err}"
            );
            assert!(!hub.path().join(NODE_PID_FILE).exists());
            assert!(!hub.path().join(NODE_PORT_FILE).exists());
            assert!(!hub.path().join(NODE_START_FILE).exists());
            FileLock::acquire(&hub.path().join(NODE_LOCK_FILE), Duration::ZERO)
                .expect("the start lock is released");
        }

        #[test]
        fn a_node_that_never_gets_healthy_is_killed_and_forgotten_at_the_timeout() {
            let hub = tempfile::tempdir().unwrap();
            let launcher = NodeLauncher {
                exe: PathBuf::from("sh"),
                pre_args: vec!["-c".into(), "sleep 30; true".into(), "sh".into()],
            };
            let t0 = Instant::now();
            let err = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &launcher,
                Duration::from_millis(800),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("did not report a healthy cluster"), "{err}");
            assert!(t0.elapsed() < Duration::from_secs(10));
            assert!(!hub.path().join(NODE_PID_FILE).exists());
            assert!(!hub.path().join(NODE_PORT_FILE).exists());
        }

        #[test]
        fn the_node_is_recorded_before_it_is_healthy_pid_port_and_start_time_together() {
            // The record must exist while the starter is still waiting for the node:
            // that is what lets a killed or timed-out starter leave something to find.
            let hub = tempfile::tempdir().unwrap();
            let hub_path = hub.path().to_path_buf();
            let launcher = NodeLauncher {
                exe: PathBuf::from("sh"),
                pre_args: vec![
                    "-c".into(),
                    // a node-like process: its command line carries the data dir
                    "sleep 30; true".into(),
                    "sh".into(),
                ],
            };
            let url = silent_url();
            let starter = std::thread::spawn(move || {
                ensure_node_with(&hub_path, Some(&url), &launcher, Duration::from_secs(4))
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            let seen = loop {
                if let Some(rec) = read_record(hub.path()) {
                    break Some(rec);
                }
                if Instant::now() > deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let rec = seen.expect("a record appears while the node is still booting");
            assert!(rec.port.is_some_and(|p| p >= FIRST_AUTO_PORT), "{rec:?}");
            assert!(
                rec.start.is_some(),
                "the start time is recorded with the pid"
            );
            assert!(!starter.is_finished(), "the starter is still waiting");
            let result = starter.join().unwrap();
            assert!(result.is_err(), "it never became healthy");
            assert_eq!(
                read_pid(hub.path()),
                None,
                "and the failed start cleaned up"
            );
            assert_eq!(
                proc_state(rec.pid),
                ProcState::Gone,
                "the child it started was killed, not left running"
            );
        }

        #[test]
        fn a_node_is_killed_when_its_record_cannot_be_written() {
            // E12: spawn, then write node.pid. With a directory in the way the
            // write failed and `?` returned before any cleanup: a live node, no
            // record, and `hub stop` saying there is nothing to stop.
            let hub = tempfile::tempdir().unwrap();
            fs::create_dir(hub.path().join(NODE_PID_FILE)).unwrap();
            let pid_file = hub.path().join("spawned.pid");
            let launcher = NodeLauncher {
                exe: PathBuf::from("sh"),
                pre_args: vec![
                    "-c".into(),
                    // $1 = file to write our pid into; the node flags follow it
                    r#"echo $$ > "$1"; sleep 30; true"#.into(),
                    "sh".into(),
                    pid_file.to_string_lossy().into_owned(),
                ],
            };
            let err = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &launcher,
                Duration::from_secs(5),
            )
            .unwrap_err();
            assert!(
                format!("{err:#}").contains("record the local node"),
                "{err:#}"
            );
            // The shell may take a moment to have written its pid; it was killed
            // either way, and what it wrote names a process that is gone.
            std::thread::sleep(Duration::from_millis(200));
            if let Ok(text) = fs::read_to_string(&pid_file) {
                let pid: u32 = text.trim().parse().unwrap();
                assert_eq!(
                    proc_state(pid),
                    ProcState::Gone,
                    "the node that could not be recorded must not be left running"
                );
            }
            assert!(!hub.path().join(NODE_PORT_FILE).exists(), "no half record");
            assert!(!hub.path().join(NODE_START_FILE).exists());
        }

        // ── answering as the child: the port race ───────────────────────────

        #[test]
        fn a_foreign_responder_on_the_port_is_not_accepted_as_the_node_we_started() {
            // E6: `pick_free_port` drops its sockets before the child binds, so
            // something else can take the port. A bare "health is green" accepted
            // it and recorded the impostor's port as the node's.
            let mut child = Command::new("sleep").arg("30").spawn().unwrap();
            let log = tempfile::NamedTempFile::new().unwrap();
            let impostor = fake_node_with_pid(child.id() + 1_000_000);
            let url = format!("http://127.0.0.1:{impostor}");
            let failure =
                wait_until_healthy(&mut child, &url, Duration::from_millis(700), log.path(), 0)
                    .expect_err("an answer from another process must not count");
            assert!(
                failure
                    .message
                    .contains("not by the local node this tool started"),
                "{}",
                failure.message
            );
            assert!(!failure.port_taken);
            // An answerer with no pid at all (a bare health endpoint) is no better.
            let bare = fake_node("200 OK", r#"{"status":"green"}"#);
            let failure = wait_until_healthy(
                &mut child,
                &format!("http://127.0.0.1:{bare}"),
                Duration::from_millis(500),
                log.path(),
                0,
            )
            .expect_err("health without a pid is not proof");
            assert!(
                failure.message.contains("does not report its pid"),
                "{}",
                failure.message
            );

            // The right process answering is accepted.
            let genuine = fake_node_with_pid(child.id());
            wait_until_healthy(
                &mut child,
                &format!("http://127.0.0.1:{genuine}"),
                Duration::from_secs(5),
                log.path(),
                0,
            )
            .expect("the child's own answer is accepted");
            child.kill().unwrap();
            child.wait().unwrap();
        }

        #[test]
        fn a_dead_child_is_never_reported_healthy_even_if_something_answers() {
            let mut child = Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap();
            child.wait().unwrap();
            let log = tempfile::NamedTempFile::new().unwrap();
            fs::write(
                log.path(),
                "bind 127.0.0.1:9301: Address already in use (os error 98)\n",
            )
            .unwrap();
            let port = fake_node_with_pid(child.id());
            let failure = wait_until_healthy(
                &mut child,
                &format!("http://127.0.0.1:{port}"),
                Duration::from_secs(5),
                log.path(),
                0,
            )
            .expect_err("the child has exited");
            assert!(failure.message.contains("exited"), "{}", failure.message);
            assert!(failure.port_taken, "the log says the port was taken");
        }

        #[test]
        fn port_taken_is_read_from_this_attempts_log_only() {
            let log = tempfile::NamedTempFile::new().unwrap();
            fs::write(
                log.path(),
                "attempt 1: Address already in use (os error 98)\n",
            )
            .unwrap();
            let from = fs::metadata(log.path()).unwrap().len();
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(log.path())
                .unwrap();
            writeln!(f, "attempt 2: some other failure").unwrap();
            assert!(looks_like_port_taken(&log_tail_since(log.path(), 0, 20)));
            assert!(
                !looks_like_port_taken(&log_tail_since(log.path(), from, 20)),
                "an earlier attempt's EADDRINUSE must not explain this attempt"
            );
            for text in [
                "Address already in use (os error 98)",
                "AddrInUse",
                "Only one usage of each socket address (os error 10048)",
                "os error 48",
            ] {
                assert!(looks_like_port_taken(text), "{text}");
            }
            assert!(!looks_like_port_taken(
                "no such file or directory (os error 2)"
            ));
        }

        /// Not a test: a stand-in `xerj` node, started as
        /// `<this binary> --ignored --exact node::tests::processes::fake_node_main
        ///  --nocapture -- --insecure --bind 127.0.0.1 --port P --data-dir D --config C`.
        /// It answers `/_cluster/health` and `/_nodes` (with its own pid) on `P`.
        /// Test hook: while `<hub>/fail-once` exists it removes the file and
        /// exits the way a real node does when its port was taken.
        #[test]
        #[ignore = "child-process entry point, started by the port-race test"]
        fn fake_node_main() {
            let args: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
            let value = |flag: &str| {
                args.iter()
                    .position(|a| a == flag)
                    .and_then(|i| args.get(i + 1))
                    .cloned()
            };
            let Some(port) = value("--port").and_then(|p| p.parse::<u16>().ok()) else {
                return; // run by hand (`--ignored`) without flags: nothing to do
            };
            let data_dir = PathBuf::from(value("--data-dir").expect("--data-dir"));
            let hub = data_dir.parent().expect("hub dir").to_path_buf();
            let marker = hub.join("fail-once");
            if marker.exists() {
                let _ = fs::write(hub.join("first-port"), port.to_string());
                let _ = fs::remove_file(&marker);
                eprintln!(
                    "Error: ES-compat: bind 127.0.0.1:{port}\n\nCaused by:\n    Address already in use (os error 98)"
                );
                std::process::exit(1);
            }
            let pid = std::process::id();
            let listener = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| {
                eprintln!("Error: ES-compat: bind 127.0.0.1:{port}: {e}");
                std::process::exit(1)
            });
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 2048];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let body = if req.contains("/_nodes") {
                        format!(r#"{{"nodes":{{"fake":{{"process":{{"id":{pid}}}}}}}}}"#)
                    } else {
                        r#"{"status":"green"}"#.to_string()
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = conn.write_all(reply.as_bytes());
                });
            }
        }

        fn fake_node_launcher() -> NodeLauncher {
            NodeLauncher {
                exe: std::env::current_exe().unwrap(),
                pre_args: [
                    "--ignored",
                    "--exact",
                    "node::tests::processes::fake_node_main",
                    "--nocapture",
                    "--",
                ]
                .map(String::from)
                .to_vec(),
            }
        }

        #[test]
        fn a_stand_in_node_is_started_recorded_reused_and_stopped_end_to_end() {
            let hub = tempfile::tempdir().unwrap();
            let r = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &fake_node_launcher(),
                Duration::from_secs(30),
            )
            .unwrap();
            assert!(r.managed && r.started_now, "{r:?}");
            let rec = read_record(hub.path()).expect("recorded");
            assert_eq!(r.url, node_url(rec.port.unwrap()));
            assert_eq!(identify_record(hub.path(), &rec), Identity::Ours);

            // A second caller finds it and starts nothing.
            let again = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &forbidden_launcher(),
                Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(again.url, r.url);
            assert!(again.managed && !again.started_now);

            // And hub stop stops exactly it.
            assert_eq!(
                stop_node(hub.path()).unwrap(),
                StopOutcome::Stopped { pid: rec.pid }
            );
            assert!(probe(&r.url, Duration::from_secs(1)).is_none());
        }

        #[test]
        fn a_node_that_loses_its_port_race_is_retried_on_a_fresh_port() {
            // E6c: the port the starter picked is taken before the node binds it; the
            // node dies on EADDRINUSE. The starter must notice, not report failure
            // (or, worse, success for whatever holds the port) — it picks again.
            let hub = tempfile::tempdir().unwrap();
            fs::write(hub.path().join("fail-once"), "x").unwrap();
            let r = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &fake_node_launcher(),
                Duration::from_secs(30),
            )
            .expect("the second attempt, on a fresh port, succeeds");
            assert!(r.managed && r.started_now);
            let first: u16 = fs::read_to_string(hub.path().join("first-port"))
                .expect("the first attempt ran and failed")
                .trim()
                .parse()
                .unwrap();
            let rec = read_record(hub.path()).unwrap();
            assert_ne!(rec.port, Some(first), "a different port was used");
            assert!(
                rec.port.unwrap() > first,
                "searched upwards from the failed one"
            );
            assert_eq!(identify_record(hub.path(), &rec), Identity::Ours);
            stop_node(hub.path()).unwrap();
        }

        #[test]
        fn the_node_is_started_with_the_plans_flags_plus_loopback_bind_and_config() {
            let hub = tempfile::tempdir().unwrap();
            // `printf '%s\n' "$@"` writes the node's argv to its log, then fails
            // fast so the test does not wait on a healthy cluster.
            let launcher = NodeLauncher {
                exe: PathBuf::from("sh"),
                pre_args: vec![
                    "-c".into(),
                    "printf '%s\\n' \"$@\"; exit 4".into(),
                    "sh".into(),
                ],
            };
            let _ = ensure_node_with(
                hub.path(),
                Some(&silent_url()),
                &launcher,
                Duration::from_secs(10),
            );
            let log = fs::read_to_string(hub.path().join(NODE_LOG_FILE)).unwrap();
            let argv: Vec<&str> = log.lines().collect();
            assert_eq!(argv[0], "--insecure");
            assert_eq!(&argv[1..3], ["--bind", "127.0.0.1"]);
            assert_eq!(argv[3], "--port");
            let port: u16 = argv[4].parse().unwrap();
            assert!(
                port >= FIRST_AUTO_PORT,
                "port {port} is below the floor (never 9200, never 9297-9299)"
            );
            assert_eq!(argv[5], "--data-dir");
            assert_eq!(argv[6], node_data_dir(hub.path()).to_string_lossy());
            assert_eq!(argv[7], "--config");
            assert_eq!(argv[8], hub.path().join("node.toml").to_string_lossy());
        }
    }
}
