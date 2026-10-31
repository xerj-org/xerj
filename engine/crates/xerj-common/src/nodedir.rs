//! Which data dir last served a local port, and whether its node is still
//! alive (#2455).
//!
//! A client that cannot connect to a loopback node (`xerj mcp` is the main
//! one) used to say only "no XERJ node is listening", whether no node was
//! ever started or one was running and died. The second case can be
//! diagnosed precisely: the node recorded its PID in `<data_dir>/node.lock`,
//! and that PID either still exists or does not.
//!
//! The server publishes `~/.xerj/by-port/<port>.data-dir` on every start,
//! with or without auth. It is one line, the absolute data dir. It sits
//! beside the admin-key pointer [`crate::localauth::publish_admin_key_for_port`]
//! writes (that one is only written when auth is on). A client reads it with
//! [`unreachable_node_hint`]. A pointer that outlived its node is the point:
//! it names the dir whose `node.lock` explains why nothing answers.

use std::path::{Path, PathBuf};

/// `~/.xerj/by-port/<port>.data-dir` under `home`.
fn data_dir_pointer_in(home: &Path, port: u16) -> PathBuf {
    home.join(".xerj")
        .join("by-port")
        .join(format!("{port}.data-dir"))
}

/// Record that `data_dir` is serving `port`. A missing home directory is not
/// an error, since there is nowhere to publish.
pub fn publish_data_dir_for_port(port: u16, data_dir: &Path) -> std::io::Result<()> {
    let Some(home) = crate::home::home_dir() else {
        return Ok(());
    };
    publish_data_dir_for_port_in(&home, port, data_dir)
}

fn publish_data_dir_for_port_in(home: &Path, port: u16, data_dir: &Path) -> std::io::Result<()> {
    let dest = data_dir_pointer_in(home, port);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let abs = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    std::fs::write(&dest, format!("{}\n", abs.display()))
}

/// The data dir that last served `port`, if any. It falls back to the parent
/// of the admin-key pointer, which nodes before this change wrote when auth was on.
fn published_data_dir_in(home: &Path, port: u16) -> Option<PathBuf> {
    let read = |path: PathBuf| {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| PathBuf::from(text.trim()))
            .filter(|path| !path.as_os_str().is_empty())
    };
    read(data_dir_pointer_in(home, port)).or_else(|| {
        read(home.join(".xerj").join("by-port").join(port.to_string()))
            .and_then(|key| key.parent().map(Path::to_path_buf))
    })
}

/// Whether a PID names a running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidState {
    Alive,
    Dead,
    /// The check failed or is not supported on this platform.
    Unknown,
}

/// The same probe the engine uses for `node.lock` recovery
/// (`xerj-engine/src/node_lock.rs` `pid_liveness`). `kill(pid, 0)` on Unix,
/// `OpenProcess` + `WaitForSingleObject` on Windows.
#[cfg(unix)]
pub fn pid_state(pid: u32) -> PidState {
    let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
        return PidState::Unknown;
    };
    // SAFETY: signal 0 delivers nothing. `kill` only reports whether `pid`
    // names a process the caller could signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return PidState::Alive;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::ESRCH => PidState::Dead,
        Some(code) if code == libc::EPERM => PidState::Alive,
        _ => PidState::Unknown,
    }
}

