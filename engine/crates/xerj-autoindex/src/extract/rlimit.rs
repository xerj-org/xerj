//! Address-space cap for isolated extraction workers (`RLIMIT_AS`).
//!
//! The cap is crash isolation: a parser that allocates until it dies takes
//! the worker with it, and the parent keeps scanning. It is not a security
//! boundary. Some kernels reject the resource outright — macOS 27 returns
//! `EINVAL` for `setrlimit(RLIMIT_AS, 1536 MiB)` and for `ulimit -v` — and
//! an unprivileged process gets `EPERM` when the requested hard limit is
//! above the one it inherited. Either refusal used to fail `Command::spawn`
//! inside `pre_exec`, so every PDF (and every isolated `.xls`) was junk
//! before `exec` (#2336).
//!
//! `EINVAL` and `EPERM` are skipped. The worker still starts, in its own
//! process group, under whatever limit it inherited. Any other errno still
//! fails that one spawn. Linux keeps the cap: the probe there succeeds and
//! the child installs it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Byte value the parent probes with. Matches the PDF worker cap. A kernel
/// that rejects this call rejects the resource; the `.xls` worker uses the
/// same answer and still installs its own (larger) ceiling when the probe
/// says the call works.
const PROBE_BYTES: u64 = 1536 << 20;

/// One-line progress note. Present only after a probe has observed a refusal,
/// and only once per process.
pub(crate) const ADDRESS_SPACE_CAP_WARNING: &str = "\
warning: this OS refused setrlimit(RLIMIT_AS) for isolated extraction workers; \
they still start without that address-space cap. The cap is crash isolation, \
not a security boundary.";

static CAP_SUPPORTED: OnceLock<bool> = OnceLock::new();
static CAP_WARNING_ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// What one `setrlimit(RLIMIT_AS)` call did. `Skipped` is a successful spawn
/// decision: the platform will not install the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressSpaceCap {
    Applied,
    Skipped,
    Failed(i32),
}

/// `EINVAL` (resource rejected) and `EPERM` (cannot raise the hard limit).
pub(crate) fn refusal_is_benign(errno: i32) -> bool {
    errno == libc::EINVAL || errno == libc::EPERM
}

pub(crate) fn classify_address_space_cap(rc: i32, errno: i32) -> AddressSpaceCap {
    if rc == 0 {
        AddressSpaceCap::Applied
    } else if refusal_is_benign(errno) {
        AddressSpaceCap::Skipped
    } else {
        AddressSpaceCap::Failed(errno)
    }
}

/// Install `bytes` as both the soft and hard `RLIMIT_AS`. Safe to call from
/// `pre_exec`: on the refusal path it reads `errno` and does not allocate.
pub(crate) fn apply_address_space_cap(bytes: u64) -> AddressSpaceCap {
    let limit = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: bytes as libc::rlim_t,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) };
    // `errno` is only meaningful when the call failed. A success leaves it
    // stale, and [`classify_address_space_cap`] ignores it when `rc == 0`.
    classify_address_space_cap(rc, if rc == 0 { 0 } else { errno_raw() })
}

/// `true` when this OS accepted a 1536 MiB `RLIMIT_AS` in a forked child.
///
/// The probe runs in the parent, once. The child calls `setrlimit` and
/// `_exit` only — both async-signal-safe — so a fork from the multithreaded
/// indexer does not touch the Rust allocator. The parent's own limit is not
/// modified. An inconclusive probe (fork failure, or an errno other than
/// `EINVAL`/`EPERM`) answers `true`, and the worker's `pre_exec` still skips
/// a later benign refusal.
pub(crate) fn address_space_cap_supported() -> bool {
    *CAP_SUPPORTED.get_or_init(probe_address_space_cap)
}

/// The progress note for a refused cap, the first time it is asked after the
/// probe has run and failed. `None` before the probe, when the cap was
/// accepted, and on every call after the first announcement.
pub(crate) fn take_address_space_cap_warning() -> Option<&'static str> {
    let supported = *CAP_SUPPORTED.get()?;
    warning_if_refused(supported, &CAP_WARNING_ANNOUNCED)
}

