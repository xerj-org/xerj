//! `xerj service install|uninstall|status` (#3267): run the local node under
//! the user's own service manager, so it comes back after a crash, a logout
//! or a reboot.
//!
//! * Linux: a systemd `--user` unit in `$XDG_CONFIG_HOME/systemd/user`
//!   (`~/.config/systemd/user`), enabled and started with `systemctl --user`.
//! * macOS: a launchd LaunchAgent in `~/Library/LaunchAgents`, loaded with
//!   `launchctl bootstrap gui/<uid>`.
//! * Windows: nothing is installed; the Task Scheduler / `sc.exe` commands
//!   are printed instead.
//!
//! Every file written is under the user's home. Re-running `install` with
//! the same flags changes nothing; with new flags it rewrites the unit and
//! restarts the node. `--dry-run` prints the unit and the commands and
//! touches nothing.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::localnode;
use crate::nodedir::{self, PidState};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
/// `status`: the unit is installed but the node is not running (LSB code).
pub const EXIT_NOT_RUNNING: i32 = 3;
/// `status`: no unit is installed (LSB "unknown").
pub const EXIT_NOT_INSTALLED: i32 = 4;

/// The data dir a service node uses when neither `--data-dir` nor a
/// `--config` file names one. `./data` (the bare server default) would land
/// in the service manager's working directory.
pub const DEFAULT_DATA_DIR_NAME: &str = "xerj-data";

pub const USAGE: &str = "\
xerj service — run the local node under your user's service manager, so it
survives the terminal closing, a crash, a logout and (with lingering on
Linux) a reboot.

USAGE:
    xerj service install   [--name N] [--dry-run] [SERVER FLAGS...]
    xerj service uninstall [--name N] [--dry-run]
    xerj service status    [--name N]

    SERVER FLAGS are the flags of a bare `xerj` start, e.g.
        xerj service install --insecure --data-dir ~/xerj-data --port 9200
    The data dir defaults to ~/xerj-data (not ./data) and is written into
    the unit as an absolute path. --detach is refused: the service manager
    keeps the node running.

    --name N     run a second node side by side: unit xerj-N.service
                 (launchd org.xerj.node-N). Letters, digits, '-' and '_'.
    --dry-run    print the unit file and the commands; write and start nothing.

WHERE:
    Linux    ~/.config/systemd/user/xerj.service    (systemctl --user)
             Starts at login. To start at boot with no login:
             loginctl enable-linger $USER
    macOS    ~/Library/LaunchAgents/org.xerj.node.plist   (launchctl)
    Windows  not installed by xerj; `install` prints the Task Scheduler
             command to run instead.

EXIT CODES:
    0  done (status: running)        2  usage error
    1  failed                        3  status: installed, not running
                                     4  status: not installed";

/// The service manager of the OS this binary was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    SystemdUser,
    Launchd,
    Windows,
}

impl Platform {
    pub fn for_this_os() -> Option<Platform> {
        if cfg!(target_os = "linux") {
            Some(Platform::SystemdUser)
        } else if cfg!(target_os = "macos") {
            Some(Platform::Launchd)
        } else if cfg!(windows) {
            Some(Platform::Windows)
        } else {
            None
        }
    }
}

/// What the service is called by each manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// systemd: `xerj.service` or `xerj-<name>.service`.
    pub unit: String,
    /// launchd: `org.xerj.node` or `org.xerj.node-<name>`.
    pub label: String,
    /// Windows Task Scheduler: `xerj-node` or `xerj-node-<name>`.
    pub task: String,
    /// `""` or ` --name <name>`, for the commands printed back to the user.
    pub flag: String,
}

impl Names {
    pub fn new(name: Option<&str>) -> Result<Names> {
        match name {
            None => Ok(Names {
                unit: "xerj.service".into(),
                label: "org.xerj.node".into(),
                task: "xerj-node".into(),
                flag: String::new(),
            }),
            Some(n) => {
                let ok = !n.is_empty()
                    && n.len() <= 64
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if !ok {
                    bail!("--name {n:?}: use 1-64 letters, digits, '-' or '_'");
                }
                Ok(Names {
                    unit: format!("xerj-{n}.service"),
                    label: format!("org.xerj.node-{n}"),
                    task: format!("xerj-node-{n}"),
                    flag: format!(" --name {n}"),
                })
            }
        }
    }
}

/// Where unit files go. Injectable so tests write into a temp dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDirs {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`.
    pub systemd_user: PathBuf,
    /// `~/Library/LaunchAgents`.
    pub launch_agents: PathBuf,
}

