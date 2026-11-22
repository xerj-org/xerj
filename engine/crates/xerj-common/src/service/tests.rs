use super::*;
use std::cell::RefCell;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/service/golden")
        .join(name);
    if std::env::var_os("XERJ_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with XERJ_UPDATE_GOLDEN=1", path.display()));
    assert_eq!(actual, want, "golden {name} differs");
}

fn spec(exe: &str, data: &str, flags: &[&str]) -> Spec {
    resolve_spec(
        &s(&[flags, &["--data-dir", data]].concat()),
        Path::new("/work"),
        Path::new("/home/u"),
        Path::new(exe),
    )
    .unwrap()
}

fn resolve(args: &[&str], cwd: &Path, home: &str) -> Result<Spec> {
    resolve_spec(&s(args), cwd, Path::new(home), Path::new("/x"))
}

#[test]
fn systemd_unit_golden() {
    let sp = spec(
        "/opt/xerj/bin/xerj",
        "/home/u/xerj-data",
        &["--insecure", "--port", "9280"],
    );
    golden("xerj.service", &render_systemd_unit(&sp).unwrap());
}

#[test]
fn systemd_unit_quotes_spaces_percent_and_dollar() {
    let sp = spec(
        "/opt/my apps/xerj",
        "/home/u/My Data 100%",
        &["--config", "/etc/x$y.toml"],
    );
    golden("xerj-escaped.service", &render_systemd_unit(&sp).unwrap());
}

#[test]
fn units_refuse_a_newline() {
    let sp = spec("/opt/xerj", "/home/u/a\nb", &[]);
    assert!(render_systemd_unit(&sp).is_err());
    assert!(render_launchd_plist(&sp, "org.xerj.node").is_err());
}

#[test]
fn launchd_plist_golden() {
    let sp = spec(
        "/usr/local/bin/xerj",
        "/Users/u/R&D <data>",
        &["--insecure"],
    );
    golden(
        "org.xerj.node.plist",
        &render_launchd_plist(&sp, "org.xerj.node").unwrap(),
    );
}

#[test]
fn windows_instructions_golden() {
    let sp = Spec {
        exe: PathBuf::from(r"C:\Program Files\xerj\xerj.exe"),
        args: s(&["--insecure", "--data-dir", r"C:\Users\u\xerj data"]),
        data_dir: PathBuf::from(r"C:\Users\u\xerj data"),
    };
    golden(
        "windows.txt",
        &render_windows_instructions(&sp, "xerj-node"),
    );
}

#[test]
fn data_dir_round_trips_through_each_unit() {
    let sp = spec("/opt/xerj", "/home/u/R&D 50%", &["--insecure"]);
    let unit = render_systemd_unit(&sp).unwrap();
    assert_eq!(
        data_dir_from_unit(Platform::SystemdUser, &unit),
        Some(PathBuf::from("/home/u/R&D 50%"))
    );
    let plist = render_launchd_plist(&sp, "org.xerj.node").unwrap();
    assert_eq!(
        data_dir_from_unit(Platform::Launchd, &plist),
        Some(PathBuf::from("/home/u/R&D 50%"))
    );
}

#[test]
fn resolve_defaults_to_home_xerj_data_not_dot_data() {
    let sp = resolve(&["--insecure"], Path::new("/work"), "/home/u").unwrap();
    assert_eq!(sp.data_dir, PathBuf::from("/home/u/xerj-data"));
    assert_eq!(
        sp.args,
        s(&["--insecure", "--data-dir", "/home/u/xerj-data"])
    );
}

#[test]
fn resolve_makes_paths_absolute_and_keeps_one_data_dir() {
    let sp = resolve(
        &[
            "-d",
            "old",
            "--data-dir",
            "./sub/../nodes/a",
            "-c",
            "cfg/x.toml",
            "--port",
            "9300",
        ],
        Path::new("/work"),
        "/home/u",
    )
    .unwrap();
    assert_eq!(sp.data_dir, PathBuf::from("/work/nodes/a"));
    assert_eq!(
        sp.args,
        s(&[
            "-c",
            "/work/cfg/x.toml",
            "--port",
            "9300",
            "--data-dir",
            "/work/nodes/a"
        ])
    );
    assert_eq!(sp.port_flag(), Some(9300));
    let sp = resolve(&["-d", "~/n"], Path::new("/w"), "/home/u").unwrap();
    assert_eq!(sp.data_dir, PathBuf::from("/home/u/n"));
}

#[test]
fn resolve_reads_the_config_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("x.toml");
    std::fs::write(&cfg, "[server]\ndata_dir = \"store\"\n").unwrap();
    let sp = resolve(&["--config", "x.toml"], dir.path(), "/home/u").unwrap();
    assert_eq!(sp.data_dir, dir.path().join("store"));
    std::fs::write(&cfg, "[server]\nport = 1\n").unwrap();
    let sp = resolve(&["--config", "x.toml"], dir.path(), "/home/u").unwrap();
    assert_eq!(sp.data_dir, PathBuf::from("/home/u/xerj-data"));
}

