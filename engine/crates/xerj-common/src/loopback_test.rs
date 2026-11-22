//! Loopback binds for the workspace's tests.
//!
//! Many tests stand up a fake node or server on an ephemeral 127.0.0.1 port.
//! In a sandbox that forbids loopback networking they used to fail with a
//! bare `PermissionDenied` from `unwrap()`, hundreds at a time, which reads as
//! a broken build (#2886). [`expect_loopback`] names the cause instead.
//!
//! Test support only: it lives here, not behind `cfg(test)`, so that every
//! crate's unit and integration tests can reach it.

use std::io;

/// Unwraps a test's bind of 127.0.0.1, std or tokio:
///
/// ```ignore
/// let l = expect_loopback(std::net::TcpListener::bind("127.0.0.1:0"));
/// let l = expect_loopback(tokio::net::TcpListener::bind("127.0.0.1:0").await);
/// ```
///
/// On failure it panics, at the caller, with [`bind_failure`]'s message.
#[track_caller]
pub fn expect_loopback<T>(bound: io::Result<T>) -> T {
    match bound {
        Ok(listener) => listener,
        Err(e) => panic!("{}", bind_failure(&e)),
    }
}

/// The panic message for a failed loopback bind. A denied bind (a sandbox) and
/// an unavailable address (a network namespace with loopback down) both mean
/// the environment has no loopback networking, not that the code is broken.
pub fn bind_failure(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::AddrNotAvailable => format!(
            "loopback bind to 127.0.0.1 failed ({e}): this test starts a fake server on \
             127.0.0.1 and needs loopback networking. Run the tests where binding \
             127.0.0.1 is allowed."
        ),
        _ => format!("bind 127.0.0.1: {e}"),
    }
}

/// Three consecutive loopback ports (`base`, `base + 1`, `base + 2`), all held
/// by this value until it is released or dropped. A booted node takes them as
/// ES-compat, native REST and gRPC (`--port base`).
pub struct PortTriple {
    base: u16,
    _held: [std::net::TcpListener; 3],
}

impl PortTriple {
    /// The first of the three ports.
    pub fn base(&self) -> u16 {
        self.base
    }

    /// Frees the three ports for the child that must bind them, and returns
    /// the base. Call it as late as possible, right before the spawn.
    pub fn release(self) -> u16 {
        self.base
    }
}

/// Holds three consecutive free loopback ports; see [`PortTriple`].
///
/// Bases come from below the kernel's ephemeral range, not from `bind(:0)`.
/// The old helpers drew a base from `:0` and probed `base + 1` and `base + 2`,
/// and at load 40 all 64 or 200 tries failed ("no free port triple"): Linux
/// hands `bind(:0)` ports of one parity and gives `connect()` the other, so
/// under parallel tests `base + 1` is usually a client socket or its
/// TIME_WAIT. Below the ephemeral range no outgoing connection ever takes a
/// port, so neither the probe nor the gap between release and the child's
/// bind can lose it to one. Within the process no base is handed out twice.
///
/// `XERJ_TEST_PORT_BASE=N` confines bases to `N..=N+97` on a shared machine.
/// Otherwise it falls back to `bind(:0)` bases only when the host's ephemeral
/// range leaves no room below it. The search stops after 30 s, which on a healthy host is
/// never reached: each try is three non-blocking binds.
pub fn free_port_triple() -> PortTriple {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::Mutex;
    static GIVEN: Mutex<Vec<u16>> = Mutex::new(Vec::new());

    let range = std::env::var("XERJ_TEST_PORT_BASE")
        .ok()
        .and_then(|b| b.parse::<u16>().ok())
        .filter(|b| (1025..=u16::MAX - 100).contains(b))
        .map(|b| (b, b + 97))
        .or_else(below_ephemeral_range);
    let seed = std::collections::hash_map::RandomState::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut last_err = None;
    for attempt in 0u64.. {
        if std::time::Instant::now() > deadline {
            break;
        }
        let base = match range {
            Some((lo, hi)) => {
                let mut h = seed.build_hasher();
                h.write_u64(attempt);
                lo + (h.finish() % u64::from(hi - lo + 1)) as u16
            }
            None => {
                let probe = expect_loopback(std::net::TcpListener::bind("127.0.0.1:0"));
                let p = probe.local_addr().map(|a| a.port()).unwrap_or(0);
                if !(1025..=u16::MAX - 2).contains(&p) {
                    continue;
                }
                p
            }
        };
        {
            let given = GIVEN.lock().unwrap_or_else(|e| e.into_inner());
            if given.iter().any(|g| g.abs_diff(base) < 3) {
                continue;
            }
        }
        let bind = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p));
        match (bind(base), bind(base + 1), bind(base + 2)) {
            (Ok(a), Ok(b), Ok(c)) => {
                GIVEN.lock().unwrap_or_else(|e| e.into_inner()).push(base);
                return PortTriple {
                    base,
                    _held: [a, b, c],
                };
            }
            (a, b, c) => {
                for r in [a, b, c] {
                    if let Err(e) = r {
                        if matches!(
                            e.kind(),
                            io::ErrorKind::PermissionDenied | io::ErrorKind::AddrNotAvailable
                        ) {
                            panic!("{}", bind_failure(&e));
                        }
                        last_err = Some(e);
                    }
                }
            }
        }
    }
    panic!(
        "no free port triple in 30 s (range {range:?}, last error {last_err:?}): every \
         candidate base had one of its three ports taken"
    );
}