impl ServiceDirs {
    pub fn from_env(env: &dyn Fn(&str) -> Option<OsString>) -> Result<ServiceDirs> {
        let home = crate::home::home_dir_from(env).ok_or_else(|| {
            anyhow!("neither HOME nor USERPROFILE is set; cannot place a user service")
        })?;
        let config = env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        Ok(ServiceDirs {
            systemd_user: config.join("systemd").join("user"),
            launch_agents: home.join("Library").join("LaunchAgents"),
            home,
        })
    }

    /// The unit file for `platform`; `None` on Windows (nothing is written).
    pub fn unit_path(&self, platform: Platform, names: &Names) -> Option<PathBuf> {
        match platform {
            Platform::SystemdUser => Some(self.systemd_user.join(&names.unit)),
            Platform::Launchd => Some(self.launch_agents.join(format!("{}.plist", names.label))),
            Platform::Windows => None,
        }
    }
}

/// The node the unit starts: `exe args`, run in `data_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub exe: PathBuf,
    /// Server flags; always ends with `--data-dir <absolute data_dir>`.
    pub args: Vec<String>,
    pub data_dir: PathBuf,
}

impl Spec {
    pub fn insecure(&self) -> bool {
        self.args.iter().any(|a| a == "--insecure" || a == "-k")
    }

    /// The `--port` flag, when given.
    pub fn port_flag(&self) -> Option<u16> {
        let i = self.args.iter().rposition(|a| a == "--port")?;
        self.args.get(i + 1)?.parse().ok()
    }
}

fn lexical_absolute(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn expand_path(raw: &str, cwd: &Path, home: &Path) -> PathBuf {
    let p = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    };
    lexical_absolute(&p, cwd)
}

/// `server.data_dir` from a TOML config file, when the file sets it.
fn config_data_dir(path: &Path) -> Result<Option<String>> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let v: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parse config {}", path.display()))?;
    Ok(v.get("server")
        .and_then(|s| s.get("data_dir"))
        .and_then(|d| d.as_str())
        .map(str::to_string))
}

/// Turn the server flags typed after `xerj service install` into the flags
/// the unit runs: `--config` made absolute, every `--data-dir` replaced by
/// one absolute `--data-dir` at the end. The data dir is `--data-dir`, else
/// the config file's `server.data_dir` (relative to `cwd`), else
/// `~/xerj-data`.
pub fn resolve_spec(server_args: &[String], cwd: &Path, home: &Path, exe: &Path) -> Result<Spec> {
    let mut args = Vec::new();
    let mut data_dir: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut it = server_args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--detach" => bail!(
                "--detach does not go in a service: the service manager keeps the node \
                 running. Drop it"
            ),
            "--data-dir" | "-d" => {
                let v = it.next().ok_or_else(|| anyhow!("{a} needs a value"))?;
                data_dir = Some(expand_path(v, cwd, home));
            }
            "--config" | "-c" => {
                let v = it.next().ok_or_else(|| anyhow!("{a} needs a value"))?;
                let p = expand_path(v, cwd, home);
                args.push(a.clone());
                args.push(p.display().to_string());
                config = Some(p);
            }
            _ => args.push(a.clone()),
        }
    }
    let data_dir = match data_dir {
        Some(d) => d,
        None => match config
            .as_deref()
            .map(config_data_dir)
            .transpose()?
            .flatten()
        {
            Some(d) => expand_path(&d, cwd, home),
            None => home.join(DEFAULT_DATA_DIR_NAME),
        },
    };
    let ds = data_dir
        .to_str()
        .ok_or_else(|| anyhow!("data dir {} is not valid UTF-8", data_dir.display()))?
        .to_string();
    args.push("--data-dir".into());
    args.push(ds);
    Ok(Spec {
        exe: exe.to_path_buf(),
        args,
        data_dir,
    })
}

fn refuse_control(s: &str) -> Result<()> {
    if s.chars().any(|c| c.is_control()) {
        bail!("{s:?} contains a control character; refusing to write it into a unit");
    }
    Ok(())
}

/// One `ExecStart=` token: verbatim when plainly safe, else double-quoted with
/// systemd's escapes. Same rule as `xerj hub sync --install-timer`
/// (`xerj-hub/src/sync.rs` `systemd_exec_token`).
pub fn systemd_exec_token(s: &str) -> Result<String> {
    refuse_control(s)?;
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+:@,=".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        return Ok(s.to_string());
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| anyhow!("{} is not valid UTF-8", p.display()))
}

