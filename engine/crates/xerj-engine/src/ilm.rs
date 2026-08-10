//! Index lifecycle management (ILM) — the *executor*.
//!
//! # Why this module exists
//!
//! Before this module, `PUT /_ilm/policy/{name}` stored a policy in a
//! `DashMap` and nothing ever ran it (issue #199). A user migrating from
//! Elasticsearch did:
//!
//! ```text
//! PUT _ilm/policy/logs-30d   -> 200 OK
//! GET _ilm/policy/logs-30d   -> their policy, exactly as written
//! ```
//!
//! …concluded that retention was configured, and got an index that grows
//! forever. That is worse than a 404: an endpoint that refuses tells the
//! truth, an endpoint that accepts and ignores produces an unbounded disk
//! bill and an unmeetable "we delete logs after 30 days" compliance claim.
//!
//! This module closes that in the two ways that are both honest:
//!
//!  * **Execute what we can.** A background pass ages every ILM-managed index
//!    and applies the phases XERJ can genuinely perform — `delete` (drop the
//!    index) and `readonly` (set the write block).
//!  * **Refuse what we cannot.** [`validate_policy`] is an *allowlist*: a
//!    policy naming an action this engine does not execute is rejected at
//!    `PUT` time with the action named. Nothing is accepted-and-ignored.
//!
//! # Prior art (reference-coding mandate)
//!
//! quickwit's janitor solves the same shape of problem and its choices are
//! the ones adopted here (Apache-2.0, adapted — not copied):
//!
//!  * `quickwit-janitor/src/actors/retention_policy_executor.rs:35` —
//!    retention runs on a coarse periodic loop (`RUN_INTERVAL = 1 hour`), not
//!    a per-index timer. Ours defaults to 10 minutes (ES's
//!    `indices.lifecycle.poll_interval`) and is configurable.
//!  * `quickwit-janitor/src/actors/retention_policy_executor.rs:78-80` —
//!    "Should not return an error to prevent the actor from crashing": the
//!    loop logs and continues. [`Engine::run_ilm_once`] returns a report and
//!    never propagates an error out of the tick.
//!  * `quickwit-janitor/src/retention_policy_execution.rs:65-76` — splits that
//!    *cannot* be evaluated (no timestamp range) are **warned about and
//!    skipped, never deleted**. Same rule here: an index whose age cannot be
//!    established is skipped with a reason, never deleted on a guess.
//!  * `quickwit-janitor/src/retention_policy_execution.rs:49-52` — cutoff is
//!    `now - retention_period`, a single one-directional comparison.
//!
//! Elasticsearch's own ILM was consulted for *wire semantics only* (phase
//! names, `min_age` origin, the `_ilm/explain` and `_ilm/status` shapes). No
//! ES code is copied or adapted here — it is licensed AGPL/SSPL/Elastic-2.0
//! and XERJ shares no code with it.
//!
//! # What "age" means
//!
//! ES measures `min_age` from the index's creation date, overridable by
//! `index.lifecycle.origination_date`. We resolve, in order:
//!
//!  1. `index.lifecycle.origination_date` (epoch ms) from the index settings,
//!  2. `index.creation_date` (epoch ms) from the index settings,
//!  3. the creation timestamp this node recorded when the index was placed
//!     under a policy (`<data_dir>/ilm_state.json`),
//!  4. the index directory's filesystem birth time, else its mtime,
//!  5. …and if none of those exist, the index is **skipped**, loudly.
//!
//! Every fallback errs late (an under-estimated age delays deletion), never
//! early. Deleting data one tick late is a cost; deleting it one day early is
//! a catastrophe.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::engine::Engine;

// ─────────────────────────────────────────────────────────────────────────────
// Policy model
// ─────────────────────────────────────────────────────────────────────────────

/// The phases ES defines, in the order an index moves through them.
pub const PHASE_ORDER: [&str; 5] = ["hot", "warm", "cold", "frozen", "delete"];

/// The actions this engine actually executes.
///
/// Anything outside this list is rejected by [`validate_policy`] rather than
/// stored and ignored. Growing this list means growing [`Engine::run_ilm_once`]
/// at the same time — the two must never drift, which is what
/// `executable_actions_are_all_executed` asserts.
pub const EXECUTABLE_ACTIONS: [&str; 2] = ["delete", "readonly"];

/// One phase of a parsed policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhasePlan {
    /// Phase name, one of [`PHASE_ORDER`].
    pub name: String,
    /// Age at which the phase becomes due, in milliseconds. Absent `min_age`
    /// means 0 (due immediately), matching ES.
    pub min_age_ms: i64,
    /// Action names in this phase, sorted for determinism.
    pub actions: Vec<String>,
}

/// A policy reduced to what the executor needs: phases sorted by `min_age`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyPlan {
    pub phases: Vec<PhasePlan>,
}

impl PolicyPlan {
    /// The last phase whose `min_age` has elapsed at `age_ms`, if any.
    pub fn current_phase(&self, age_ms: i64) -> Option<&PhasePlan> {
        self.phases.iter().rfind(|p| age_ms >= p.min_age_ms)
    }

    /// The first phase that is not yet due at `age_ms`, if any.
    pub fn next_phase(&self, age_ms: i64) -> Option<&PhasePlan> {
        self.phases.iter().find(|p| age_ms < p.min_age_ms)
    }

    /// Every phase that is due at `age_ms`, in phase order.
    ///
    /// The executor applies *all* due phases rather than stepping one phase
    /// per tick: `readonly` is idempotent and `delete` is terminal, so
    /// applying a phase we "should" have applied yesterday converges to the
    /// same state without needing per-index step bookkeeping on disk.
    pub fn due_phases(&self, age_ms: i64) -> impl Iterator<Item = &PhasePlan> {
        self.phases.iter().filter(move |p| age_ms >= p.min_age_ms)
    }
}

