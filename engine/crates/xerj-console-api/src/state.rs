//! Shared state for Xerj Console handlers.
//!
//! `ConsoleState` is `Clone` and lives behind `axum::extract::State`. The
//! types it wraps are individually responsible for their own concurrency
//! (the `Engine` is internally `Arc`-shared, the in-memory caches are
//! `Arc<DashMap<…>>` etc.). This struct is just the join.

use std::sync::Arc;

use dashmap::DashMap;
use xerj_engine::Engine;

use crate::time::now_epoch_ms;

/// Server start time, in epoch ms. Used by `/cluster/info` so the SPA can
/// show "node up since …" without a separate /uptime fetch.
#[derive(Debug, Clone, Copy)]
pub struct StartedAt(pub i64);

/// In-memory record of a WebAuthn challenge that the server issued but
/// the client has not yet completed. Indexed by an opaque challenge id
/// (random 32-byte url-safe base64) that the SPA echoes back on
/// `…/finish`. Auto-expires after 5 minutes; entries past their TTL
/// are pruned on every insert.
#[derive(Debug, Clone)]
pub struct PendingChallenge {
    pub kind: ChallengeKind,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub enum ChallengeKind {
    /// `POST /auth/passkey/begin` — enrolling a new credential. We hold
    /// the registration state plus the user_id we're enrolling for so
    /// `…/finish` knows where to attach the credential.
    PasskeyEnroll {
        user_id: String,
        state: webauthn_rs::prelude::PasskeyRegistration,
    },
    /// `POST /auth/login/begin` — proving an existing credential.
    /// The `email` is null when we returned a fake challenge for an
    /// unknown email (anti-enumeration).
    Login {
        email: Option<String>,
        state: webauthn_rs::prelude::PasskeyAuthentication,
    },
}

/// In-memory record of an enrollment session — issued when a user
/// redeems a magic link, consumed when they finish passkey enrollment.
/// Lives only in RAM (max 30 minutes), not persisted; if the server
/// restarts mid-bootstrap, the user re-redeems with a fresh link.
#[derive(Debug, Clone)]
pub struct EnrollmentSession {
    pub session_id: String,
    pub email: String,
    pub user_id: String,
    pub role: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

/// Coarse "is this node part of a RAFT cluster?" flag, set at startup
/// from `cfg.cluster.enabled` and never mutated. Phase 4 swaps this for
/// a live handle to the cluster runner so `/cluster/raft` can stream
/// real RAFT state; in phase 1 we only need the standalone-vs-raft
/// branch in `/cluster/info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterMode {
    Standalone,
    Raft,
}

/// WebAuthn Relying Party configuration. `rp_id` is the effective domain
/// (e.g. `"localhost"` for dev, `"xerj-console.example.com"` for prod) and
/// `rp_origin` is the URL the SPA was loaded from (e.g.
/// `"http://localhost:9200"`). Both must match between enrolment and
/// login or the browser refuses the assertion.
#[derive(Debug, Clone)]
pub struct RpConfig {
    pub rp_id: String,
    pub rp_origin: String,
    pub rp_name: String,
}

impl Default for RpConfig {
    fn default() -> Self {
        Self {
            rp_id: "localhost".to_string(),
            rp_origin: "http://localhost:9200".to_string(),
            rp_name: "Xerj Console".to_string(),
        }
    }
}

/// Top-level state object shared into every Xerj Console handler.
#[derive(Clone)]
pub struct ConsoleState {
    pub engine: Engine,
    pub started_at: StartedAt,
    pub node_id: Arc<String>,
    pub cluster_mode: ClusterMode,
    pub rp: Arc<RpConfig>,

    /// Live WebAuthn challenges. `pending_challenges[challenge_b64u]
    /// = PendingChallenge`. Capped at 1024 entries; stale entries are
    /// pruned on every insert via `prune_pending_challenges`.
    pub pending_challenges: Arc<DashMap<String, PendingChallenge>>,

    /// Live enrollment sessions (post-magic-link, pre-passkey-finish).
    pub enrollment_sessions: Arc<DashMap<String, EnrollmentSession>>,

    /// Per-IP rate-limit counters for auth endpoints. Reset every minute
    /// on first hit after the window. Map key is `"<ip>:<endpoint>"`.
    pub auth_rate_counters: Arc<DashMap<String, RateWindow>>,

    /// HMAC key used to sign session cookies (derived from the
    /// `data_dir/.xerj_master_key` file or the `XERJ_CONSOLE_KEY`
    /// env var; generated on first start and persisted).
    pub master_key: Arc<[u8; 32]>,
}

#[derive(Debug, Clone, Copy)]
pub struct RateWindow {
    pub count: u32,
    pub window_start_ms: i64,
}

impl ConsoleState {
    pub fn new(
        engine: Engine,
        node_id: String,
        master_key: [u8; 32],
        cluster_mode: ClusterMode,
    ) -> Self {
        Self::new_with_rp(
            engine,
            node_id,
            master_key,
            cluster_mode,
            RpConfig::default(),
        )
    }

    pub fn new_with_rp(
        engine: Engine,
        node_id: String,
        master_key: [u8; 32],
        cluster_mode: ClusterMode,
        rp: RpConfig,
    ) -> Self {
        Self {
            engine,
            started_at: StartedAt(now_epoch_ms()),
            node_id: Arc::new(node_id),
            cluster_mode,
            rp: Arc::new(rp),
            pending_challenges: Arc::new(DashMap::new()),
            enrollment_sessions: Arc::new(DashMap::new()),
            auth_rate_counters: Arc::new(DashMap::new()),
            master_key: Arc::new(master_key),
        }
    }
}

impl std::fmt::Debug for ConsoleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleState")
            .field("node_id", &self.node_id)
            .field("started_at", &self.started_at.0)
            .field("pending_challenges", &self.pending_challenges.len())
            .field("enrollment_sessions", &self.enrollment_sessions.len())
            .finish_non_exhaustive()
    }
}
