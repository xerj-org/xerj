//! Live database sources: connect once, the index stays current.
//!
//! `xerj connect postgres://user@host/db` (or `PUT /_xerj/sources/{name}`)
//! registers a source inside a running node. The node keeps it in
//! `<data_dir>/_sources/<name>/`, starts a worker for it at boot, and the
//! worker takes a consistent snapshot of the selected tables, then follows
//! every insert, update, delete and truncate through Postgres logical
//! replication, acknowledging an LSN only after its changes are applied.
//! `GET /_xerj/sources/{name}` and `xerj sources` show where it is.
//!
//! This crate is engine-agnostic: changes leave through [`change::Sink`],
//! which the node implements in-process (`xerj-api/src/sources_api.rs`).

pub mod change;
pub mod cli;
pub mod pg;
pub mod spec;
pub mod status;
pub mod store;
pub mod worker;

pub use change::{FactsObserver, Op, RowChange, RowChangeKind, Sink};
pub use spec::{Mode, SourceSpec};
pub use status::{Shared, State, Status};
pub use store::{PersistedState, Phase, Store};
pub use worker::Worker;