/// Parse an ES time value (`"30d"`, `"12h"`, `"90m"`, `"0"`) into milliseconds.
///
/// Deliberately strict: an unparsable `min_age` is a rejected policy, not a
/// silently-zeroed one. A zeroed `min_age` would delete the user's data
/// *immediately* — the single worst failure this module could have.
pub fn parse_time_value(raw: &str) -> Result<i64, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("empty time value".to_string());
    }
    // ES accepts a bare "0" (and only 0) without a unit.
    if let Ok(n) = s.parse::<i64>() {
        if n == 0 {
            return Ok(0);
        }
        return Err(format!(
            "time value '{raw}' has no unit — use one of ms, s, m, h, d (e.g. '30d')"
        ));
    }
    let (digits, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let value: f64 = digits
        .parse()
        .map_err(|_| format!("time value '{raw}' is not a number followed by a unit"))?;
    if value < 0.0 {
        return Err(format!("time value '{raw}' must not be negative"));
    }
    let unit_ms: f64 = match unit.trim() {
        "nanos" => 1.0 / 1_000_000.0,
        "micros" => 1.0 / 1_000.0,
        "ms" => 1.0,
        "s" => 1_000.0,
        "m" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        other => {
            return Err(format!(
                "time value '{raw}' has unsupported unit '{other}' — use ms, s, m, h or d"
            ))
        }
    };
    Ok((value * unit_ms).round() as i64)
}

/// Strip the `{"policy": …}` envelope ES wraps a policy body in.
///
/// Accepts both the enveloped form (what ES clients send) and a bare
/// `{"phases": …}`, and always returns the bare form. Storing the bare form
/// is what makes `GET /_ilm/policy/{name}` able to answer in ES's shape
/// (`{name: {version, modified_date, policy: {phases}}}`) instead of the
/// double-wrapped `{name: {policy: {policy: {phases}}}}` it used to emit.
pub fn unwrap_policy_envelope(body: &Value) -> Value {
    match body.get("policy") {
        Some(inner) if inner.is_object() => inner.clone(),
        _ => body.clone(),
    }
}

/// Validate a (bare) policy, failing closed on anything the executor will not
/// honour.
///
/// This is the anti-"accepted and ignored" gate (issue #204's bug class). It
/// returns the *whole* list of problems in one message so a migrating user
/// fixes their policy in one round trip instead of five.
pub fn validate_policy(policy: &Value) -> Result<(), String> {
    let obj = policy
        .as_object()
        .ok_or_else(|| "policy must be a JSON object".to_string())?;
    for key in obj.keys() {
        // `_meta` is arbitrary user metadata in ES and carries no behaviour.
        if key != "phases" && key != "_meta" {
            return Err(format!(
                "unknown policy field '{key}' — expected 'phases' (and optionally '_meta')"
            ));
        }
    }
    let phases = obj
        .get("phases")
        .ok_or_else(|| "policy has no 'phases'".to_string())?
        .as_object()
        .ok_or_else(|| "'phases' must be a JSON object".to_string())?;
    if phases.is_empty() {
        return Err("policy has no phases — it would do nothing".to_string());
    }

    let mut unsupported: Vec<String> = Vec::new();
    for (phase_name, phase) in phases {
        if !PHASE_ORDER.contains(&phase_name.as_str()) {
            return Err(format!(
                "unknown phase '{phase_name}' — expected one of {}",
                PHASE_ORDER.join(", ")
            ));
        }
        let phase_obj = phase
            .as_object()
            .ok_or_else(|| format!("phase '{phase_name}' must be a JSON object"))?;
        for key in phase_obj.keys() {
            if key != "min_age" && key != "actions" {
                return Err(format!(
                    "phase '{phase_name}' has unknown field '{key}' — expected 'min_age' or 'actions'"
                ));
            }
        }
        if let Some(min_age) = phase_obj.get("min_age") {
            let text = min_age
                .as_str()
                .map(str::to_string)
                .or_else(|| min_age.as_i64().map(|n| n.to_string()))
                .ok_or_else(|| {
                    format!("phase '{phase_name}' min_age must be a time string like '30d'")
                })?;
            parse_time_value(&text)
                .map_err(|e| format!("phase '{phase_name}' has an invalid min_age: {e}"))?;
        }
        let actions = phase_obj
            .get("actions")
            .ok_or_else(|| format!("phase '{phase_name}' has no 'actions'"))?
            .as_object()
            .ok_or_else(|| format!("phase '{phase_name}' 'actions' must be a JSON object"))?;
        if actions.is_empty() {
            return Err(format!(
                "phase '{phase_name}' has no actions — it would do nothing"
            ));
        }
        for action in actions.keys() {
            if !EXECUTABLE_ACTIONS.contains(&action.as_str()) {
                unsupported.push(format!("{phase_name}.{action}"));
                continue;
            }
            if action == "delete" && phase_name != "delete" {
                return Err(format!(
                    "the 'delete' action is only valid in the 'delete' phase, found in '{phase_name}'"
                ));
            }
            if action == "readonly" && phase_name == "delete" {
                return Err("the 'readonly' action is not valid in the 'delete' phase".to_string());
            }
        }
    }

    if !unsupported.is_empty() {
        unsupported.sort();
        return Err(format!(
            "xerj executes only these ILM actions: {}. This policy also asks for: {}. \
             The policy is REJECTED rather than stored and ignored — a retention policy \
             that is accepted but never run is worse than none, because nothing tells you \
             it is not running (issue #199). Remove the unsupported actions, or perform \
             them yourself (e.g. POST /<data_stream>/_rollover, POST /<index>/_forcemerge).",
            EXECUTABLE_ACTIONS.join(", "),
            unsupported.join(", "),
        ));
    }
    Ok(())
}

/// Reduce a validated policy to a [`PolicyPlan`].
///
/// Returns `Err` on the same conditions [`validate_policy`] rejects, so a
/// policy that somehow reached the store un-validated (e.g. written by an
/// older build before this change, then reloaded) can never be *executed*
/// half-understood — it is reported as an error in `_ilm/explain` instead.
pub fn plan_policy(policy: &Value) -> Result<PolicyPlan, String> {
    validate_policy(policy)?;
    let phases = policy
        .get("phases")
        .and_then(Value::as_object)
        .ok_or_else(|| "policy has no 'phases'".to_string())?;
    let mut planned: Vec<PhasePlan> = Vec::new();
    for (name, phase) in phases {
        let min_age_ms = match phase.get("min_age") {
            Some(v) => {
                let text = v
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
                    .unwrap_or_default();
                parse_time_value(&text)?
            }
            None => 0,
        };
        let mut actions: Vec<String> = phase
            .get("actions")
            .and_then(Value::as_object)
            .map(|a| a.keys().cloned().collect())
            .unwrap_or_default();
        actions.sort();
        planned.push(PhasePlan {
            name: name.clone(),
            min_age_ms,
            actions,
        });
    }
    // Sort by min_age, then by ES phase order so equal ages still step
    // hot → warm → cold → frozen → delete.
    planned.sort_by_key(|p| {
        (
            p.min_age_ms,
            PHASE_ORDER
                .iter()
                .position(|n| *n == p.name)
                .unwrap_or(usize::MAX),
        )
    });
    Ok(PolicyPlan { phases: planned })
}

