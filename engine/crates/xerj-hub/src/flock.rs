//! The hub's cross-process file lock lives in [`xerj_common::flock`], so the
//! MCP hub cache (`xerj_common::hub_client_cache`) takes the very same lock
//! the automatic node start and `xerj hub pull` take.

pub(crate) use xerj_common::flock::{held_by, FileLock, LockError};