const SYSTEMD_DATA_DIR_TAG: &str = "# xerj-data-dir=";

/// The systemd `--user` unit. `Restart=on-failure` brings a crashed node
/// back; a clean stop (`systemctl --user stop`) stays stopped.
pub fn render_systemd_unit(spec: &Spec) -> Result<String> {
    let mut exec = vec![systemd_exec_token(path_str(&spec.exe)?)?];
    for a in &spec.args {
        exec.push(systemd_exec_token(a)?);
    }
    let dir = path_str(&spec.data_dir)?;
    refuse_control(dir)?;
    Ok(format!(
        "# Written by `xerj service install`. Re-run it to change the flags;\n\
         # remove it with `xerj service uninstall`.\n\
         {SYSTEMD_DATA_DIR_TAG}{dir}\n\
         [Unit]\n\
         Description=XERJ search node\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}\n\
         WorkingDirectory={}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         TimeoutStopSec=60\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exec.join(" "),
        dir.replace('%', "%%"),
    ))
}

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The launchd LaunchAgent. `RunAtLoad` starts it at login; `KeepAlive`
/// with `SuccessfulExit=false` restarts it after a crash but not after a
/// clean exit. Output goes to `<data_dir>/node.log`, like `xerj --detach`.
pub fn render_launchd_plist(spec: &Spec, label: &str) -> Result<String> {
    let mut argv = String::new();
    for a in std::iter::once(path_str(&spec.exe)?).chain(spec.args.iter().map(String::as_str)) {
        refuse_control(a)?;
        argv.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(a)));
    }
    let dir = path_str(&spec.data_dir)?;
    refuse_control(dir)?;
    let log = spec.data_dir.join(localnode::NODE_LOG_FILE);
    let log = xml_escape(path_str(&log)?);
    let dir = xml_escape(dir);
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Written by `xerj service install`. Remove it with `xerj service uninstall`. -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {argv}\
         \t</array>\n\
         \t<key>WorkingDirectory</key>\n\
         \t<string>{dir}</string>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<dict>\n\
         \t\t<key>SuccessfulExit</key>\n\
         \t\t<false/>\n\
         \t</dict>\n\
         \t<key>ThrottleInterval</key>\n\
         \t<integer>10</integer>\n\
         \t<key>StandardOutPath</key>\n\
         \t<string>{log}</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t<string>{log}</string>\n\
         </dict>\n\
         </plist>\n"
    ))
}

fn cmd_quote(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '"', '&', '|', '<', '>', '^']) {
        s.to_string()
    } else {
        format!("\\\"{}\\\"", s.replace('"', ""))
    }
}

/// What `xerj service install` prints on Windows, where it installs nothing.
pub fn render_windows_instructions(spec: &Spec, task: &str) -> String {
    let mut tr = vec![
        cmd_quote(&spec.exe.display().to_string()),
        "--detach".into(),
    ];
    tr.extend(spec.args.iter().map(|a| cmd_quote(a)));
    let tr = tr.join(" ");
    let dir = spec.data_dir.display().to_string();
    let log = format!(
        "{}\\{}",
        dir.trim_end_matches('\\'),
        localnode::NODE_LOG_FILE
    );
    format!(
        "xerj service: Windows has no per-user service manager that xerj drives yet.\n\
         Nothing was installed. To start this node at every logon, run these in cmd\n\
         (an elevated prompt if schtasks says \"Access is denied\"):\n\
         \n\
         \x20 schtasks /Create /TN {task} /SC ONLOGON /RL LIMITED /F /TR \"{tr}\"\n\
         \x20 schtasks /Run /TN {task}\n\
         \n\
         Check it:   schtasks /Query /TN {task}\n\
         Remove it:  schtasks /Delete /TN {task} /F   (then stop the node: taskkill /PID <pid>)\n\
         The node logs to {log}.\n\
         \n\
         sc.exe: `sc.exe create` only registers programs that answer the Windows Service\n\
         Control Manager. xerj does not, so a service made with sc.exe directly is stopped\n\
         after about 30 s (error 1053). Run xerj under a service wrapper if you need a\n\
         boot-time Windows service, or use the logon task above.\n"
    )
}