// ─────────────────────────────────────────────────────────────────────────────
// Persisted ILM state
// ─────────────────────────────────────────────────────────────────────────────

/// What this node remembers about one ILM-managed index.
///
/// Only indices ILM has been told about get an entry, so the file stays small
/// on a node with thousands of indices (and index creation does not pay an
/// O(indices) rewrite per create).
///
/// The presence of an entry is itself the signal: it means *this node decided*
/// something about the index, and [`Engine::ilm_policy_for_index`] therefore
/// answers from it without consulting the index's settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IlmIndexState {
    /// Epoch-ms creation time as observed by this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
    /// The policy attached via `index.lifecycle.name`.
    ///
    /// `None` on a *present* entry is the **detach tombstone**: an operator
    /// ran `PUT /{index}/_settings {"index.lifecycle.name": null}` and this
    /// index must not be managed, whatever its `settings.json` still says.
    /// Absence of the whole entry means "never heard of it", which is a
    /// different thing and falls back to the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
}

/// On-disk shape of `<data_dir>/ilm_state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IlmStateFile {
    #[serde(default)]
    policies: HashMap<String, Value>,
    #[serde(default)]
    indices: HashMap<String, IlmIndexState>,
}

/// Live counters for `GET /_ilm/status`. Cheap, monotonic, honest: every
/// number here is incremented by an action that actually happened.
#[derive(Debug)]
pub struct IlmStats {
    /// Background passes completed.
    pub passes: AtomicU64,
    /// Indices deleted by the delete phase.
    pub deleted: AtomicU64,
    /// Indices switched to read-only by the readonly action.
    pub read_only: AtomicU64,
    /// Indices skipped (unknown age, unparsable policy, safety rail).
    pub skipped: AtomicU64,
    /// Epoch-ms of the last completed pass (0 = never run).
    pub last_run_ms: AtomicU64,
    /// Operator kill switch: `POST /_ilm/stop` clears it.
    pub running: AtomicBool,
}

impl IlmStats {
    /// Fresh counters, running (subject to `ilm.enabled` in config).
    pub fn new() -> Self {
        Self {
            passes: AtomicU64::new(0),
            deleted: AtomicU64::new(0),
            read_only: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            last_run_ms: AtomicU64::new(0),
            running: AtomicBool::new(true),
        }
    }
}

impl Default for IlmStats {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// One pass
// ─────────────────────────────────────────────────────────────────────────────

/// What one ILM pass did. Returned by [`Engine::run_ilm_once`] so tests (and
/// the executor's log line) can assert on it instead of on side effects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IlmRunReport {
    /// Indices that carry a policy and were evaluated.
    pub evaluated: usize,
    /// Indices deleted, sorted.
    pub deleted: Vec<String>,
    /// Indices set read-only by this pass, sorted.
    pub read_only: Vec<String>,
    /// `(index, reason)` for every managed index the pass declined to act on.
    pub skipped: Vec<(String, String)>,
}

impl IlmRunReport {
    fn sort(&mut self) {
        self.deleted.sort();
        self.read_only.sort();
        self.skipped.sort();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Settings readers
// ─────────────────────────────────────────────────────────────────────────────

/// Read a dotted index setting out of a settings blob, tolerating every shape
/// the API accepts: nested (`{"index":{"lifecycle":{"name":…}}}`), half-flat
/// (`{"index":{"lifecycle.name":…}}`) and fully flat
/// (`{"index.lifecycle.name":…}`), with or without the `index.` prefix.
fn setting<'a>(settings: &'a Value, dotted: &str) -> Option<&'a Value> {
    let with_prefix = format!("index.{dotted}");
    let candidates = [dotted.to_string(), with_prefix];
    for key in &candidates {
        // Fully flat.
        if let Some(v) = settings.get(key.as_str()) {
            return Some(v);
        }
        // Nested, one segment at a time, allowing any suffix to be flat.
        let segs: Vec<&str> = key.split('.').collect();
        for split in 1..=segs.len() {
            let mut cursor = settings;
            let mut ok = true;
            for seg in &segs[..split] {
                match cursor.get(*seg) {
                    Some(next) => cursor = next,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            if split == segs.len() {
                return Some(cursor);
            }
            let rest = segs[split..].join(".");
            if let Some(v) = cursor.get(rest.as_str()) {
                return Some(v);
            }
        }
    }
    None
}

/// `index.lifecycle.name` out of a settings blob, if present and non-empty.
pub fn lifecycle_name_from_settings(settings: &Value) -> Option<String> {
    setting(settings, "lifecycle.name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// What a settings *body* asks ILM to do about the index's attachment.
///
/// Three-valued on purpose, because `Option<String>` cannot tell "the caller
/// said nothing about `index.lifecycle.name`" apart from "the caller said
/// `null`", and those two mean opposite things:
///
///  * `None` — the body does not mention `index.lifecycle.name`. Leave the
///    attachment exactly as it is. A body that sets, say,
///    `index.lifecycle.origination_date` must not silently detach the index.
///  * `Some(None)` — explicit `null` (or an empty string): **detach**, ES's
///    documented way to stop managing an index.
///  * `Some(Some(policy))` — attach to `policy`.
pub fn lifecycle_directive_from_settings(settings: &Value) -> Option<Option<String>> {
    match setting(settings, "lifecycle.name")? {
        Value::Null => Some(None),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Some(None)
            } else {
                Some(Some(t.to_string()))
            }
        }
        // Any other JSON type is not a policy name. Treat it as "said
        // nothing" rather than guessing a detach out of a malformed body.
        _ => None,
    }
}

/// An explicit epoch-ms origin from settings, accepting the string form ES
/// coerces settings into as well as a raw number.
fn epoch_ms_from_settings(settings: &Value, dotted: &str) -> Option<i64> {
    let v = setting(settings, dotted)?;
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        .filter(|ms| *ms > 0)
}

/// Format an age the way ES's `_ilm/explain` does (`"3.2d"`, `"45m"`).
pub fn format_age_ms(ms: i64) -> String {
    let ms_f = ms as f64;
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms_f / 1_000.0)
    } else if ms < 3_600_000 {
        format!("{:.1}m", ms_f / 60_000.0)
    } else if ms < 86_400_000 {
        format!("{:.1}h", ms_f / 3_600_000.0)
    } else {
        format!("{:.1}d", ms_f / 86_400_000.0)
    }
}