#[test]
fn resolve_refuses_detach_and_a_missing_value() {
    let e = resolve(&["--detach"], Path::new("/w"), "/h").unwrap_err();
    assert!(format!("{e}").contains("--detach"));
    assert!(resolve(&["--data-dir"], Path::new("/w"), "/h").is_err());
}

#[test]
fn names_default_and_suffix() {
    let n = Names::new(None).unwrap();
    assert_eq!(
        (n.unit.as_str(), n.label.as_str(), n.task.as_str()),
        ("xerj.service", "org.xerj.node", "xerj-node")
    );
    let n = Names::new(Some("dev_2")).unwrap();
    assert_eq!(
        (n.unit.as_str(), n.label.as_str()),
        ("xerj-dev_2.service", "org.xerj.node-dev_2")
    );
    let long = "x".repeat(65);
    for bad in ["", "a/b", "a b", "../x", long.as_str()] {
        assert!(Names::new(Some(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn xdg_config_home_must_be_absolute() {
    let env = |k: &str| match k {
        "HOME" => Some(OsString::from("/home/u")),
        "XDG_CONFIG_HOME" => Some(OsString::from("rel")),
        _ => None,
    };
    let d = ServiceDirs::from_env(&env).unwrap();
    assert_eq!(
        d.systemd_user,
        PathBuf::from("/home/u/.config/systemd/user")
    );
    let env = |k: &str| match k {
        "HOME" => Some(OsString::from("/home/u")),
        "XDG_CONFIG_HOME" => Some(OsString::from("/cfg")),
        _ => None,
    };
    let d = ServiceDirs::from_env(&env).unwrap();
    assert_eq!(d.systemd_user, PathBuf::from("/cfg/systemd/user"));
    assert_eq!(
        d.launch_agents,
        PathBuf::from("/home/u/Library/LaunchAgents")
    );
    assert!(ServiceDirs::from_env(&|_| None).is_err());
}

fn shown(steps: &[Step]) -> Vec<String> {
    steps.iter().map(Step::display).collect()
}

#[test]
fn systemd_steps_are_idempotent() {
    let n = Names::new(None).unwrap();
    let p = Path::new("/u/xerj.service");
    let steps = |c, start| shown(&install_steps(Platform::SystemdUser, &n, 1, p, c, start));
    assert_eq!(
        steps(Change::New, true),
        s(&[
            "systemctl --user daemon-reload",
            "systemctl --user enable xerj.service",
            "systemctl --user start xerj.service"
        ])
    );
    assert_eq!(
        steps(Change::Changed, true),
        s(&[
            "systemctl --user daemon-reload",
            "systemctl --user enable xerj.service",
            "systemctl --user restart xerj.service"
        ])
    );
    assert_eq!(
        steps(Change::Unchanged, true),
        s(&[
            "systemctl --user enable xerj.service",
            "systemctl --user start xerj.service"
        ])
    );
    assert_eq!(
        steps(Change::New, false),
        s(&[
            "systemctl --user daemon-reload",
            "systemctl --user enable xerj.service"
        ])
    );
}

#[test]
fn launchd_steps_are_idempotent() {
    let n = Names::new(None).unwrap();
    let p = Path::new("/Users/u/Library/LaunchAgents/org.xerj.node.plist");
    let steps = |c, start| shown(&install_steps(Platform::Launchd, &n, 501, p, c, start));
    assert_eq!(
        steps(Change::New, true),
        s(&[
            "launchctl bootout gui/501/org.xerj.node",
            "launchctl bootstrap gui/501 /Users/u/Library/LaunchAgents/org.xerj.node.plist"
        ])
    );
    assert_eq!(
        steps(Change::Unchanged, true),
        s(&[
            "launchctl bootstrap gui/501 /Users/u/Library/LaunchAgents/org.xerj.node.plist",
            "launchctl kickstart gui/501/org.xerj.node"
        ])
    );
    assert!(steps(Change::New, false).is_empty());
    let (before, after) = uninstall_steps(Platform::Launchd, &n, 501);
    assert_eq!(
        shown(&before),
        s(&["launchctl bootout gui/501/org.xerj.node"])
    );
    assert!(after.is_empty());
}

struct Fake {
    calls: RefCell<Vec<String>>,
    active: &'static str,
}

impl Manager for Fake {
    fn run(&self, argv: &[String]) -> Result<(bool, String)> {
        let line = argv.join(" ");
        self.calls.borrow_mut().push(line.clone());
        let out = if line.contains("is-active") {
            self.active.to_string()
        } else if line.contains("is-enabled") {
            "enabled".into()
        } else if line.contains("MainPID") {
            "0".into()
        } else if line.contains("Linger") {
            "no".into()
        } else {
            String::new()
        };
        Ok((true, out))
    }
}

impl Fake {
    fn new(active: &'static str) -> Fake {
        Fake {
            calls: RefCell::new(Vec::new()),
            active,
        }
    }

    fn mutating(&self) -> Vec<String> {
        let query = ["is-active", "is-enabled", "MainPID", "Linger"];
        self.calls
            .borrow()
            .iter()
            .filter(|c| !query.iter().any(|q| c.contains(q)))
            .cloned()
            .collect()
    }
}

fn ok_validate(_: &[String]) -> std::result::Result<(), String> {
    Ok(())
}

fn env_for<'a>(home: &Path, platform: Platform, m: &'a Fake) -> Env<'a> {
    let h = home.as_os_str().to_owned();
    Env {
        platform: Some(platform),
        dirs: ServiceDirs::from_env(&|k| (k == "HOME").then(|| h.clone())).unwrap(),
        cwd: home.to_path_buf(),
        exe: PathBuf::from("/opt/xerj/bin/xerj"),
        uid: 1000,
        manager: m,
        validate: &ok_validate,
        wait: Duration::ZERO,
    }
}

fn run(args: &[&str], env: &Env) -> (i32, String, String) {
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let code = run_cli_with(&s(args), env, &mut o, &mut e);
    (
        code,
        String::from_utf8(o).unwrap(),
        String::from_utf8(e).unwrap(),
    )
}

#[test]
fn linux_dry_run_prints_the_unit_and_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::SystemdUser, &m);
    let (code, out, _) = run(
        &["install", "--dry-run", "--insecure", "--port", "28210"],
        &env,
    );
    assert_eq!(code, EXIT_OK);
    let unit = home.path().join(".config/systemd/user/xerj.service");
    assert!(
        out.contains(&format!("# unit file (new): {}", unit.display())),
        "{out}"
    );
    assert!(
        out.contains("ExecStart=/opt/xerj/bin/xerj --insecure --port 28210 --data-dir "),
        "{out}"
    );
    assert!(
        out.contains(
            "systemctl --user daemon-reload\nsystemctl --user enable xerj.service\n\
             systemctl --user start xerj.service\n"
        ),
        "{out}"
    );
    assert!(!unit.exists());
    assert!(!home.path().join("xerj-data").exists());
    assert!(m.calls.borrow().is_empty(), "{:?}", m.calls.borrow());
}

#[test]
fn linux_install_is_idempotent_and_restarts_only_on_change() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::SystemdUser, &m);
    let unit = home.path().join(".config/systemd/user/xerj.service");

    let (code, out, err) = run(&["install", "--insecure"], &env);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    assert!(unit.is_file());
    assert!(home.path().join("xerj-data").is_dir());
    assert_eq!(
        m.mutating(),
        s(&[
            "systemctl --user daemon-reload",
            "systemctl --user enable xerj.service",
            "systemctl --user start xerj.service"
        ])
    );
    let first = std::fs::read_to_string(&unit).unwrap();
    let mtime = std::fs::metadata(&unit).unwrap().modified().unwrap();

    m.calls.borrow_mut().clear();
    let (code, out, _) = run(&["install", "--insecure"], &env);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("is up to date"), "{out}");
    assert_eq!(
        m.mutating(),
        s(&[
            "systemctl --user enable xerj.service",
            "systemctl --user start xerj.service"
        ])
    );
    assert_eq!(std::fs::metadata(&unit).unwrap().modified().unwrap(), mtime);

    m.calls.borrow_mut().clear();
    let (code, _, _) = run(&["install", "--insecure", "--port", "28211"], &env);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        m.mutating(),
        s(&[
            "systemctl --user daemon-reload",
            "systemctl --user enable xerj.service",
            "systemctl --user restart xerj.service"
        ])
    );
    assert_ne!(std::fs::read_to_string(&unit).unwrap(), first);
    assert!(!unit.with_file_name("xerj.service.new").exists());
}

