//! Loopback listeners for this crate's tests: the fake nodes bind through
//! [`bind`], so a sandbox that denies loopback gets a message naming it
//! instead of a bare `PermissionDenied` (#2886).

use std::net::TcpListener;

/// Binds an ephemeral port on 127.0.0.1, or panics with a message that says
/// the tests need loopback networking.
#[track_caller]
pub(crate) fn bind() -> TcpListener {
    xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"))
}
