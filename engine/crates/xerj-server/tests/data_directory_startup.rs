//! Observe the real startup banner without touching any existing node or data.
use std::fs::File;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Node(Child);
impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn startup(work: &std::path::Path, extra: &[&str], env_config: Option<&str>) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"));
    child
        .current_dir(work)
        .env_clear()
        .env("XERJ_DISABLE_FEEDBACK", "true")
        .env("XERJ_NUM_CPUS", "2")
        .args(["--insecure"])
        .args(extra)
        .stdout(Stdio::from(File::create(work.join("stdout")).unwrap()))
        .stderr(Stdio::from(File::create(work.join("stderr")).unwrap()));
    if let Some(path) = env_config {
        child.env("XERJ_CONFIG", path);
    }
    let mut node = Node(child.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let output = std::fs::read_to_string(work.join("stdout")).unwrap();
        if output.contains(" Started in ") {
            return output;
        }
        if let Some(status) = node.0.try_wait().unwrap() {
            panic!(
                "node exited {status}: {}",
                std::fs::read_to_string(work.join("stderr")).unwrap()
            );
        }
        assert!(Instant::now() < deadline, "startup timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn config(work: &std::path::Path, data: Option<&str>) {
    let first =
        xerj_common::loopback_test::expect_loopback(std::net::TcpListener::bind("127.0.0.1:0"));
    let second =
        xerj_common::loopback_test::expect_loopback(std::net::TcpListener::bind("127.0.0.1:0"));
    let mut text = format!(
        "[server]\nes_compat_port={}\nrest_port={}\ngrpc_port=0\n",
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port()
    );
    if let Some(data) = data {
        text.push_str(&format!(
            "data_dir={}\n",
            serde_json::to_string(data).unwrap()
        ));
    }
    std::fs::write(work.join("node.toml"), text).unwrap();
}

#[test]
fn startup_names_the_actual_data_directory_source_and_creation_state() {
    for (kind, setting, flags, env, expected_dir, source) in [
        (
            "config",
            Some("persistent"),
            vec!["-c", "node.toml"],
            Some("missing.toml"),
            "persistent",
            "config file (--config)",
        ),
        (
            "env",
            Some("persistent"),
            vec![],
            Some("node.toml"),
            "persistent",
            "config file (XERJ_CONFIG)",
        ),
        (
            "flag",
            Some("unused"),
            vec!["-c", "node.toml", "--data-dir", "selected"],
            None,
            "selected",
            "--data-dir",
        ),
        (
            "default",
            None,
            vec!["-c", "node.toml"],
            None,
            "data",
            "default ./data",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path();
        config(work, setting);
        let output = startup(work, &flags, env);
        assert!(output.contains(" Data source  "), "{kind}: {output}");
        assert!(output.contains(source), "{kind}: {output}");
        assert!(output.contains("created this launch"), "{kind}: {output}");
        let actual = std::fs::canonicalize(work.join(expected_dir)).unwrap();
        assert!(
            output.contains(&actual.display().to_string().escape_debug().to_string()),
            "{kind}: {output}"
        );
        assert!(
            output.contains("previous --data-dir or --config"),
            "{kind}: {output}"
        );
        if kind == "flag" {
            assert!(!work.join("unused").exists());
        }
        std::fs::write(work.join(expected_dir).join("operator-sentinel"), "keep").unwrap();
        let repeated = startup(work, &flags, env);
        assert!(
            repeated.contains("existing directory"),
            "{kind}: {repeated}"
        );
        assert!(
            !repeated.contains("created this launch"),
            "{kind}: {repeated}"
        );
        assert_eq!(
            std::fs::read_to_string(work.join(expected_dir).join("operator-sentinel")).unwrap(),
            "keep"
        );
    }
}
