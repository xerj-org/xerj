//! Retention policy and background data cleanup.
//!
//! Partitions older than `retention_days` are eligible for deletion.
//! The background task runs periodically and removes expired data.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use xerj_common::XerjError;

use crate::ingest::HourBucket;

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ─────────────────────────────────────────────────────────────────────────────
// RetentionPolicy
// ─────────────────────────────────────────────────────────────────────────────

/// Controls how long log data is retained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Delete partitions older than this many days.
    /// `None` means retain indefinitely.
    pub retention_days: Option<u32>,
    /// How often to run the cleanup task (in seconds).
    #[serde(default = "default_check_interval_secs")]
    pub check_interval_secs: u64,
}

fn default_check_interval_secs() -> u64 {
    3600 // hourly
}

impl RetentionPolicy {
    /// Create a policy retaining data for `days` days.
    pub fn retain_days(days: u32) -> Self {
        Self {
            retention_days: Some(days),
            check_interval_secs: default_check_interval_secs(),
        }
    }

    /// Create a policy retaining data indefinitely.
    pub fn retain_forever() -> Self {
        Self {
            retention_days: None,
            check_interval_secs: default_check_interval_secs(),
        }
    }

    /// Returns `true` if the given hour bucket has expired under this policy.
    pub fn is_expired(&self, bucket: &HourBucket) -> bool {
        let retention_days = match self.retention_days {
            Some(d) => d,
            None => return false, // retain forever
        };

        let now = Utc::now();
        let cutoff = now - chrono::Duration::days(retention_days as i64);

        // Build an approximate timestamp from the bucket
        let bucket_year = bucket.year as i32;
        let bucket_month = bucket.month as u32;
        let bucket_day = bucket.day as u32;
        let bucket_hour = bucket.hour as u32;

        match chrono::NaiveDate::from_ymd_opt(bucket_year, bucket_month, bucket_day)
            .and_then(|d| d.and_hms_opt(bucket_hour, 0, 0))
            .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
        {
            Some(bucket_dt) => bucket_dt < cutoff,
            None => {
                warn!("could not parse bucket timestamp: {:?}", bucket);
                false
            }
        }
    }

    /// Identify expired buckets from a list of active buckets.
    pub fn expired_buckets(&self, buckets: &[HourBucket]) -> Vec<HourBucket> {
        buckets
            .iter()
            .filter(|b| self.is_expired(b))
            .copied()
            .collect()
    }
}

impl Default for RetentionPolicy {
    /// Default: retain 30 days.
    fn default() -> Self {
        Self::retain_days(30)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RetentionManager
// ─────────────────────────────────────────────────────────────────────────────

/// Applies a retention policy to a set of buckets.
///
/// In production, this would be driven by a background tokio task that
/// periodically calls [`apply`] and deletes the returned bucket paths from
/// object storage.
pub struct RetentionManager {
    policy: RetentionPolicy,
}

impl RetentionManager {
    pub fn new(policy: RetentionPolicy) -> Self {
        Self { policy }
    }

    /// Determine which buckets should be deleted, logging actions taken.
    ///
    /// Returns the list of buckets that should be removed from storage.
    pub fn apply(&self, active_buckets: &[HourBucket]) -> Vec<HourBucket> {
        let expired = self.policy.expired_buckets(active_buckets);

        if expired.is_empty() {
            info!("retention check: no expired partitions");
        } else {
            info!(
                "retention check: {} expired partition(s) to delete",
                expired.len()
            );
            for b in &expired {
                info!("  deleting partition: {}", b);
            }
        }

        expired
    }

    pub fn policy(&self) -> &RetentionPolicy {
        &self.policy
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn old_bucket(days_ago: i64) -> HourBucket {
        let dt = Utc::now() - chrono::Duration::days(days_ago);
        HourBucket {
            year: dt.year() as u16,
            month: dt.month() as u8,
            day: dt.day() as u8,
            hour: dt.hour() as u8,
        }
    }

    use chrono::{Datelike, Timelike};

    #[test]
    fn recent_bucket_not_expired() {
        let policy = RetentionPolicy::retain_days(30);
        let bucket = old_bucket(5); // 5 days ago
        assert!(!policy.is_expired(&bucket));
    }

    #[test]
    fn old_bucket_is_expired() {
        let policy = RetentionPolicy::retain_days(30);
        let bucket = old_bucket(40); // 40 days ago
        assert!(policy.is_expired(&bucket));
    }

    #[test]
    fn retain_forever_never_expires() {
        let policy = RetentionPolicy::retain_forever();
        let bucket = old_bucket(3650); // 10 years ago
        assert!(!policy.is_expired(&bucket));
    }

    #[test]
    fn expired_buckets_returns_correct_subset() {
        let policy = RetentionPolicy::retain_days(30);
        let buckets = vec![
            old_bucket(5),  // recent
            old_bucket(40), // expired
            old_bucket(60), // expired
            old_bucket(10), // recent
        ];
        let expired = policy.expired_buckets(&buckets);
        assert_eq!(expired.len(), 2);
    }

    #[test]
    fn manager_apply_returns_expired() {
        let policy = RetentionPolicy::retain_days(30);
        let manager = RetentionManager::new(policy);
        let buckets = vec![old_bucket(5), old_bucket(45)];
        let to_delete = manager.apply(&buckets);
        assert_eq!(to_delete.len(), 1);
    }
}
