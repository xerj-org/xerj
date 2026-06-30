//! Xerj Console backend API.
//!
//! Mounted at `/_xerj-console/api/v1/*` from the bundled xerj binary. Implements
//! the surface designed in `engine/docs/xerj-console-backend/API_DESIGN_v1.md` —
//! authentication (passkeys, magic links), cluster awareness, preferences,
//! dashboards, saved views, and the pluggable data-source layer.
//!
//! ## Module layout
//!
//! Boot + cluster info:
//! - [`error`], [`response`], [`time`] — primitives.
//! - [`state`] — `ConsoleState` shared into every handler.
//! - [`indices`] — `.xerj_*` system index names + schemas.
//! - [`bootstrap`] — first-launch detection, master-key persistence,
//!   magic-link printout.
//! - [`cluster`] — `/cluster/info`.
//!
//! Auth:
//! - [`auth`] — magic-link redeem, passkey enrol, login, sessions,
//!   audit, rate limits, `/me`, passkey CRUD.
//!
//! User-state (drop the localStorage hacks):
//! - [`prefs`] — `GET/PUT /prefs`.
//! - [`dashboards`] — full CRUD with If-Match etag concurrency.
//! - [`views`] — saved views CRUD.
//! - [`data_sources`] — `/data-sources/connections` + descendants
//!   (read-only MVP backed by an auto-provisioned `built-in` adapter).
//!
//! Coming after RC: `/auth/magic/issue`, `/auth/api-tokens`, `/users`
//! admin surface, `/cluster/raft*`, dashboards/views SSE streams, write
//! paths for /data-sources/connections (alongside encryption-at-rest
//! for connection secrets).
//!
//! ## Why this is a separate crate
//!
//! Xerj Console is its own product. xerj is one possible backend (the default
//! in v1.0); the crate must extract cleanly post-v1.0 to target ES,
//! OpenSearch, Prometheus, Postgres, or remote xerj.

#![allow(clippy::module_name_repetitions)]

pub mod auth;
pub mod bootstrap;
pub mod cluster;
pub mod dashboards;
pub mod data_sources;
pub mod error;
pub mod indices;
pub mod prefs;
pub mod response;
pub mod router;
pub mod spa;
pub mod state;
pub mod time;
pub mod views;

pub use error::{ConsoleApiError, ConsoleResult};
pub use router::xerj_console_router;
pub use state::ConsoleState;
