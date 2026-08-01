//! Per-IP rate limiter for auth endpoints.
//!
//! Sliding-minute counter, capped at 10 hits per IP per minute and 100
//! per hour. Implementation is intentionally tiny — DashMap of
//! `(ip, endpoint) -> RateWindow`. Stale entries are pruned on every
//! 50th insert; we don't bother with a background task.
//!
//! On limit exceeded we return 429 with no body — leaking whether the
//! email/token was valid would defeat the purpose of rate limiting.
//!
//! The `ip` this keys on must come from [`crate::ClientIp`] — the TCP peer,
//! consulting `x-forwarded-for` only from a declared trusted proxy. Deriving
//! it from headers is what made the limiter bypassable (#76 S5-4); there is
//! deliberately no header-reading helper in this module to reach for.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{ConsoleApiError, ConsoleResult};
use crate::state::{ConsoleState, RateWindow};
use crate::time::now_epoch_ms;

const PER_MINUTE: u32 = 10;
const PER_HOUR: u32 = 100;
const WINDOW_MIN_MS: i64 = 60_000;
const WINDOW_HOUR_MS: i64 = 3_600_000;

/// Counter of insertions, used to drive periodic pruning so we never
/// pay for a full sweep. (DashMap iter is per-shard locking, so a
/// 1-in-50 prune keeps tail latency unaffected.)
static INSERT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Charge one hit against the `(ip, endpoint)` bucket. Returns `Ok(())`
/// if under the limits, `Err(RateLimited)` otherwise.
pub fn charge(state: &ConsoleState, ip: &str, endpoint: &str) -> ConsoleResult<()> {
    let now_ms = now_epoch_ms();

    // Per-minute key
    let key_min = format!("{ip}:{endpoint}:m");
    let win = state
        .auth_rate_counters
        .entry(key_min.clone())
        .or_insert(RateWindow {
            count: 0,
            window_start_ms: now_ms,
        });
    let mut win_val = *win;
    drop(win); // release the entry guard so we can reinsert

    if now_ms - win_val.window_start_ms > WINDOW_MIN_MS {
        win_val = RateWindow {
            count: 0,
            window_start_ms: now_ms,
        };
    }
    win_val.count += 1;
    if win_val.count > PER_MINUTE {
        return Err(ConsoleApiError::RateLimited);
    }
    state.auth_rate_counters.insert(key_min, win_val);

    // Per-hour key
    let key_hour = format!("{ip}:{endpoint}:h");
    let win = state
        .auth_rate_counters
        .entry(key_hour.clone())
        .or_insert(RateWindow {
            count: 0,
            window_start_ms: now_ms,
        });
    let mut win_val = *win;
    drop(win);

    if now_ms - win_val.window_start_ms > WINDOW_HOUR_MS {
        win_val = RateWindow {
            count: 0,
            window_start_ms: now_ms,
        };
    }
    win_val.count += 1;
    if win_val.count > PER_HOUR {
        return Err(ConsoleApiError::RateLimited);
    }
    state.auth_rate_counters.insert(key_hour, win_val);

    if INSERT_COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(50)
    {
        prune(state, now_ms);
    }
    Ok(())
}

fn prune(state: &ConsoleState, now_ms: i64) {
    state
        .auth_rate_counters
        .retain(|_k, v| now_ms - v.window_start_ms < WINDOW_HOUR_MS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ClusterMode;

    fn make_state() -> ConsoleState {
        let dir = tempfile::TempDir::new().unwrap();
        let mut cfg = xerj_common::config::Config::default();
        cfg.server.data_dir = dir.path().to_str().unwrap().to_string();
        let engine = xerj_engine::Engine::new(cfg).unwrap();
        std::mem::forget(dir); // keep the data dir alive until process exit
        ConsoleState::new(engine, "local".into(), [0u8; 32], ClusterMode::Standalone)
    }

    #[tokio::test]
    async fn allows_under_limit() {
        let s = make_state();
        for _ in 0..PER_MINUTE {
            charge(&s, "1.2.3.4", "login").unwrap();
        }
    }

    #[tokio::test]
    async fn blocks_over_minute_limit() {
        let s = make_state();
        for _ in 0..PER_MINUTE {
            charge(&s, "1.2.3.4", "login").unwrap();
        }
        let r = charge(&s, "1.2.3.4", "login");
        assert!(matches!(r, Err(ConsoleApiError::RateLimited)));
    }

    #[tokio::test]
    async fn separate_endpoints_dont_share() {
        let s = make_state();
        for _ in 0..PER_MINUTE {
            charge(&s, "1.2.3.4", "login").unwrap();
        }
        // Different endpoint key — should still be allowed.
        charge(&s, "1.2.3.4", "magic").unwrap();
    }

    #[tokio::test]
    async fn separate_ips_dont_share() {
        let s = make_state();
        for _ in 0..PER_MINUTE {
            charge(&s, "1.2.3.4", "login").unwrap();
        }
        charge(&s, "5.6.7.8", "login").unwrap();
    }
}