/// The data dir recorded in an installed unit, so `status` and `uninstall`
/// look at the node the unit runs.
pub fn data_dir_from_unit(platform: Platform, body: &str) -> Option<PathBuf> {
    match platform {
        Platform::SystemdUser => body
            .lines()
            .find_map(|l| l.strip_prefix(SYSTEMD_DATA_DIR_TAG))
            .map(PathBuf::from),
        Platform::Launchd => {
            let after = body.split("<key>WorkingDirectory</key>").nth(1)?;
            let start = after.find("<string>")? + "<string>".len();
            let end = after[start..].find("</string>")? + start;
            Some(PathBuf::from(xml_unescape(&after[start..end])))
        }
        Platform::Windows => None,
    }
}

/// One service-manager command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub argv: Vec<String>,
    /// `true` for a command whose failure is expected and harmless (unload
    /// a job that is not loaded, load one that already is).
    pub tolerate_failure: bool,
}

impl Step {
    fn new(argv: &[&str], tolerate_failure: bool) -> Step {
        Step {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            tolerate_failure,
        }
    }

    pub fn display(&self) -> String {
        self.argv
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("'{a}'")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How the unit on disk compares with the one `install` would write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    New,
    Changed,
    Unchanged,
}

impl Change {
    pub fn of(existing: Option<&str>, wanted: &str) -> Change {
        match existing {
            None => Change::New,
            Some(e) if e == wanted => Change::Unchanged,
            Some(_) => Change::Changed,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Change::New => "new",
            Change::Changed => "changed",
            Change::Unchanged => "unchanged",
        }
    }
}

/// The commands `install` runs after writing the unit. `start = false`
/// (another node already holds the data dir) registers without starting.
pub fn install_steps(
    platform: Platform,
    names: &Names,
    uid: u32,
    unit_path: &Path,
    change: Change,
    start: bool,
) -> Vec<Step> {
    match platform {
        Platform::SystemdUser => {
            let u = names.unit.as_str();
            let mut s = Vec::new();
            if change != Change::Unchanged {
                s.push(Step::new(&["systemctl", "--user", "daemon-reload"], false));
            }
            s.push(Step::new(&["systemctl", "--user", "enable", u], false));
            if start {
                let verb = if change == Change::Changed {
                    "restart"
                } else {
                    "start"
                };
                s.push(Step::new(&["systemctl", "--user", verb, u], false));
            }
            s
        }
        Platform::Launchd => {
            if !start {
                return Vec::new();
            }
            let domain = format!("gui/{uid}");
            let target = format!("{domain}/{}", names.label);
            let plist = unit_path.display().to_string();
            if change == Change::Unchanged {
                return vec![
                    Step::new(&["launchctl", "bootstrap", &domain, &plist], true),
                    Step::new(&["launchctl", "kickstart", &target], false),
                ];
            }
            vec![
                Step::new(&["launchctl", "bootout", &target], true),
                Step::new(&["launchctl", "bootstrap", &domain, &plist], false),
            ]
        }
        Platform::Windows => Vec::new(),
    }
}

/// `uninstall`: the commands before the unit file is removed, and after.
pub fn uninstall_steps(platform: Platform, names: &Names, uid: u32) -> (Vec<Step>, Vec<Step>) {
    match platform {
        Platform::SystemdUser => {
            let u = names.unit.as_str();
            (
                vec![Step::new(
                    &["systemctl", "--user", "disable", "--now", u],
                    true,
                )],
                vec![
                    Step::new(&["systemctl", "--user", "daemon-reload"], false),
                    Step::new(&["systemctl", "--user", "reset-failed", u], true),
                ],
            )
        }
        Platform::Launchd => (
            vec![Step::new(
                &[
                    "launchctl",
                    "bootout",
                    &format!("gui/{uid}/{}", names.label),
                ],
                true,
            )],
            Vec::new(),
        ),
        Platform::Windows => (Vec::new(), Vec::new()),
    }
}

/// Write through a sibling file and a rename, so the manager never reads a
/// half-written unit.
fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))
}

/// Runs service-manager commands. Injectable so the CLI path is testable
/// without touching the real systemd or launchd.
pub trait Manager {
    /// Run `argv`; `(success, stdout+stderr)`.
    fn run(&self, argv: &[String]) -> Result<(bool, String)>;
}

pub struct SystemManager;

impl Manager for SystemManager {
    fn run(&self, argv: &[String]) -> Result<(bool, String)> {
        let (prog, rest) = argv.split_first().ok_or_else(|| anyhow!("empty command"))?;
        let out = Command::new(prog)
            .args(rest)
            .output()
            .with_context(|| format!("run {prog}"))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.success(), text.trim().to_string()))
    }
}

