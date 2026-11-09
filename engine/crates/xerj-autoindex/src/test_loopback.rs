//! Loopback listeners for this crate's tests: the fake nodes bind through
//! [`bind`], so a sandbox that denies loopback gets a message naming it
//! instead of a bare `PermissionDenied` (#2886).

use std::net::TcpListener;

/// Identity returned by the real node's GET /, independent of wire version.
pub(crate) const XERJ_ROOT: &str = r#"{"cluster_uuid":"xerj-cluster-0000-0000-0000-000000000000"}"#;

pub(crate) fn xerj_root() -> serde_json::Value {
    serde_json::from_str(XERJ_ROOT).unwrap()
}

/// Binds an ephemeral port on 127.0.0.1, or panics with a message that says
/// the tests need loopback networking.
#[track_caller]
pub(crate) fn bind() -> TcpListener {
    xerj_common::loopback_test::expect_loopback(TcpListener::bind("127.0.0.1:0"))
}