/// `[10000, ephemeral_low - 3]`, where bases are safe from `connect()`.
/// `None` when the host's ephemeral range starts too low to leave room.
fn below_ephemeral_range() -> Option<(u16, u16)> {
    const LOW: u16 = 10_000;
    // Linux default; macOS and Windows start theirs at 49152, so 32768 is
    // below the ephemeral range there too.
    let mut ephemeral_low: u16 = 32_768;
    if let Ok(text) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        if let Some(first) = text.split_whitespace().next().and_then(|s| s.parse().ok()) {
            ephemeral_low = first;
        }
    }
    let hi = ephemeral_low.checked_sub(3)?;
    (hi >= LOW + 1_000).then_some((LOW, hi))
}

/// Fails the test, naming the fix, unless `addr` (a loopback address other
/// than 127.0.0.1, such as 127.0.0.2) is usable on this host. Linux routes all
/// of 127.0.0.0/8 to loopback; stock macOS configures only 127.0.0.1, and a
/// connection to another address fails with an error that does not say why
/// (#2903). Call it at the top of a test that connects to such an address.
#[track_caller]
pub fn require_loopback_alias(addr: std::net::Ipv4Addr) {
    if let Err(e) = std::net::TcpListener::bind((addr, 0)) {
        panic!("{}", alias_failure(addr, &e));
    }
}

/// The panic message for a loopback address the host cannot bind.
pub fn alias_failure(addr: std::net::Ipv4Addr, e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::AddrNotAvailable => format!(
            "loopback address {addr} is not configured on this host ({e}): this test \
             connects to {addr} as a second loopback address. Linux has it by default; \
             on macOS add it with `sudo ifconfig lo0 alias {addr} up`."
        ),
        io::ErrorKind::PermissionDenied => format!(
            "loopback bind to {addr} failed ({e}): this test needs loopback networking. \
             Run the tests where binding {addr} is allowed."
        ),
        _ => format!("bind {addr}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_or_unavailable_loopback_bind_names_the_requirement() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::AddrNotAvailable,
        ] {
            let msg = bind_failure(&io::Error::from(kind));
            assert!(msg.contains("needs loopback networking"), "{kind:?}: {msg}");
            assert!(msg.contains("127.0.0.1"), "{kind:?}: {msg}");
        }
        let other = bind_failure(&io::Error::from(io::ErrorKind::Other));
        assert!(!other.contains("needs loopback networking"), "{other}");
    }

    #[test]
    #[should_panic(expected = "needs loopback networking")]
    fn expect_loopback_panics_with_the_message() {
        let denied: io::Result<()> = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        expect_loopback(denied);
    }

    #[test]
    fn expect_loopback_passes_a_listener_through() {
        assert_eq!(expect_loopback(Ok::<u16, io::Error>(7)), 7);
    }

    #[test]
    fn a_missing_loopback_alias_names_the_address_and_the_fix() {
        let addr = std::net::Ipv4Addr::new(127, 0, 0, 2);
        let msg = alias_failure(addr, &io::Error::from(io::ErrorKind::AddrNotAvailable));
        assert!(msg.contains("127.0.0.2 is not configured"), "{msg}");
        assert!(msg.contains("ifconfig lo0 alias 127.0.0.2"), "{msg}");
        let denied = alias_failure(addr, &io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(denied.contains("needs loopback networking"), "{denied}");
        assert!(!denied.contains("ifconfig"), "{denied}");
        let other = alias_failure(addr, &io::Error::from(io::ErrorKind::Other));
        assert!(!other.contains("ifconfig"), "{other}");
    }

    #[test]
    fn a_port_triple_is_held_until_released_and_never_handed_out_twice() {
        let a = free_port_triple();
        let b = free_port_triple();
        assert!(
            a.base().abs_diff(b.base()) >= 3,
            "{} {}",
            a.base(),
            b.base()
        );
        for p in a.base()..=a.base() + 2 {
            assert!(
                std::net::TcpListener::bind(("127.0.0.1", p)).is_err(),
                "{p} is not held"
            );
        }
        if let (Err(_), Some((lo, hi))) = (
            std::env::var("XERJ_TEST_PORT_BASE"),
            below_ephemeral_range(),
        ) {
            assert!(
                (lo..=hi).contains(&a.base()),
                "{} not in {lo}..={hi}",
                a.base()
            );
        }
        let base = a.release();
        for p in base..=base + 2 {
            expect_loopback(std::net::TcpListener::bind(("127.0.0.1", p)));
        }
        drop(b);
    }

    #[test]
    fn require_loopback_alias_accepts_127_0_0_1() {
        require_loopback_alias(std::net::Ipv4Addr::LOCALHOST);
    }

    /// A non-loopback documentation address (TEST-NET-1) is never configured,
    /// so it stands in for macOS's missing 127.0.0.2.
    #[test]
    #[should_panic(expected = "is not configured on this host")]
    fn require_loopback_alias_panics_for_an_unconfigured_address() {
        require_loopback_alias(std::net::Ipv4Addr::new(192, 0, 2, 1));
    }
}