fn run_q(m: &dyn Manager, argv: &[&str]) -> Option<String> {
    let v: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    match m.run(&v) {
        Ok((_, out)) => Some(out),
        Err(_) => None,
    }
}

/// What the manager reports about the unit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManagerState {
    pub running: bool,
    pub pid: Option<u32>,
    /// systemd `is-active` / launchd `state`, verbatim.
    pub state: String,
    /// systemd `is-enabled`; launchd: "loaded" / "not loaded".
    pub enabled: String,
}

pub fn manager_state(platform: Platform, names: &Names, uid: u32, m: &dyn Manager) -> ManagerState {
    match platform {
        Platform::SystemdUser => {
            let u = names.unit.as_str();
            let state = run_q(m, &["systemctl", "--user", "is-active", u]).unwrap_or_default();
            let enabled = run_q(m, &["systemctl", "--user", "is-enabled", u]).unwrap_or_default();
            let pid = run_q(
                m,
                &["systemctl", "--user", "show", "-p", "MainPID", "--value", u],
            )
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|p| *p > 0);
            ManagerState {
                running: state.trim() == "active",
                pid,
                state: state.trim().to_string(),
                enabled: enabled.trim().to_string(),
            }
        }
        Platform::Launchd => {
            let target = format!("gui/{uid}/{}", names.label);
            let v = vec!["launchctl".to_string(), "print".to_string(), target];
            match m.run(&v) {
                Ok((true, out)) => {
                    let field = |k: &str| {
                        out.lines()
                            .map(str::trim)
                            .find_map(|l| l.strip_prefix(k))
                            .map(|v| v.trim().to_string())
                    };
                    let state = field("state =").unwrap_or_default();
                    let pid = field("pid =").and_then(|p| p.parse().ok());
                    ManagerState {
                        running: state == "running",
                        pid,
                        state,
                        enabled: "loaded".into(),
                    }
                }
                _ => ManagerState {
                    running: false,
                    pid: None,
                    state: "not loaded".into(),
                    enabled: "not loaded".into(),
                },
            }
        }
        Platform::Windows => ManagerState::default(),
    }
}

/// Everything the CLI reads from the outside world, injectable for tests.
pub struct Env<'a> {
    pub platform: Option<Platform>,
    pub dirs: ServiceDirs,
    pub cwd: PathBuf,
    pub exe: PathBuf,
    pub uid: u32,
    pub manager: &'a dyn Manager,
    /// Checks the server flags the way a bare `xerj` start would.
    pub validate: &'a dyn Fn(&[String]) -> std::result::Result<(), String>,
    /// How long `install` waits for the node to answer.
    pub wait: Duration,
}

/// `xerj service …` from the real process environment.
pub fn run_cli(
    args: &[String],
    validate: &dyn Fn(&[String]) -> std::result::Result<(), String>,
) -> i32 {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let dirs = match ServiceDirs::from_env(&|k| std::env::var_os(k)) {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(err, "xerj service: {e:#}");
            return EXIT_FAIL;
        }
    };
    let exe = match localnode::xerj_executable().or_else(|| std::env::current_exe().ok()) {
        Some(e) => e,
        None => {
            let _ = writeln!(err, "xerj service: cannot locate the xerj binary");
            return EXIT_FAIL;
        }
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 0;
    let env = Env {
        platform: Platform::for_this_os(),
        cwd: std::env::current_dir().unwrap_or_else(|_| dirs.home.clone()),
        dirs,
        exe,
        uid,
        manager: &SystemManager,
        validate,
        wait: localnode::start_timeout(),
    };
    run_cli_with(args, &env, &mut out, &mut err)
}

struct Parsed {
    action: String,
    name: Option<String>,
    dry_run: bool,
    server: Vec<String>,
}

fn parse(args: &[String]) -> std::result::Result<Option<Parsed>, String> {
    let Some((action, rest)) = args.split_first() else {
        return Err("missing action: install, uninstall or status".into());
    };
    if matches!(action.as_str(), "-h" | "--help" | "help") {
        return Ok(None);
    }
    if !matches!(action.as_str(), "install" | "uninstall" | "status") {
        return Err(format!("unknown action '{action}'"));
    }
    let mut p = Parsed {
        action: action.clone(),
        name: None,
        dry_run: false,
        server: Vec::new(),
    };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--dry-run" => p.dry_run = true,
            "--name" => {
                p.name = Some(it.next().ok_or("--name needs a value")?.clone());
            }
            _ if p.action == "install" => p.server.push(a.clone()),
            other => return Err(format!("{} takes no '{other}'", p.action)),
        }
    }
    if p.action == "status" && p.dry_run {
        return Err("status takes no --dry-run".into());
    }
    Ok(Some(p))
}