pub(crate) fn warning_if_refused(supported: bool, announced: &AtomicBool) -> Option<&'static str> {
    if supported || announced.swap(true, Ordering::SeqCst) {
        return None;
    }
    Some(ADDRESS_SPACE_CAP_WARNING)
}

/// Whether the calling process currently has `RLIMIT_AS` pinned to `bytes`.
/// The worker reports this in its containment string, so a platform that
/// refused the cap does not claim the cap is in force.
pub(crate) fn current_address_space_is_capped(bytes: u64) -> bool {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut current) } != 0 {
        return false;
    }
    current.rlim_cur == bytes as libc::rlim_t && current.rlim_max == bytes as libc::rlim_t
}

pub(crate) fn errno_raw() -> i32 {
    // `__error` on the BSDs (including macOS); `__errno_location` on Linux
    // and Android. Both are async-signal-safe. OpenBSD's `___errno` is not
    // covered; this crate's CI is Linux, macOS, and Windows.
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "watchos",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd"
    ))]
    unsafe {
        *libc::__error()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "watchos",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd"
    )))]
    unsafe {
        *libc::__errno_location()
    }
}

fn probe_address_space_cap() -> bool {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return true;
    }
    if pid == 0 {
        let code = match apply_address_space_cap(PROBE_BYTES) {
            AddressSpaceCap::Applied => 0,
            AddressSpaceCap::Skipped => 2,
            AddressSpaceCap::Failed(_) => 3,
        };
        unsafe { libc::_exit(code) };
    }
    let mut status: libc::c_int = 0;
    if unsafe { libc::waitpid(pid, &mut status, 0) } != pid {
        return true;
    }
    // Exit 2 is the only definitive refusal. Anything else (applied, an
    // unexpected errno, a signal) still attempts the cap in the worker.
    if libc::WIFEXITED(status) {
        return libc::WEXITSTATUS(status) != 2;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn einval_and_eperm_skip_the_cap_and_other_errnos_fail_the_spawn() {
        assert_eq!(
            classify_address_space_cap(0, libc::EINVAL),
            AddressSpaceCap::Applied,
            "a successful setrlimit ignores a stale errno"
        );
        assert_eq!(
            classify_address_space_cap(-1, libc::EINVAL),
            AddressSpaceCap::Skipped
        );
        assert_eq!(
            classify_address_space_cap(-1, libc::EPERM),
            AddressSpaceCap::Skipped
        );
        assert_eq!(
            classify_address_space_cap(-1, libc::EFAULT),
            AddressSpaceCap::Failed(libc::EFAULT)
        );
        assert_eq!(
            classify_address_space_cap(-1, libc::ENOMEM),
            AddressSpaceCap::Failed(libc::ENOMEM)
        );
    }

    #[test]
    fn the_refusal_warning_is_announced_once_and_only_when_the_cap_was_refused() {
        let announced = AtomicBool::new(false);
        assert!(warning_if_refused(true, &announced).is_none());
        assert!(
            !announced.load(Ordering::SeqCst),
            "an accepted cap is not a warning"
        );
        let first = warning_if_refused(false, &announced).unwrap();
        assert!(first.contains("setrlimit(RLIMIT_AS)"));
        assert!(first.contains("still start"));
        assert!(warning_if_refused(false, &announced).is_none());
    }

    #[test]
    fn probing_the_cap_does_not_change_the_parent_limit() {
        let mut before = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut before) }, 0);
        let supported = address_space_cap_supported();
        let mut after = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut after) }, 0);
        assert_eq!(before.rlim_cur, after.rlim_cur);
        assert_eq!(before.rlim_max, after.rlim_max);
        // Linux accepts the call. A refusal here would mean this runner
        // cannot install the cap the PDF worker documents, which is the
        // macOS 27 failure mode and must not be silent on Linux CI.
        #[cfg(target_os = "linux")]
        assert!(supported, "Linux accepts setrlimit(RLIMIT_AS, 1536 MiB)");
        #[cfg(not(target_os = "linux"))]
        let _ = supported;
    }
}
