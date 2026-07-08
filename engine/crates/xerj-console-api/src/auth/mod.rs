//! Authentication & identity for Xerj Console.
//!
//! Implements the design from `engine/docs/xerj-console-backend/API_DESIGN_v1.md`
//! §3 — magic-link bootstrap → passkey enrolment → cookie session, with
//! returning-user passkey login. No passwords. API tokens (covered by a
//! later phase) require an enrolled passkey.

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

pub use sessions::{AuthSession, OptionalAuthSession};
