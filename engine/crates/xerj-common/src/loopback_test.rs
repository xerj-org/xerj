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