#[test]
fn linux_status_and_uninstall() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::SystemdUser, &m);
    assert_eq!(run(&["status"], &env).0, EXIT_NOT_INSTALLED);
    assert_eq!(
        run(&["install", "--insecure", "--port", "28212"], &env).0,
        EXIT_OK
    );
    let (code, out, _) = run(&["status"], &env);
    assert_eq!(code, EXIT_NOT_RUNNING, "{out}");
    let dd = home.path().join("xerj-data");
    assert!(
        out.contains(&format!("data dir  {}", dd.display())),
        "{out}"
    );
    assert!(out.contains("loginctl enable-linger"), "{out}");

    let m2 = Fake::new("active");
    let env2 = env_for(home.path(), Platform::SystemdUser, &m2);
    assert_eq!(run(&["status"], &env2).0, EXIT_OK);

    m.calls.borrow_mut().clear();
    let (code, out, _) = run(&["uninstall", "--dry-run"], &env);
    assert_eq!(code, EXIT_OK);
    assert!(
        out.contains("systemctl --user disable --now xerj.service"),
        "{out}"
    );
    assert!(m.calls.borrow().is_empty());
    let unit = home.path().join(".config/systemd/user/xerj.service");
    assert!(unit.exists());

    let (code, _, _) = run(&["uninstall"], &env);
    assert_eq!(code, EXIT_OK);
    assert!(!unit.exists());
    assert!(dd.is_dir(), "the data dir is kept");
    assert_eq!(
        m.mutating(),
        s(&[
            "systemctl --user disable --now xerj.service",
            "systemctl --user daemon-reload",
            "systemctl --user reset-failed xerj.service"
        ])
    );
    let (code, out, _) = run(&["uninstall"], &env);
    assert_eq!(code, EXIT_OK);
    assert!(out.contains("not installed"));
}

