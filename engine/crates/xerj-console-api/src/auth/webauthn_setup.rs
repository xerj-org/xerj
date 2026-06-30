//! WebAuthn relying-party setup.
//!
//! Builds a `Webauthn` instance from `ConsoleState.rp` (rp_id, rp_origin,
//! rp_name). Cheap enough to call per-request — `Webauthn::new` just
//! stores config; no I/O.

use webauthn_rs::prelude::*;

use crate::error::{ConsoleApiError, ConsoleResult};
use crate::state::ConsoleState;

pub fn build(state: &ConsoleState) -> ConsoleResult<Webauthn> {
    let origin = Url::parse(&state.rp.rp_origin)
        .map_err(|e| ConsoleApiError::Internal(format!("rp_origin parse: {e}")))?;
    let builder = WebauthnBuilder::new(&state.rp.rp_id, &origin)
        .map_err(|e| ConsoleApiError::Internal(format!("webauthn build: {e}")))?
        .rp_name(&state.rp.rp_name);
    builder
        .build()
        .map_err(|e| ConsoleApiError::Internal(format!("webauthn finalise: {e}")))
}
