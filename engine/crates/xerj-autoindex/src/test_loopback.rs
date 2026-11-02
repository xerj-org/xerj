//! Loopback listeners for this crate's tests.
//!
//! Most tests here stand up a fake node on an ephemeral 127.0.0.1 port. In a
//! sandbox that forbids loopback networking every one of them used to fail
//! with a bare `PermissionDenied` from `unwrap()`, which reads as a broken
//! build. [`bind`] names the cause instead (#2886).

use std::io;
use std::net::TcpListener;

/// Binds an ephemeral port on 127.0.0.1, or panics with a message that says
/// the tests need loopback networking.
pub(crate) fn bind() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{}", bind_failure(&e)))
}

/// The panic message for a failed loopback bind. A denied bind (a sandbox) and
/// an unavailable address (a network namespace with loopback down) both mean
/// the environment has no loopback networking, not that the code is broken.
pub(crate) fn bind_failure(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::AddrNotAvailable => format!(
            "loopback bind to 127.0.0.1:0 failed ({e}): the xerj-autoindex tests start fake \
             nodes on 127.0.0.1 and need loopback networking. Run them where binding \
             127.0.0.1 is allowed."
        ),
        _ => format!("bind 127.0.0.1:0: {e}"),
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
            assert!(msg.contains("need loopback networking"), "{kind:?}: {msg}");
            assert!(msg.contains("127.0.0.1"), "{kind:?}: {msg}");
        }
        let other = bind_failure(&io::Error::from(io::ErrorKind::Other));
        assert!(!other.contains("need loopback networking"), "{other}");
    }
}