#[test]
fn launchd_install_writes_the_plist_and_bootstraps() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::Launchd, &m);
    let (code, out, err) = run(&["install", "--insecure", "--name", "t"], &env);
    assert_eq!(code, EXIT_OK, "{out}{err}");
    let plist = home
        .path()
        .join("Library/LaunchAgents/org.xerj.node-t.plist");
    assert!(plist.is_file());
    assert_eq!(
        m.mutating(),
        s(&[
            "launchctl print gui/1000/org.xerj.node-t",
            "launchctl bootout gui/1000/org.xerj.node-t",
            &format!("launchctl bootstrap gui/1000 {}", plist.display()),
            "launchctl print gui/1000/org.xerj.node-t",
        ])
    );
}

#[test]
fn windows_prints_instructions_and_installs_nothing() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::Windows, &m);
    let (code, out, _) = run(&["install", "--insecure"], &env);
    assert_eq!(code, EXIT_FAIL);
    assert!(out.contains("schtasks /Create /TN xerj-node"), "{out}");
    assert!(out.contains("sc.exe"), "{out}");
    assert_eq!(run(&["install", "--dry-run"], &env).0, EXIT_OK);
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    assert!(m.calls.borrow().is_empty());
}

#[test]
fn usage_errors_exit_2() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::SystemdUser, &m);
    assert_eq!(run(&[], &env).0, EXIT_USAGE);
    assert_eq!(run(&["start"], &env).0, EXIT_USAGE);
    assert_eq!(run(&["status", "--dry-run"], &env).0, EXIT_USAGE);
    assert_eq!(run(&["uninstall", "--insecure"], &env).0, EXIT_USAGE);
    assert_eq!(run(&["install", "--name", "a/b"], &env).0, EXIT_USAGE);
    assert_eq!(run(&["install", "--detach"], &env).0, EXIT_USAGE);
    assert_eq!(run(&["install", "--help"], &env).0, EXIT_OK);
    let bad = |_: &[String]| Err("unknown argument: --bogus".to_string());
    let env = Env {
        validate: &bad,
        ..env_for(home.path(), Platform::SystemdUser, &m)
    };
    let (code, _, err) = run(&["install", "--bogus"], &env);
    assert_eq!(code, EXIT_USAGE);
    assert!(err.contains("--bogus"));
    assert!(m.calls.borrow().is_empty());
}

#[test]
fn port_is_read_back_from_each_unit() {
    let sp = spec("/opt/xerj", "/home/u/d", &["--insecure", "--port", "28220"]);
    let unit = render_systemd_unit(&sp).unwrap();
    assert_eq!(port_from_unit(&unit), Some(28220));
    let plist = render_launchd_plist(&sp, "org.xerj.node").unwrap();
    assert_eq!(port_from_unit(&plist), Some(28220));
    let sp = spec("/opt/xerj", "/home/u/d", &["--insecure"]);
    assert_eq!(port_from_unit(&render_systemd_unit(&sp).unwrap()), None);
    let sp = spec("/opt/xerj", "/home/u/--port-x9", &[]);
    assert_eq!(port_from_unit(&render_systemd_unit(&sp).unwrap()), None);
}

#[test]
fn hints_carry_the_name() {
    let home = tempfile::tempdir().unwrap();
    let m = Fake::new("inactive");
    let env = env_for(home.path(), Platform::SystemdUser, &m);
    let (_, out, _) = run(&["install", "--name", "dev"], &env);
    assert!(out.contains("xerj service uninstall --name dev"), "{out}");
    let (_, out, _) = run(&["status", "--name", "other"], &env);
    assert!(out.contains("xerj service install --name other"), "{out}");
}