#[cfg(windows)]
pub fn pid_state(pid: u32) -> PidState {
    if pid == 0 {
        return PidState::Unknown;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 0x102;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(
            desired_access: u32,
            inherit_handle: i32,
            process_id: u32,
        ) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }

    // SAFETY: the Win32 liveness calls the engine already makes. The handle
    // is null or closed before returning. `GetLastError` is read right after
    // the failed `OpenProcess`.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return match GetLastError() {
                ERROR_INVALID_PARAMETER => PidState::Dead,
                ERROR_ACCESS_DENIED => PidState::Alive,
                _ => PidState::Unknown,
            };
        }
        let waited = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        match waited {
            WAIT_TIMEOUT => PidState::Alive,
            WAIT_OBJECT_0 => PidState::Dead,
            _ => PidState::Unknown,
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub fn pid_state(_pid: u32) -> PidState {
    PidState::Unknown
}

/// One sentence about the node that last served `url`'s port, or `None`
/// when nothing is known (not loopback, no pointer, no PID in `node.lock`).
pub fn unreachable_node_hint(url: &str) -> Option<String> {
    if !crate::localauth::url_is_loopback(url) {
        return None;
    }
    let home = crate::home::home_dir()?;
    unreachable_node_hint_in(&home, url, &pid_state)
}

fn unreachable_node_hint_in(
    home: &Path,
    url: &str,
    probe: &dyn Fn(u32) -> PidState,
) -> Option<String> {
    let port = crate::localauth::xerj_url_port(url);
    let data_dir = published_data_dir_in(home, port)?;
    node_lock_hint(&data_dir, port, probe)
}

/// The hint for one data dir, from its `node.lock` PID.
///
/// `port` is the port the node served; a non-default one is part of the
/// restart command, or the restarted node would come back on 9200.
pub fn node_lock_hint(
    data_dir: &Path,
    port: u16,
    probe: &dyn Fn(u32) -> PidState,
) -> Option<String> {
    let lock = data_dir.join("node.lock");
    let port_flag = if port == 9200 {
        String::new()
    } else {
        format!(" --port {port}")
    };
    let pid: u32 = std::fs::read_to_string(&lock).ok()?.trim().parse().ok()?;
    let dir = data_dir.display();
    match probe(pid) {
        PidState::Dead => Some(format!(
            "The node that last served this port used data dir {dir}; its pid {pid} \
             (recorded in {}) is no longer running, so it exited or was killed. \
             Restart it with the same --data-dir: xerj --data-dir {dir}{port_flag} (add \
             --insecure if it ran that way).",
            lock.display()
        )),
        PidState::Alive => Some(format!(
            "Process {pid} still holds {} for data dir {dir}, but nothing answers on this \
             port. It may still be opening its indices (wait and retry), or it is \
             listening on a different port.",
            lock.display()
        )),
        PidState::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dead_pid_in_the_published_dir_names_the_dir_and_the_restart() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("node.lock"), "4242").unwrap();
        publish_data_dir_for_port_in(home.path(), 9207, data.path()).unwrap();
        let hint = unreachable_node_hint_in(home.path(), "http://localhost:9207", &|pid| {
            assert_eq!(pid, 4242);
            PidState::Dead
        })
        .expect("hint");
        assert!(hint.contains("pid 4242"), "{hint}");
        assert!(hint.contains("is no longer running"), "{hint}");
        assert!(hint.contains("xerj --data-dir "), "{hint}");
        assert!(hint.contains(" --port 9207 "), "{hint}");
        let canon = std::fs::canonicalize(data.path()).unwrap();
        assert!(hint.contains(&canon.display().to_string()), "{hint}");
    }

    #[test]
    fn a_live_pid_says_the_node_may_still_be_starting() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("node.lock"), "77\n").unwrap();
        publish_data_dir_for_port_in(home.path(), 9200, data.path()).unwrap();
        let hint = unreachable_node_hint_in(home.path(), "http://127.0.0.1", &|_| PidState::Alive)
            .expect("hint");
        assert!(hint.contains("Process 77 still holds"), "{hint}");
        assert!(hint.contains("still be opening its indices"), "{hint}");
    }

    #[test]
    fn no_pointer_no_lock_or_unknown_liveness_adds_nothing() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        assert!(
            unreachable_node_hint_in(home.path(), "http://localhost:9200", &|_| {
                PidState::Dead
            })
            .is_none()
        );
        publish_data_dir_for_port_in(home.path(), 9200, data.path()).unwrap();
        assert!(
            unreachable_node_hint_in(home.path(), "http://localhost:9200", &|_| {
                PidState::Dead
            })
            .is_none()
        );
        std::fs::write(data.path().join("node.lock"), "12").unwrap();
        assert!(
            unreachable_node_hint_in(home.path(), "http://localhost:9200", &|_| {
                PidState::Unknown
            })
            .is_none()
        );
    }

    #[test]
    fn an_auth_node_pointer_from_older_builds_still_locates_the_dir() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("node.lock"), "5").unwrap();
        let by_port = home.path().join(".xerj/by-port");
        std::fs::create_dir_all(&by_port).unwrap();
        std::fs::write(
            by_port.join("9200"),
            format!("{}\n", data.path().join("admin.key").display()),
        )
        .unwrap();
        let hint =
            unreachable_node_hint_in(home.path(), "http://localhost:9200", &|_| PidState::Dead)
                .expect("hint");
        assert!(hint.contains("pid 5"), "{hint}");
    }

    #[test]
    fn pid_state_sees_this_process_alive_and_a_reaped_child_dead() {
        assert_eq!(pid_state(std::process::id()), PidState::Alive);
        #[cfg(unix)]
        {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            assert_eq!(pid_state(pid), PidState::Dead);
        }
    }
}