/// `xerj service …` over an injectable environment.
pub fn run_cli_with(args: &[String], env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let p = match parse(args) {
        Ok(Some(p)) => p,
        Ok(None) => {
            let _ = writeln!(out, "{USAGE}");
            return EXIT_OK;
        }
        Err(e) => {
            let _ = writeln!(err, "xerj service: {e}\n\n{USAGE}");
            return EXIT_USAGE;
        }
    };
    let names = match Names::new(p.name.as_deref()) {
        Ok(n) => n,
        Err(e) => {
            let _ = writeln!(err, "xerj service: {e:#}");
            return EXIT_USAGE;
        }
    };
    let Some(platform) = env.platform else {
        let _ = writeln!(
            err,
            "xerj service: this OS has neither systemd, launchd nor Windows; run `xerj --detach` \
             from your init system instead"
        );
        return EXIT_FAIL;
    };
    let r = match p.action.as_str() {
        "install" => install(&p, &names, platform, env, out, err),
        "uninstall" => uninstall(&p, &names, platform, env, out),
        _ => status(&names, platform, env, out),
    };
    r.unwrap_or_else(|e| {
        let _ = writeln!(err, "xerj service {}: {e:#}", p.action);
        EXIT_FAIL
    })
}

fn run_steps(steps: &[Step], m: &dyn Manager, out: &mut dyn Write) -> Result<()> {
    for s in steps {
        let (ok, text) = m.run(&s.argv)?;
        if !ok && !s.tolerate_failure {
            bail!("`{}` failed: {text}", s.display());
        }
        writeln!(out, "  ran       {}", s.display())?;
    }
    Ok(())
}

/// A node other than the service's own that is alive on `data_dir`.
fn foreign_node(data_dir: &Path, service_pid: Option<u32>) -> Option<u32> {
    let rec = localnode::read_launch_record(data_dir)?;
    if Some(rec.pid) == service_pid || rec.pid == 0 {
        return None;
    }
    (nodedir::pid_state(rec.pid) == PidState::Alive).then_some(rec.pid)
}