/// Wall clock in epoch milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Engine integration
// ─────────────────────────────────────────────────────────────────────────────

/// Why an index's age could not be established, or where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeOrigin {
    /// `index.lifecycle.origination_date`.
    OriginationDate,
    /// `index.creation_date`.
    CreationDate,
    /// Recorded by this node when the index was placed under a policy.
    Recorded,
    /// The index directory's filesystem birth time.
    DirBirth,
    /// The index directory's mtime (birth time unsupported by the filesystem).
    DirMtime,
}

impl AgeOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            AgeOrigin::OriginationDate => "index.lifecycle.origination_date",
            AgeOrigin::CreationDate => "index.creation_date",
            AgeOrigin::Recorded => "recorded_at_attach",
            AgeOrigin::DirBirth => "index_dir_birth_time",
            AgeOrigin::DirMtime => "index_dir_mtime",
        }
    }
}

impl Engine {
    /// Path of the persisted ILM store (`<data_dir>/ilm_state.json`).
    fn ilm_state_path(&self) -> PathBuf {
        self.data_dir_path().join("ilm_state.json")
    }

    /// Snapshot policies + per-index ILM state to `<data_dir>/ilm_state.json`
    /// atomically, mirroring `flush_aliases`.
    ///
    /// Policies are persisted here because retention that forgets its own
    /// policy on restart is retention that silently stops — the exact failure
    /// issue #199 is about. (Issue #203 is adding general persistence for the
    /// ES-compat metadata maps; when it lands, this half can be dropped in
    /// favour of it. Loading is idempotent, so both may coexist.)
    pub(crate) fn flush_ilm_state(&self) {
        let file = IlmStateFile {
            policies: self
                .ilm_policies
                .iter()
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect(),
            indices: self
                .ilm_index_state
                .iter()
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect(),
        };
        let bytes = match serde_json::to_vec_pretty(&file) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to serialize ILM state for persistence");
                return;
            }
        };
        if let Err(e) = crate::index::write_file_atomic(&self.ilm_state_path(), &bytes) {
            warn!(error = %e, "failed to persist ilm_state.json (ILM works until restart)");
        }
    }

    /// Load `<data_dir>/ilm_state.json` on boot. Missing file is normal; a
    /// corrupt one is logged and ignored (the node still boots, and ILM then
    /// simply manages nothing rather than guessing).
    pub(crate) fn load_persisted_ilm_state(&self) {
        let Ok(bytes) = std::fs::read(self.ilm_state_path()) else {
            return;
        };
        match serde_json::from_slice::<IlmStateFile>(&bytes) {
            Ok(file) => {
                let (np, ni) = (file.policies.len(), file.indices.len());
                for (name, policy) in file.policies {
                    self.ilm_policies.insert(name, policy);
                }
                for (name, state) in file.indices {
                    self.ilm_index_state.insert(name, state);
                }
                if np > 0 || ni > 0 {
                    info!(
                        policies = np,
                        managed_indices = ni,
                        "restored persisted ILM state"
                    );
                }
            }
            Err(e) => warn!(error = %e, "ignoring corrupt ilm_state.json"),
        }
    }

    /// Store a validated, envelope-stripped policy and persist it.
    pub fn put_ilm_policy(&self, name: &str, policy: Value) {
        self.ilm_policies.insert(name.to_string(), policy);
        self.flush_ilm_state();
    }

    /// Remove a policy. Returns whether it existed.
    pub fn remove_ilm_policy(&self, name: &str) -> bool {
        let existed = self.ilm_policies.remove(name).is_some();
        if existed {
            self.flush_ilm_state();
        }
        existed
    }

    /// Place `index` under `policy` (or detach it when `policy` is `None`),
    /// recording the creation time the executor will age it from.
    ///
    /// Called from every path that can carry `index.lifecycle.name`: index
    /// creation and `PUT /{index}/_settings`. Attachment is persisted here
    /// rather than relying on the index-settings map, which is in-memory only
    /// — otherwise a restart would silently un-manage the index.
    ///
    /// # A detach writes a tombstone, it does not erase the entry
    ///
    /// `PUT /{index}/_settings {"index.lifecycle.name": null}` is ES's
    /// documented way to stop managing an index, and it is the only way this
    /// engine offers (`POST /{index}/_ilm/remove` is not implemented). The
    /// first cut of this method implemented it as
    /// `self.ilm_index_state.remove(index)` — and that was a **data-loss
    /// defect of exactly the class this module exists to remove**. The index's
    /// persisted `settings.json` still carried the create-time
    /// `index.lifecycle.name`, [`Engine::ilm_policy_for_index`] fell back to
    /// it, and the detach the operator was told had been acknowledged was
    /// ignored: the next pass deleted the index anyway. It was also a silent
    /// no-op whenever no in-memory entry existed at all.
    ///
    /// So a detach now *records* itself: `IlmIndexState { policy: None }` is a
    /// persisted tombstone meaning "this node was explicitly told to stop
    /// managing this index", and the resolver honours it ahead of any settings
    /// fallback. One source of truth, and the one the operator can write.
    pub fn set_index_lifecycle_policy(&self, index: &str, policy: Option<&str>) {
        // Preserved across a detach/re-attach so the re-attached index is aged
        // from its real creation time rather than from the moment of re-attach.
        let recorded_created = self.ilm_index_state.get(index).and_then(|s| s.created_ms);
        let state = match policy {
            Some(p) => IlmIndexState {
                created_ms: Some(
                    recorded_created
                        .or_else(|| self.index_dir_creation_ms(index))
                        .unwrap_or_else(now_ms),
                ),
                policy: Some(p.to_string()),
            },
            None => IlmIndexState {
                created_ms: recorded_created,
                policy: None,
            },
        };
        let unchanged = self
            .ilm_index_state
            .get(index)
            .is_some_and(|prev| *prev.value() == state);
        if unchanged {
            return;
        }
        match policy {
            Some(p) => info!(index, policy = p, "index placed under ILM policy"),
            None => info!(index, "index detached from ILM"),
        }
        self.ilm_index_state.insert(index.to_string(), state);
        self.flush_ilm_state();
    }

    /// Drop any ILM bookkeeping for a deleted index.
    pub(crate) fn forget_ilm_index(&self, index: &str) {
        if self.ilm_index_state.remove(index).is_some() {
            self.flush_ilm_state();
        }
    }

    /// Filesystem birth time (else mtime) of the index directory, epoch ms.
    fn index_dir_creation_ms(&self, index: &str) -> Option<i64> {
        self.index_dir_creation(index).map(|(ms, _)| ms)
    }

    fn index_dir_creation(&self, index: &str) -> Option<(i64, AgeOrigin)> {
        let meta = std::fs::metadata(self.data_dir_path().join(index)).ok()?;
        let to_ms = |t: std::time::SystemTime| -> Option<i64> {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as i64)
        };
        if let Some(ms) = meta.created().ok().and_then(to_ms) {
            return Some((ms, AgeOrigin::DirBirth));
        }
        meta.modified()
            .ok()
            .and_then(to_ms)
            .map(|ms| (ms, AgeOrigin::DirMtime))
    }

    /// The settings blob to read ILM settings from: the API's round-trip copy
    /// merged with whatever the index persisted at create time.
    async fn ilm_settings_for(&self, index: &str) -> Value {
        if let Some(s) = self.index_settings.get(index) {
            if lifecycle_name_from_settings(s.value()).is_some()
                || setting(s.value(), "lifecycle.origination_date").is_some()
                || setting(s.value(), "creation_date").is_some()
            {
                return s.value().clone();
            }
        }
        match self.get_index(index) {
            Ok(idx) => idx.settings_snapshot().await,
            Err(_) => Value::Null,
        }
    }

    /// Which policy manages `index`, if any.
    ///
    /// **The recorded state is authoritative and total.** If this node has an
    /// [`IlmIndexState`] entry for the index, that entry is the answer — a
    /// `policy: Some(p)` attachment *or* a `policy: None` detach tombstone
    /// (see [`Engine::set_index_lifecycle_policy`]). The index's own settings
    /// are consulted only when there is no entry at all, which is the
    /// upgrade case: an index created before `ilm_state.json` existed, whose
    /// `settings.json` is the only record that it was ever attached.
    ///
    /// Reading the settings *after* the recorded state instead of only in its
    /// absence is what made an acknowledged detach a lie: the create-time
    /// `index.lifecycle.name` in `settings.json` outlived the entry the
    /// operator could clear, so the executor kept deleting a detached index.
    ///
    /// Every "is this index managed?" question must come through here —
    /// `run_ilm_once`, `_ilm/explain`, and the `DELETE /_ilm/policy` in-use
    /// check all do, so they cannot disagree.
    ///
    /// **Index templates are deliberately not consulted here.** A template that
    /// carries `index.lifecycle.name` is read at *creation* time
    /// (`Engine::create_index_with_settings`, via
    /// [`Engine::template_lifecycle_name`]) and the attachment is recorded on
    /// the index then, which is exactly when ES applies template settings.
    /// Re-reading the template at evaluation time instead would make a newly
    /// added `logs-*` template retroactively manage — and, past its `min_age`,
    /// *delete* — every `logs-*` index that already existed before the template
    /// was written. ES never does that, and a retention feature whose first act
    /// is to destroy pre-existing data is not one anybody should ship.
    pub async fn ilm_policy_for_index(&self, index: &str) -> Option<String> {
        if let Some(state) = self.ilm_index_state.get(index) {
            // `None` here is the explicit detach tombstone, not "unknown".
            return state.policy.clone();
        }
        let settings = self.ilm_settings_for(index).await;
        lifecycle_name_from_settings(&settings)
    }

    /// Every *existing* index that [`Engine::ilm_policy_for_index`] resolves to
    /// `policy`, sorted.
    ///
    /// `DELETE /_ilm/policy/{name}` refuses while this is non-empty. It has to
    /// ask the same resolver the executor asks, or the two disagree in both
    /// directions: scanning `ilm_index_state` alone let the DELETE succeed for
    /// a policy that an upgraded index still points at through its settings,
    /// and counted detach tombstones as live users.
    ///
    /// Restricted to indices that currently exist, so stale bookkeeping can
    /// never make a policy permanently undeletable by naming a phantom index.
    pub async fn ilm_indices_using_policy(&self, policy: &str) -> Vec<String> {
        let mut out = Vec::new();
        for name in self.list_index_names() {
            if self.ilm_policy_for_index(&name).await.as_deref() == Some(policy) {
                out.push(name);
            }
        }
        out.sort();
        out
    }

    /// Epoch-ms the index's age is measured from, and where that came from.
    pub async fn ilm_age_origin_ms(&self, index: &str) -> Option<(i64, AgeOrigin)> {
        let settings = self.ilm_settings_for(index).await;
        if let Some(ms) = epoch_ms_from_settings(&settings, "lifecycle.origination_date") {
            return Some((ms, AgeOrigin::OriginationDate));
        }
        if let Some(ms) = epoch_ms_from_settings(&settings, "creation_date") {
            return Some((ms, AgeOrigin::CreationDate));
        }
        if let Some(ms) = self
            .ilm_index_state
            .get(index)
            .and_then(|s| s.created_ms)
            .filter(|ms| *ms > 0)
        {
            return Some((ms, AgeOrigin::Recorded));
        }
        self.index_dir_creation(index)
    }

    /// If `index` is the *current write index* of a data stream, name that
    /// stream. ILM must never delete the index new documents are landing in —
    /// ES's delete phase does not either.
    fn data_stream_write_index_owner(&self, index: &str) -> Option<String> {
        self.data_streams
            .iter()
            .find_map(|e| match e.value().backing_indices.last() {
                Some(write_index) if write_index == index => Some(e.key().clone()),
                _ => None,
            })
    }

    /// Drop a deleted backing index from its data stream's generation list so
    /// the stream does not keep advertising an index that no longer exists.
    fn detach_data_stream_backing_index(&self, index: &str) {
        for mut entry in self.data_streams.iter_mut() {
            entry.backing_indices.retain(|b| b != index);
        }
    }

    /// Is the executor allowed to act right now?
    pub fn ilm_running(&self) -> bool {
        self.config().ilm.enabled && self.ilm_stats.running.load(Ordering::Relaxed)
    }

    /// `POST /_ilm/start` / `POST /_ilm/stop`.
    ///
    /// Refuses to report RUNNING when the build-time switch is off, so
    /// `GET /_ilm/status` can never claim to be running while nothing runs.
    pub fn set_ilm_running(&self, running: bool) {
        self.ilm_stats.running.store(running, Ordering::Relaxed);
    }

    /// Safety rail: names this executor will never delete.
    ///
    /// Dot-prefixed names are XERJ/Kibana internals (`.xerj-memory-*` brains,
    /// `.kibana*`, security stores). A wildcard index template must not be
    /// able to attach a 7-day retention policy to a user's second brain by
    /// accident. Data-stream backing indices (`.ds-*`) are the one dotted
    /// family ILM is *for*, so they are exempt from the rail.
    fn ilm_delete_is_forbidden(name: &str) -> bool {
        name.starts_with('.') && !name.starts_with(".ds-")
    }

    /// Why the delete phase must not fire on `index`, if it must not.
    ///
    /// One function so the executor's refusal and `_ilm/explain`'s
    /// `blocked_reason` can never disagree about what will happen.
    fn ilm_delete_block_reason(&self, index: &str) -> Option<String> {
        if Self::ilm_delete_is_forbidden(index) {
            return Some("internal index — ILM will not delete a dot-prefixed index".to_string());
        }
        self.data_stream_write_index_owner(index)
            .map(|stream| format!("current write index of data stream '{stream}'"))
    }

    /// Evaluate every ILM-managed index at wall-clock `now_ms` and apply the
    /// phases that are due.
    ///
    /// `now_ms` is a parameter rather than a read of the clock so a test can
    /// age an index through a phase transition deterministically instead of
    /// sleeping. The background executor passes the real clock.
    ///
    /// Never returns an error: a failure on one index is recorded and the
    /// pass continues (quickwit's janitor makes the same call —
    /// `retention_policy_executor.rs:78-80`).
    pub async fn run_ilm_once(&self, now_ms: i64) -> IlmRunReport {
        let mut report = IlmRunReport::default();
        if !self.ilm_running() {
            return report;
        }
        let names: Vec<String> = self.list_index_names();
        for name in names {
            let Some(policy_name) = self.ilm_policy_for_index(&name).await else {
                continue;
            };
            report.evaluated += 1;
            let Some(raw) = self.ilm_policies.get(&policy_name).map(|p| p.clone()) else {
                report
                    .skipped
                    .push((name.clone(), format!("policy '{policy_name}' not found")));
                continue;
            };
            let plan = match plan_policy(&raw) {
                Ok(p) => p,
                Err(e) => {
                    report.skipped.push((
                        name.clone(),
                        format!("policy '{policy_name}' is not executable: {e}"),
                    ));
                    continue;
                }
            };
            let Some((origin_ms, origin)) = self.ilm_age_origin_ms(&name).await else {
                // quickwit skips splits it cannot date rather than deleting
                // them (retention_policy_execution.rs:65-76). Same here.
                report.skipped.push((
                    name.clone(),
                    "age unknown (no creation date and no directory timestamp)".to_string(),
                ));
                continue;
            };
            let age_ms = now_ms.saturating_sub(origin_ms);
            if age_ms < 0 {
                report
                    .skipped
                    .push((name.clone(), "creation date is in the future".to_string()));
                continue;
            }

            let mut delete_due = false;
            let mut readonly_due = false;
            for phase in plan.due_phases(age_ms) {
                for action in &phase.actions {
                    match action.as_str() {
                        "delete" => delete_due = true,
                        "readonly" => readonly_due = true,
                        // Unreachable: `plan_policy` runs `validate_policy`,
                        // which rejects everything outside EXECUTABLE_ACTIONS.
                        other => report
                            .skipped
                            .push((name.clone(), format!("action '{other}' is not executed"))),
                    }
                }
            }

            // A delete that is due but blocked by a safety rail must not also
            // swallow the readonly action: the index is staying, so the phase
            // it is genuinely in still applies.
            let delete_block = if delete_due {
                self.ilm_delete_block_reason(&name)
            } else {
                None
            };

            if readonly_due && (!delete_due || delete_block.is_some()) {
                match self.get_index(&name) {
                    Ok(idx) => {
                        if !idx.is_write_blocked().await {
                            match idx.set_block("write").await {
                                Ok(()) => {
                                    info!(
                                        index = name.as_str(),
                                        policy = policy_name.as_str(),
                                        age = format_age_ms(age_ms).as_str(),
                                        "ILM set index read-only"
                                    );
                                    report.read_only.push(name.clone());
                                }
                                Err(e) => report
                                    .skipped
                                    .push((name.clone(), format!("readonly failed: {e}"))),
                            }
                        }
                    }
                    Err(e) => report
                        .skipped
                        .push((name.clone(), format!("readonly failed: {e}"))),
                }
            }

            if delete_due {
                if let Some(reason) = delete_block {
                    report.skipped.push((name.clone(), reason));
                    continue;
                }
                // Loud on purpose: this line is the receipt that a retention
                // policy destroyed data, and the only place an operator can
                // reconstruct why afterwards.
                info!(
                    index = name.as_str(),
                    policy = policy_name.as_str(),
                    age = format_age_ms(age_ms).as_str(),
                    age_from = origin.as_str(),
                    "ILM delete phase is deleting this index"
                );
                match self.delete_index(&name).await {
                    Ok(()) => {
                        self.detach_data_stream_backing_index(&name);
                        report.deleted.push(name.clone());
                    }
                    Err(e) => report
                        .skipped
                        .push((name.clone(), format!("delete failed: {e}"))),
                }
            }
        }

        report.sort();
        self.ilm_stats.passes.fetch_add(1, Ordering::Relaxed);
        self.ilm_stats
            .deleted
            .fetch_add(report.deleted.len() as u64, Ordering::Relaxed);
        self.ilm_stats
            .read_only
            .fetch_add(report.read_only.len() as u64, Ordering::Relaxed);
        self.ilm_stats
            .skipped
            .fetch_add(report.skipped.len() as u64, Ordering::Relaxed);
        self.ilm_stats
            .last_run_ms
            .store(now_ms.max(0) as u64, Ordering::Relaxed);
        if !report.skipped.is_empty() {
            for (index, reason) in &report.skipped {
                warn!(
                    index = index.as_str(),
                    reason = reason.as_str(),
                    "ILM skipped an index"
                );
            }
        }
        report
    }

    /// `GET /{index}/_ilm/explain` — what ILM knows and what it will do next.
    ///
    /// Observability is half the fix: a retention policy you cannot inspect is
    /// a retention policy you cannot trust. The `xerj` block says plainly
    /// whether the policy is executable, when the next action fires, and why
    /// an index is being skipped.
    pub async fn ilm_explain(&self, index: &str, now_ms: i64) -> Value {
        let Some(policy_name) = self.ilm_policy_for_index(index).await else {
            return json!({ "index": index, "managed": false });
        };
        let mut out = json!({
            "index": index,
            "managed": true,
            "policy": policy_name,
        });
        let obj = out.as_object_mut().expect("object literal");

        let raw = self.ilm_policies.get(&policy_name).map(|p| p.clone());
        let Some(raw) = raw else {
            obj.insert(
                "xerj".to_string(),
                json!({
                    "executable": false,
                    "reason": format!("policy '{policy_name}' is not defined on this node"),
                }),
            );
            return out;
        };
        let plan = match plan_policy(&raw) {
            Ok(p) => p,
            Err(e) => {
                obj.insert(
                    "xerj".to_string(),
                    json!({ "executable": false, "reason": e }),
                );
                return out;
            }
        };
        let Some((origin_ms, origin)) = self.ilm_age_origin_ms(index).await else {
            obj.insert(
                "xerj".to_string(),
                json!({
                    "executable": false,
                    "reason": "age unknown (no creation date and no directory timestamp) — \
                               this index will never be aged out",
                }),
            );
            return out;
        };
        let age_ms = now_ms.saturating_sub(origin_ms).max(0);
        obj.insert("index_creation_date_millis".to_string(), json!(origin_ms));
        obj.insert("lifecycle_date_millis".to_string(), json!(origin_ms));
        obj.insert("age".to_string(), json!(format_age_ms(age_ms)));
        let current = plan.current_phase(age_ms);
        obj.insert(
            "phase".to_string(),
            json!(current
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "new".into())),
        );
        obj.insert(
            "action".to_string(),
            json!(current
                .and_then(|p| p.actions.first().cloned())
                .unwrap_or_else(|| "complete".into())),
        );
        let next = plan.next_phase(age_ms);
        let mut xerj = json!({
            "executable": true,
            "running": self.ilm_running(),
            "age_millis": age_ms,
            "age_measured_from": origin.as_str(),
        });
        let xerj_obj = xerj.as_object_mut().expect("object literal");
        if let Some(next) = next {
            xerj_obj.insert("next_phase".to_string(), json!(next.name));
            xerj_obj.insert("next_phase_actions".to_string(), json!(next.actions));
            xerj_obj.insert(
                "next_phase_due_at_millis".to_string(),
                json!(origin_ms.saturating_add(next.min_age_ms)),
            );
            xerj_obj.insert(
                "next_phase_in".to_string(),
                json!(format_age_ms(next.min_age_ms.saturating_sub(age_ms))),
            );
        } else {
            xerj_obj.insert("next_phase".to_string(), Value::Null);
        }
        if plan
            .due_phases(age_ms)
            .any(|p| p.actions.iter().any(|a| a == "delete"))
        {
            if let Some(reason) = self.ilm_delete_block_reason(index) {
                xerj_obj.insert("blocked_reason".to_string(), json!(reason));
            }
        }
        obj.insert("xerj".to_string(), xerj);
        out
    }

    /// `GET /_ilm/status`, with XERJ's honest extras.
    pub fn ilm_status(&self) -> Value {
        let s = &self.ilm_stats;
        json!({
            "operation_mode": if self.ilm_running() { "RUNNING" } else { "STOPPED" },
            "xerj": {
                "enabled_in_config": self.config().ilm.enabled,
                "poll_interval_secs": self.config().ilm.poll_interval_secs,
                "executable_actions": EXECUTABLE_ACTIONS,
                "passes": s.passes.load(Ordering::Relaxed),
                "indices_deleted": s.deleted.load(Ordering::Relaxed),
                "indices_set_read_only": s.read_only.load(Ordering::Relaxed),
                "indices_skipped": s.skipped.load(Ordering::Relaxed),
                "last_run_millis": s.last_run_ms.load(Ordering::Relaxed),
                // Attachments only — a detach tombstone (`policy: None`) is a
                // record that we are *not* managing the index, and counting it
                // here would report retention on an index nothing retains.
                "managed_indices": self
                    .ilm_index_state
                    .iter()
                    .filter(|e| e.value().policy.is_some())
                    .count(),
                "policies": self.ilm_policies.len(),
            }
        })
    }

    /// Start the background executor.
    ///
    /// Takes `Arc<Self>` and holds only a `Weak`, exactly like
    /// `spawn_resource_sampler`: the engine owns the data-dir `node.lock`, so
    /// a long-lived task holding a strong clone would keep that lock alive
    /// after the last user-visible engine was dropped and wedge the next open
    /// of the same directory.
    ///
    /// # Index-guard contract
    ///
    /// This spawn reaches [`Engine::get_index`] and [`Engine::delete_index`],
    /// so [`crate::index_guard`]'s rule applies: carry the request guard with
    /// `index_guard::current`/`scoped`, or say at the spawn site why it
    /// cannot. It cannot, and deliberately: there is no request and no
    /// principal behind a retention tick — it is started by `Engine::new`'s
    /// caller at boot and runs on the node's own authority, like
    /// `spawn_pit_sweeper`. Running it under any caller's guard would be
    /// *wrong*, not merely unnecessary: retention would then apply only to
    /// whichever principal happened to boot the node. The safety boundary here
    /// is the delete rail in [`Engine::ilm_delete_block_reason`] (no
    /// dot-prefixed index, no data-stream write index), not visibility. The
    /// audit table in `index_guard`'s module docs carries the matching row.
    pub fn spawn_ilm_executor(self: &Arc<Self>) {
        if !self.config().ilm.enabled {
            info!("ILM executor disabled by config (ilm.enabled = false)");
            return;
        }
        let interval =
            std::time::Duration::from_secs(self.config().ilm.poll_interval_secs.clamp(1, 86_400));
        let weak: Weak<Engine> = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            // Skip the immediate first tick: at boot the index set is still
            // settling and there is nothing an extra pass buys.
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(engine) = weak.upgrade() else {
                    return;
                };
                let report = engine.run_ilm_once(now_ms()).await;
                if !report.deleted.is_empty() || !report.read_only.is_empty() {
                    info!(
                        evaluated = report.evaluated,
                        deleted = report.deleted.len(),
                        read_only = report.read_only.len(),
                        "ILM pass applied retention"
                    );
                }
            }
        });
        info!(
            poll_interval_secs = self.config().ilm.poll_interval_secs,
            executable_actions = EXECUTABLE_ACTIONS.join(","),
            "ILM executor started"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_values_parse_like_es() {
        assert_eq!(parse_time_value("0").unwrap(), 0);
        assert_eq!(parse_time_value("500ms").unwrap(), 500);
        assert_eq!(parse_time_value("30s").unwrap(), 30_000);
        assert_eq!(parse_time_value("5m").unwrap(), 300_000);
        assert_eq!(parse_time_value("2h").unwrap(), 7_200_000);
        assert_eq!(parse_time_value("30d").unwrap(), 30 * 86_400_000);
    }

    #[test]
    fn unitless_and_unknown_units_are_rejected_not_zeroed() {
        // A silently-zeroed min_age would delete the user's data immediately.
        assert!(parse_time_value("30").is_err());
        assert!(parse_time_value("30 weeks").is_err());
        assert!(parse_time_value("").is_err());
        assert!(parse_time_value("-1d").is_err());
    }

    #[test]
    fn delete_policy_is_accepted_and_planned() {
        let policy = json!({
            "phases": {
                "delete": { "min_age": "30d", "actions": { "delete": {} } }
            }
        });
        validate_policy(&policy).expect("delete policies are executable");
        let plan = plan_policy(&policy).unwrap();
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].min_age_ms, 30 * 86_400_000);
        assert!(plan.current_phase(29 * 86_400_000).is_none());
        assert_eq!(plan.current_phase(31 * 86_400_000).unwrap().name, "delete");
    }

    #[test]
    fn unsupported_actions_are_rejected_with_their_names() {
        let policy = json!({
            "phases": {
                "hot": { "actions": { "rollover": { "max_age": "1d" } } },
                "warm": { "min_age": "7d", "actions": { "forcemerge": { "max_num_segments": 1 } } },
                "delete": { "min_age": "30d", "actions": { "delete": {} } }
            }
        });
        let err = validate_policy(&policy).expect_err("rollover/forcemerge are not executed");
        assert!(err.contains("hot.rollover"), "{err}");
        assert!(err.contains("warm.forcemerge"), "{err}");
    }

    #[test]
    fn misplaced_delete_action_is_rejected() {
        let policy = json!({
            "phases": { "warm": { "min_age": "7d", "actions": { "delete": {} } } }
        });
        assert!(validate_policy(&policy).is_err());
    }

    #[test]
    fn envelope_is_stripped_once() {
        let enveloped =
            json!({ "policy": { "phases": { "delete": { "actions": { "delete": {} } } } } });
        let bare = unwrap_policy_envelope(&enveloped);
        assert!(bare.get("phases").is_some());
        // Idempotent: unwrapping a bare policy leaves it alone.
        assert_eq!(unwrap_policy_envelope(&bare), bare);
    }

    #[test]
    fn phases_are_planned_in_age_order() {
        let policy = json!({
            "phases": {
                "delete": { "min_age": "30d", "actions": { "delete": {} } },
                "warm":   { "min_age": "7d",  "actions": { "readonly": {} } }
            }
        });
        let plan = plan_policy(&policy).unwrap();
        assert_eq!(
            plan.phases
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["warm", "delete"]
        );
        let due: Vec<&str> = plan
            .due_phases(31 * 86_400_000)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(due, vec!["warm", "delete"]);
        assert_eq!(plan.next_phase(8 * 86_400_000).unwrap().name, "delete");
    }

    #[test]
    fn lifecycle_name_is_read_from_every_settings_shape() {
        let nested = json!({ "index": { "lifecycle": { "name": "logs-30d" } } });
        let half_flat = json!({ "index": { "lifecycle.name": "logs-30d" } });
        let flat = json!({ "index.lifecycle.name": "logs-30d" });
        let bare = json!({ "lifecycle": { "name": "logs-30d" } });
        for s in [nested, half_flat, flat, bare] {
            assert_eq!(
                lifecycle_name_from_settings(&s).as_deref(),
                Some("logs-30d"),
                "{s}"
            );
        }
        assert_eq!(lifecycle_name_from_settings(&json!({})), None);
        // An empty name is not an attachment.
        assert_eq!(
            lifecycle_name_from_settings(&json!({ "index.lifecycle.name": "  " })),
            None
        );
    }

    #[test]
    fn every_executable_action_has_an_executor_arm() {
        // Guard against the drift this whole module exists to prevent: an
        // action allowed by validation but not implemented would be accepted
        // and ignored — the exact bug of issue #199.
        for action in EXECUTABLE_ACTIONS {
            assert!(
                matches!(action, "delete" | "readonly"),
                "action '{action}' is allowed by validation but run_ilm_once has no arm for it"
            );
        }
    }
}
