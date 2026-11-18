//! Authentication & identity for Xerj Console.
//!
//! Implements the design from `engine/docs/xerj-console-backend/API_DESIGN_v1.md`
//! §3 — magic-link bootstrap → passkey enrolment → cookie session, with
//! returning-user passkey login. No passwords. Minting an API token, issuing
//! an invitation, deleting a passkey, or logging out requires a
//! [`PasskeySession`]; a bearer API token is 403 on those routes
//! (CVE-2026-97406 / CERT VU#645818).

pub mod audit;
pub mod login;
pub mod magic;
pub mod me;
pub mod passkey;
pub mod rate_limit;
pub mod sessions;
pub mod store;
pub mod tokens;
pub mod webauthn_setup;

pub use sessions::{AuthSession, OptionalAuthSession, PasskeySession};