/// The `--port` value written into a unit (systemd `--port N`, launchd
/// `<string>--port</string> <string>N</string>`).
pub fn port_from_unit(body: &str) -> Option<u16> {
    let after = &body[body.rfind("--port")? + "--port".len()..];
    let start = after.find(|c: char| c.is_ascii_digit())?;
    let gap = after[..start]
        .replace("</string>", "")
        .replace("<string>", "");
    if !gap.chars().all(|c| c.is_whitespace() || c == '"') {
        return None;
    }
    let digits: String = after[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn node_port(data_dir: &Path, spec_port: Option<u16>, pid: Option<u32>) -> (u16, bool) {
    match localnode::read_launch_record(data_dir) {
        Some(r) if pid.is_some_and(|p| p == r.pid) => (r.port, true),
        _ => (spec_port.unwrap_or(localnode::DEFAULT_PORT), false),
    }
}

fn log_hint(platform: Platform, names: &Names, data_dir: &Path) -> String {
    match platform {
        Platform::SystemdUser => format!("journalctl --user -u {}", names.unit),
        _ => data_dir
            .join(localnode::NODE_LOG_FILE)
            .display()
            .to_string(),
    }
}

fn install(
    p: &Parsed,
    names: &Names,
    platform: Platform,
    env: &Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let spec = match resolve_spec(&p.server, &env.cwd, &env.dirs.home, &env.exe) {
        Ok(s) => s,
        Err(e) => {
            writeln!(err, "xerj service install: {e:#}")?;
            return Ok(EXIT_USAGE);
        }
    };
    if let Err(e) = (env.validate)(&spec.args) {
        writeln!(err, "xerj service install: {e}\n\n{USAGE}")?;
        return Ok(EXIT_USAGE);
    }
    let Some(path) = env.dirs.unit_path(platform, names) else {
        write!(out, "{}", render_windows_instructions(&spec, &names.task))?;
        return Ok(if p.dry_run { EXIT_OK } else { EXIT_FAIL });
    };
    let body = match platform {
        Platform::SystemdUser => render_systemd_unit(&spec)?,
        _ => render_launchd_plist(&spec, &names.label)?,
    };
    let existing = std::fs::read_to_string(&path).ok();
    let change = Change::of(existing.as_deref(), &body);
    let state = if p.dry_run {
        ManagerState::default()
    } else {
        manager_state(platform, names, env.uid, env.manager)
    };
    let foreign = foreign_node(&spec.data_dir, state.pid);
    let steps = install_steps(platform, names, env.uid, &path, change, foreign.is_none());

    if p.dry_run {
        writeln!(
            out,
            "# xerj service install --dry-run: nothing was written or started.\n\
             # unit file ({}): {}",
            change.word(),
            path.display()
        )?;
        write!(out, "{body}")?;
        writeln!(out, "# commands:")?;
        for s in &steps {
            writeln!(out, "{}", s.display())?;
        }
        if let Some(pid) = foreign {
            writeln!(
                out,
                "# a node (pid {pid}) already runs on {}; it would be registered, not started",
                spec.data_dir.display()
            )?;
        }
        return Ok(EXIT_OK);
    }

    std::fs::create_dir_all(&spec.data_dir)
        .with_context(|| format!("create data dir {}", spec.data_dir.display()))?;
    if change == Change::Unchanged {
        writeln!(out, "xerj service: {} is up to date", path.display())?;
    } else {
        write_file_atomic(&path, &body)?;
        writeln!(
            out,
            "xerj service: wrote {} ({})",
            path.display(),
            change.word()
        )?;
    }
    run_steps(&steps, env.manager, out)?;
    writeln!(out, "  data dir  {}", spec.data_dir.display())?;
    writeln!(
        out,
        "  log       {}",
        log_hint(platform, names, &spec.data_dir)
    )?;
    if let Some(pid) = foreign {
        let start = match platform {
            Platform::SystemdUser => format!("systemctl --user start {}", names.unit),
            _ => format!("launchctl bootstrap gui/{} {}", env.uid, path.display()),
        };
        writeln!(
            out,
            "  NOT started: a node (pid {pid}) not run by the service already holds this data \
             dir.\n  Stop it, then start the service:\n    kill {pid}\n    {start}"
        )?;
        return Ok(EXIT_OK);
    }
    let code = wait_serving(&spec, names, platform, env, out)?;
    if platform == Platform::SystemdUser && code == EXIT_OK {
        let linger = run_q(
            env.manager,
            &[
                "loginctl",
                "show-user",
                &env.uid.to_string(),
                "-p",
                "Linger",
                "--value",
            ],
        )
        .unwrap_or_default();
        if linger.trim() != "yes" {
            writeln!(
                out,
                "  boot      starts at login. To start at boot with no login: loginctl \
                 enable-linger $USER"
            )?;
        }
    }
    let stop = match platform {
        Platform::SystemdUser => format!("systemctl --user stop {}", names.unit),
        _ => format!("launchctl bootout gui/{}/{}", env.uid, names.label),
    };
    writeln!(
        out,
        "  stop      {stop}   remove: xerj service uninstall{}",
        names.flag
    )?;
    Ok(code)
}

fn wait_serving(
    spec: &Spec,
    names: &Names,
    platform: Platform,
    env: &Env,
    out: &mut dyn Write,
) -> Result<i32> {
    let t0 = Instant::now();
    loop {
        let st = manager_state(platform, names, env.uid, env.manager);
        let (port, recorded) = node_port(&spec.data_dir, spec.port_flag(), st.pid);
        let scheme = if spec.insecure() { "http" } else { "https" };
        let url = format!("{scheme}://localhost:{port}");
        if st.running && recorded && localnode::url_serving(&url) {
            writeln!(
                out,
                "  running   pid {}, {url}, answered after {:.1} s",
                st.pid.unwrap_or(0),
                t0.elapsed().as_secs_f64()
            )?;
            return Ok(EXIT_OK);
        }
        if st.state == "failed" {
            writeln!(
                out,
                "  FAILED    the node exited; see {}",
                log_hint(platform, names, &spec.data_dir)
            )?;
            let tail = match platform {
                Platform::SystemdUser => run_q(
                    env.manager,
                    &[
                        "journalctl",
                        "--user",
                        "-u",
                        &names.unit,
                        "-n",
                        "20",
                        "--no-pager",
                    ],
                )
                .unwrap_or_default(),
                _ => localnode::log_tail(&spec.data_dir.join(localnode::NODE_LOG_FILE), 20),
            };
            writeln!(out, "{tail}")?;
            return Ok(EXIT_FAIL);
        }
        if t0.elapsed() >= env.wait {
            writeln!(
                out,
                "  starting  not answering after {:.0} s (state {:?}); a large data dir can take \
                 longer. Check: xerj service status",
                t0.elapsed().as_secs_f64(),
                st.state
            )?;
            return Ok(EXIT_OK);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn uninstall(
    p: &Parsed,
    names: &Names,
    platform: Platform,
    env: &Env,
    out: &mut dyn Write,
) -> Result<i32> {
    let Some(path) = env.dirs.unit_path(platform, names) else {
        writeln!(
            out,
            "xerj service: nothing to uninstall on Windows. If you made the logon task:\n  \
             schtasks /Delete /TN {} /F",
            names.task
        )?;
        return Ok(if p.dry_run { EXIT_OK } else { EXIT_FAIL });
    };
    let (before, after) = uninstall_steps(platform, names, env.uid);
    let present = path.exists();
    if p.dry_run {
        writeln!(
            out,
            "# xerj service uninstall --dry-run: nothing was stopped or removed."
        )?;
        for s in &before {
            writeln!(out, "{}", s.display())?;
        }
        if present {
            writeln!(out, "rm {}", path.display())?;
        } else {
            writeln!(out, "# {} is not there", path.display())?;
        }
        for s in &after {
            writeln!(out, "{}", s.display())?;
        }
        return Ok(EXIT_OK);
    }
    if !present {
        writeln!(
            out,
            "xerj service: not installed ({} is not there)",
            path.display()
        )?;
        return Ok(EXIT_OK);
    }
    run_steps(&before, env.manager, out)?;
    std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    writeln!(out, "  removed   {}", path.display())?;
    run_steps(&after, env.manager, out)?;
    writeln!(
        out,
        "xerj service: uninstalled; the data dir was left as it is"
    )?;
    Ok(EXIT_OK)
}

fn status(names: &Names, platform: Platform, env: &Env, out: &mut dyn Write) -> Result<i32> {
    let Some(path) = env.dirs.unit_path(platform, names) else {
        writeln!(
            out,
            "xerj service: Windows services are not managed by xerj. Check the logon task:\n  \
             schtasks /Query /TN {}",
            names.task
        )?;
        return Ok(EXIT_NOT_INSTALLED);
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        writeln!(
            out,
            "xerj service: not installed ({} is not there). Install: xerj service install{}",
            path.display(),
            names.flag
        )?;
        return Ok(EXIT_NOT_INSTALLED);
    };
    let st = manager_state(platform, names, env.uid, env.manager);
    writeln!(out, "xerj service: {}", path.display())?;
    writeln!(out, "  enabled   {}", st.enabled)?;
    writeln!(out, "  state     {}", st.state)?;
    if let Some(pid) = st.pid {
        writeln!(out, "  pid       {pid}")?;
    }
    if let Some(dir) = data_dir_from_unit(platform, &body) {
        let insecure = body.contains("--insecure");
        let (port, recorded) = node_port(&dir, port_from_unit(&body), st.pid);
        let scheme = if insecure { "http" } else { "https" };
        let url = format!("{scheme}://localhost:{port}");
        let answer = if st.running && recorded && localnode::url_serving(&url) {
            "answering"
        } else {
            "not answering"
        };
        writeln!(out, "  data dir  {}", dir.display())?;
        writeln!(out, "  url       {url} ({answer})")?;
        writeln!(out, "  log       {}", log_hint(platform, names, &dir))?;
        if !st.running {
            if let Some(pid) = foreign_node(&dir, st.pid) {
                writeln!(
                    out,
                    "  note      a node (pid {pid}) not run by the service holds this data dir"
                )?;
            }
        }
    }
    if platform == Platform::SystemdUser {
        let linger = run_q(
            env.manager,
            &[
                "loginctl",
                "show-user",
                &env.uid.to_string(),
                "-p",
                "Linger",
                "--value",
            ],
        )
        .unwrap_or_default();
        let boot = if linger.trim() == "yes" {
            "starts at boot (lingering on)"
        } else {
            "starts at login (loginctl enable-linger $USER to start at boot)"
        };
        writeln!(out, "  boot      {boot}")?;
    }
    Ok(if st.running {
        EXIT_OK
    } else {
        EXIT_NOT_RUNNING
    })
}

#[cfg(test)]
mod tests;
