//! Test-only helpers shared by this crate's unit-test modules.
//!
//! The unit tests of every module run in ONE process, on parallel threads, so a
//! process-global environment variable is shared state. `XERJ_HUB_PUBKEY`
//! decides which key verifies catalogues and packs
//! ([`xerj_common::hub::hub_pubkey`]); a test that sets it while another test
//! reads it makes the second one verify against the wrong key. Each module
//! used to keep a private lock, so tests in different modules still raced
//! (a `cli` test was deleted in d26ecf43 for exactly that). [`HubPubkeyEnv`]
//! is the one lock, and `no_other_module_writes_the_hub_pubkey_var` keeps it
//! the only writer.

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

const VAR: &str = "XERJ_HUB_PUBKEY";

static HUB_PUBKEY_LOCK: Mutex<()> = Mutex::new(());

/// Exclusive use of `XERJ_HUB_PUBKEY` for one test. Taking it waits for every
/// other holder; dropping it puts back the value the variable had before, even
/// when the test panics, so one failing test cannot leave a throwaway key
/// behind for the next. A poisoned lock is recovered: the restore on drop is
/// what makes the state safe again, not the poison flag.
pub(crate) struct HubPubkeyEnv {
    prev: Option<OsString>,
    // Dropped after `Drop::drop` has restored the variable.
    _lock: MutexGuard<'static, ()>,
}

impl HubPubkeyEnv {
    pub(crate) fn lock() -> Self {
        let lock = HUB_PUBKEY_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        HubPubkeyEnv {
            prev: std::env::var_os(VAR),
            _lock: lock,
        }
    }

    /// Set `XERJ_HUB_PUBKEY` for the rest of this hold.
    pub(crate) fn set(&self, value: impl AsRef<OsStr>) {
        std::env::set_var(VAR, value);
    }

    /// Unset `XERJ_HUB_PUBKEY`: the compiled-in key applies.
    pub(crate) fn remove(&self) {
        std::env::remove_var(VAR);
    }
}

impl Drop for HubPubkeyEnv {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_restores_the_previous_value_even_after_a_panic() {
        let outer = HubPubkeyEnv::lock();
        let orig = outer.prev.clone();
        outer.set("outer");
        // Blocks on the lock until `outer` is dropped, then panics while
        // holding it with a throwaway value set (and poisons the lock).
        let t = std::thread::spawn(|| {
            let inner = HubPubkeyEnv::lock();
            inner.set("inner");
            panic!("a failing test that set {VAR}");
        });
        drop(outer);
        assert!(t.join().is_err());
        // The poisoned lock is recovered, and neither "outer" nor "inner" is
        // left behind: the variable is what it was before either test.
        let after = HubPubkeyEnv::lock();
        assert_eq!(after.prev, orig);
    }

    /// Every write to `XERJ_HUB_PUBKEY` in this crate's sources goes through
    /// [`HubPubkeyEnv`]. A bare `std::env::set_var` elsewhere would race the
    /// tests that hold the lock, which is the bug this module exists to end.
    #[test]
    fn no_other_module_writes_the_hub_pubkey_var() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension() != Some(OsStr::new("rs")) || p.ends_with("test_support.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let writes = line.contains("set_var(") || line.contains("remove_var(");
                    if writes && line.contains(VAR) {
                        offenders.push(format!("{}:{}", p.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "write {VAR} through test_support::HubPubkeyEnv: {offenders:?}"
        );
    }
}
