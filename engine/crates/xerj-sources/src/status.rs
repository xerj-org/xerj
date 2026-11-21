//! What `GET /_xerj/sources/{name}` and `xerj sources` show.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Connecting,
    Snapshotting,
    Streaming,
    /// Streaming, but more than [`LAG_SECS`] behind the database.
    Lagging,
    Polling,
    Error,
    Stopped,
}

pub const LAG_SECS: f64 = 30.0;

#[derive(Debug, Clone, Serialize, Default)]
pub struct TableStatus {
    pub table: String,
    pub index: String,
    pub rows_indexed: u64,
    /// The planner's estimate (`pg_class.reltuples`), for the snapshot ETA.
    pub rows_estimate: u64,
    pub snapshot_done: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub rows_done: u64,
    pub rows_estimate: u64,
    pub percent: f64,
    pub eta_secs: Option<u64>,
    pub rows_per_sec: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stream {
    /// Last LSN applied to the index and acknowledged to the server.
    pub confirmed_lsn: String,
    /// Newest WAL position the server reported.
    pub server_lsn: String,
    /// Seconds between the commit of the newest change the server has and
    /// now, while changes are still unapplied; 0 when caught up.
    pub lag_secs: f64,
    pub rows_per_sec: f64,
    pub inserts: u64,
    pub updates: u64,
    pub deletes: u64,
    pub truncates: u64,
    /// p50/p95 of commit-in-database to applied-in-XERJ, over the last 1,000
    /// transactions, milliseconds.
    pub apply_latency_ms_p50: Option<f64>,
    pub apply_latency_ms_p95: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ErrorInfo {
    pub message: String,
    pub hint: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub state: State,
    /// `logical` or `poll`.
    pub mode: String,
    pub tables: Vec<TableStatus>,
    /// Tables the filter selected but the source cannot follow, and why.
    pub skipped: Vec<String>,
    pub snapshot: Snapshot,
    pub stream: Stream,
    pub error: Option<ErrorInfo>,
    /// Facts, schema changes and other things an operator should read once.
    pub notes: Vec<String>,
    pub started_at: String,
    #[serde(skip)]
    pub latencies: Vec<f64>,
}

impl Status {
    pub fn note(&mut self, s: impl Into<String>) {
        let s = s.into();
        tracing::info!(target: "xerj_sources", "{s}");
        if self.notes.len() >= 50 {
            self.notes.remove(0);
        }
        self.notes.push(s);
    }

    pub fn record_latency(&mut self, ms: f64) {
        if self.latencies.len() >= 1000 {
            self.latencies.remove(0);
        }
        self.latencies.push(ms);
        let mut v = self.latencies.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
        self.stream.apply_latency_ms_p50 = Some(q(0.5));
        self.stream.apply_latency_ms_p95 = Some(q(0.95));
    }
}

pub type Shared = Arc<Mutex<Status>>;

/// Exponentially weighted rows/s.
#[derive(Debug)]
pub struct Rate {
    last: Instant,
    rate: f64,
}

impl Default for Rate {
    fn default() -> Self {
        Self {
            last: Instant::now(),
            rate: 0.0,
        }
    }
}

impl Rate {
    pub fn add(&mut self, n: u64) -> f64 {
        let dt = self.last.elapsed().as_secs_f64().max(1e-3);
        self.last = Instant::now();
        let inst = n as f64 / dt;
        let a = (dt / 5.0).min(1.0);
        self.rate = self.rate * (1.0 - a) + inst * a;
        self.rate
    }
    pub fn decay(&mut self) -> f64 {
        self.add(0)
    }
}

pub fn lsn_str(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn & 0xffff_ffff)
}

pub fn parse_lsn(s: &str) -> Option<u64> {
    let (a, b) = s.split_once('/')?;
    Some((u64::from_str_radix(a, 16).ok()? << 32) | u64::from_str_radix(b, 16).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsn_text_round_trips() {
        assert_eq!(lsn_str(0x016B_3748), "0/16B3748");
        assert_eq!(parse_lsn("1/2A"), Some((1 << 32) | 0x2A));
        assert_eq!(parse_lsn(&lsn_str(123456789012)), Some(123456789012));
    }
}
