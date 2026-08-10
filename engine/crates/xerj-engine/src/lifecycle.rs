//! Index lifecycle management — one execution engine, two REST surfaces.
//!
//! Modeled on OpenSearch's Index State Management (ISM), not Elasticsearch's
//! fixed hot/warm/cold/delete phases: ISM's state machine (named states,
//! each with its own ordered actions and ordered transitions with
//! conditions) is strictly more general, so it's the internal model.
//! Elasticsearch's `_ilm/*` surface is a translator into this same model
//! (see [`translate_ilm_policy`]), not a second engine — an ILM policy with
//! hot/warm/cold/delete phases becomes an ISM policy whose states are named
//! `"hot"`/`"warm"`/`"cold"`/`"delete"` in that fixed sequence.
//!
//! ## Known simplification vs. real ILM
//!
//! Real Elasticsearch measures a phase's `min_age` from the index's
//! rollover time (not its creation time) once the policy's hot phase has a
//! `rollover` action — the age clock effectively restarts at rollover. This
//! engine measures every state's age from when the index *entered that
//! state* (`state_entered_at_ms`), which is index-attach time for the
//! initial state, not rollover time. For a policy without `rollover`, or
//! for reasoning about relative phase durations, this is equivalent; for a
//! policy that rolls over AND has a downstream phase whose `min_age` is
//! meant to be measured strictly from rollover, this engine will advance
//! phases slightly earlier than real ILM would. Documented rather than
//! silently diverging — see PR discussion for whether this needs closing.
//!
//! ## What's NOT supported, and errors honestly instead of faking success
//!
//! `replica_count`: XERJ has no per-index configurable replica concept
//! (single-shard, single-node engine; `number_of_replicas` is accepted in
//! index settings and echoed back for wire compatibility but never
//! consumed). The action fails with an explicit error rather than silently
//! no-op'ing, so a policy relying on it is visibly stuck, not silently
//! wrong.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::engine::Engine;
use crate::{EngineError, Result};

// ─────────────────────────────────────────────────────────────────────────────
// Data model — faithful to OpenSearch ISM's policy JSON shape.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecyclePolicy {
    #[serde(default)]
    pub description: Option<String>,
    pub default_state: String,
    pub states: Vec<LifecycleState>,
}

impl LifecyclePolicy {
    /// Structural validation beyond what serde already enforces: every
    /// transition target and `default_state` must name a real state, and
    /// state names must be unique. Real ISM rejects a policy with a
    /// dangling transition at PUT time rather than discovering it mid-run.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.states.is_empty() {
            return Err("policy must declare at least one state".to_string());
        }
        let names: std::collections::HashSet<&str> =
            self.states.iter().map(|s| s.name.as_str()).collect();
        if names.len() != self.states.len() {
            return Err("state names must be unique".to_string());
        }
        if !names.contains(self.default_state.as_str()) {
            return Err(format!(
                "default_state '{}' does not name a declared state",
                self.default_state
            ));
        }
        for state in &self.states {
            for t in &state.transitions {
                if !names.contains(t.state_name.as_str()) {
                    return Err(format!(
                        "state '{}' transitions to undeclared state '{}'",
                        state.name, t.state_name
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn state(&self, name: &str) -> Option<&LifecycleState> {
        self.states.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleState {
    pub name: String,
    #[serde(default)]
    pub actions: Vec<LifecycleAction>,
    #[serde(default)]
    pub transitions: Vec<LifecycleTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleTransition {
    pub state_name: String,
    #[serde(default)]
    pub conditions: Option<LifecycleConditions>,
}

/// Conditions shared by transitions (when to leave a state) and the
/// `rollover` action (when it's actually worth rolling over) — same shape,
/// same evaluator, two different call sites. A transition/action with no
/// conditions at all is always eligible the moment it's checked (an
/// unconditional transition; an unconditional rollover).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleConditions {
    /// ES/ISM duration string: digits followed by `ms|s|m|h|d`. E.g. `"7d"`.
    #[serde(default)]
    pub min_index_age: Option<String>,
    /// ES/ISM size string: digits followed by `b|kb|mb|gb|tb`. E.g. `"50gb"`.
    #[serde(default)]
    pub min_size: Option<String>,
    #[serde(default)]
    pub min_doc_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmptyParams {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RolloverAction {
    #[serde(flatten)]
    pub conditions: LifecycleConditions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaCountAction {
    pub number_of_replicas: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction {
    Rollover(RolloverAction),
    Delete(EmptyParams),
    ReadOnly(EmptyParams),
    ReplicaCount(ReplicaCountAction),
}

impl LifecycleAction {
    pub fn name(&self) -> &'static str {
        match self {
            LifecycleAction::Rollover(_) => "rollover",
            LifecycleAction::Delete(_) => "delete",
            LifecycleAction::ReadOnly(_) => "read_only",
            LifecycleAction::ReplicaCount(_) => "replica_count",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Managed-index state — the per-index execution cursor.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedIndexState {
    pub policy_id: String,
    pub current_state: String,
    pub state_entered_at_ms: i64,
    /// Index into the current state's `actions[]` — which one runs next.
    /// Equal to `actions.len()` once every action in this state has
    /// completed and only transition-evaluation remains.
    #[serde(default)]
    pub next_action_index: usize,
    pub last_updated_ms: i64,
    /// Mirrors ISM explain's free-form `info.message`.
    #[serde(default)]
    pub info_message: String,
    /// Set when a step errors, so `explain` surfaces it instead of the
    /// index silently retrying the same failing action forever unseen.
    /// Retries still happen (a transient failure — e.g. disk contention —
    /// should self-heal); this is visibility, not a halt.
    #[serde(default)]
    pub failed: bool,
}

impl ManagedIndexState {
    pub fn new(policy_id: String, default_state: String, now_ms: i64) -> Self {
        Self {
            policy_id,
            current_state: default_state,
            state_entered_at_ms: now_ms,
            next_action_index: 0,
            last_updated_ms: now_ms,
            info_message: "attached, awaiting first execution".to_string(),
            failed: false,
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Duration / size string parsing — ES/ISM shorthand.
// ─────────────────────────────────────────────────────────────────────────────

/// Parse an ES/ISM duration string (`"7d"`, `"30m"`, `"500ms"`) into
/// milliseconds. Unlike ES's full duration grammar this accepts exactly one
/// integer + one unit, which is all `min_index_age` ever uses in practice.
pub fn parse_duration_ms(s: &str) -> std::result::Result<i64, String> {
    let s = s.trim();
    let split_at = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("duration '{s}' has no unit"))?;
    let (digits, unit) = s.split_at(split_at);
    let n: i64 = digits
        .parse()
        .map_err(|_| format!("duration '{s}' has no numeric prefix"))?;
    let ms = match unit {
        "ms" => n,
        "s" => n * 1_000,
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        other => return Err(format!("unknown duration unit '{other}' in '{s}'")),
    };
    Ok(ms)
}

/// Parse an ES/ISM size string (`"50gb"`, `"100mb"`) into bytes. Binary
/// (1024-based) units, matching ES's own `ByteSizeValue`.
pub fn parse_size_bytes(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim().to_ascii_lowercase();
    let split_at = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("size '{s}' has no unit"))?;
    let (digits, unit) = s.split_at(split_at);
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("size '{s}' has no numeric prefix"))?;
    let bytes = match unit {
        "b" => n,
        "kb" => n * 1024,
        "mb" => n * 1024 * 1024,
        "gb" => n * 1024 * 1024 * 1024,
        "tb" => n * 1024 * 1024 * 1024 * 1024,
        other => return Err(format!("unknown size unit '{other}' in '{s}'")),
    };
    Ok(bytes)
}

// ─────────────────────────────────────────────────────────────────────────────
// ES ILM → internal model translator.
// ─────────────────────────────────────────────────────────────────────────────

/// Fixed ILM phase order — ISM states are named exactly this, one per phase
/// actually present in the policy. A phase absent from the ILM policy
/// produces no state at all (matching ILM, where an unconfigured phase is
/// simply skipped).
const ILM_PHASE_ORDER: [&str; 4] = ["hot", "warm", "cold", "delete"];

/// Translate an ES ILM policy body (`{"policy": {"phases": {...}}}`, the
/// same shape `PUT _ilm/policy/{name}` accepts) into a [`LifecyclePolicy`].
///
/// Each present phase becomes a state named after the phase. A phase's own
/// `actions` (minus `min_age`, which isn't an action) become that state's
/// actions. A phase's `min_age` becomes the CONDITION on the transition
/// INTO that phase from the previous one — i.e. "warm: min_age 7d" means
/// the state before warm gets a transition to warm gated on
/// `min_index_age: 7d`. The final present phase gets no transitions
/// (terminal), matching a real ISM policy's last state.
pub fn translate_ilm_policy(ilm_body: &Value) -> std::result::Result<LifecyclePolicy, String> {
    let phases = ilm_body
        .pointer("/policy/phases")
        .and_then(Value::as_object)
        .ok_or_else(|| "ILM policy body missing policy.phases".to_string())?;

    let present: Vec<&str> = ILM_PHASE_ORDER
        .iter()
        .copied()
        .filter(|p| phases.contains_key(*p))
        .collect();
    if present.is_empty() {
        return Err("ILM policy declares no known phase (hot/warm/cold/delete)".to_string());
    }

    let mut states = Vec::with_capacity(present.len());
    for (i, phase_name) in present.iter().enumerate() {
        let phase = &phases[*phase_name];
        let actions = translate_ilm_phase_actions(phase_name, phase)?;

        let transitions = if let Some(&next_phase) = present.get(i + 1) {
            let next_min_age = phases[next_phase]
                .get("min_age")
                .and_then(Value::as_str)
                .unwrap_or("0ms");
            vec![LifecycleTransition {
                state_name: next_phase.to_string(),
                conditions: Some(LifecycleConditions {
                    min_index_age: Some(next_min_age.to_string()),
                    min_size: None,
                    min_doc_count: None,
                }),
            }]
        } else {
            Vec::new()
        };

        states.push(LifecycleState {
            name: phase_name.to_string(),
            actions,
            transitions,
        });
    }

    let policy = LifecyclePolicy {
        description: ilm_body
            .pointer("/policy/_meta/description")
            .and_then(Value::as_str)
            .map(String::from),
        default_state: present[0].to_string(),
        states,
    };
    policy.validate()?;
    Ok(policy)
}

fn translate_ilm_phase_actions(
    phase_name: &str,
    phase: &Value,
) -> std::result::Result<Vec<LifecycleAction>, String> {
    let Some(actions_obj) = phase.get("actions").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (action_name, params) in actions_obj {
        let action = match action_name.as_str() {
            "rollover" => {
                let conditions = LifecycleConditions {
                    min_index_age: params
                        .get("max_age")
                        .and_then(Value::as_str)
                        .map(String::from),
                    min_size: params
                        .get("max_size")
                        .and_then(Value::as_str)
                        .map(String::from),
                    min_doc_count: params.get("max_docs").and_then(Value::as_u64),
                };
                LifecycleAction::Rollover(RolloverAction { conditions })
            }
            "delete" => LifecycleAction::Delete(EmptyParams {}),
            "readonly" => LifecycleAction::ReadOnly(EmptyParams {}),
            "set_priority" | "allocate" | "unfollow" | "shrink" | "forcemerge" | "freeze" => {
                // Real ILM actions with no XERJ equivalent (no shard
                // allocation awareness, no shrink/forcemerge-as-a-lifecycle-
                // step wiring, no CCR). Skipped rather than erroring the
                // whole policy translation — matches this engine's existing
                // "explicit error only where behavior would be silently
                // wrong" policy: skipping a no-op-shaped action is honest
                // (nothing was promised and not delivered), unlike
                // replica_count which IS user-visible if silently ignored.
                continue;
            }
            other => {
                return Err(format!(
                    "phase '{phase_name}' action '{other}' has no ISM/XERJ equivalent"
                ))
            }
        };
        out.push(action);
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Condition evaluation.
// ─────────────────────────────────────────────────────────────────────────────

struct IndexFacts {
    age_ms: i64,
    size_bytes: u64,
    doc_count: u64,
}

async fn gather_index_facts(
    engine: &Engine,
    index_name: &str,
    state_age_ms: i64,
) -> Option<IndexFacts> {
    let idx = engine.get_index(index_name).ok()?;
    let stats = idx.stats().await;
    let segment_bytes: u64 = idx
        .store_snapshot()
        .segments
        .iter()
        .map(|s| s.size_bytes)
        .sum();
    let size_bytes = segment_bytes + stats.memtable_size_bytes as u64;
    Some(IndexFacts {
        age_ms: state_age_ms,
        size_bytes,
        doc_count: stats.doc_count,
    })
}

/// A `LifecycleConditions` with every field `None` matches immediately —
/// this is what makes an unconditional transition / unconditional rollover
/// always-eligible the first time it's checked.
fn conditions_met(
    conditions: &LifecycleConditions,
    facts: &IndexFacts,
) -> std::result::Result<bool, String> {
    if let Some(age_str) = &conditions.min_index_age {
        let threshold_ms = parse_duration_ms(age_str)?;
        if facts.age_ms < threshold_ms {
            return Ok(false);
        }
    }
    if let Some(size_str) = &conditions.min_size {
        let threshold_bytes = parse_size_bytes(size_str)?;
        if facts.size_bytes < threshold_bytes {
            return Ok(false);
        }
    }
    if let Some(min_docs) = conditions.min_doc_count {
        if facts.doc_count < min_docs {
            return Ok(false);
        }
    }
    Ok(true)
}

// ─────────────────────────────────────────────────────────────────────────────
// Action execution.
// ─────────────────────────────────────────────────────────────────────────────

pub enum ActionOutcome {
    /// The action completed; advance past it.
    Done,
    /// The action's own conditions aren't met yet (rollover only); leave
    /// `next_action_index` where it is and retry next tick.
    Waiting,
    /// The managed index itself is gone (delete action succeeded) — the
    /// caller must drop this index from `managed_indices` entirely rather
    /// than continue processing it.
    IndexDeleted,
}

async fn execute_action(
    engine: &Engine,
    index_name: &str,
    action: &LifecycleAction,
    state_age_ms: i64,
) -> Result<ActionOutcome> {
    match action {
        LifecycleAction::Rollover(rollover) => {
            let has_conditions = rollover.conditions.min_index_age.is_some()
                || rollover.conditions.min_size.is_some()
                || rollover.conditions.min_doc_count.is_some();
            if has_conditions {
                let Some(facts) = gather_index_facts(engine, index_name, state_age_ms).await else {
                    return Err(EngineError::Common(
                        xerj_common::XerjError::index_not_found(index_name),
                    ));
                };
                let met = conditions_met(&rollover.conditions, &facts)
                    .map_err(|e| EngineError::Common(xerj_common::XerjError::invalid_query(e)))?;
                if !met {
                    return Ok(ActionOutcome::Waiting);
                }
            }
            // Reuses the existing, already-real rollover implementation —
            // requires `index_name` to be a registered data stream (alias +
            // generation-numbered backing indices). A plain index that
            // isn't a data stream fails here with a clear not-found error
            // rather than silently doing nothing.
            engine.rollover_data_stream(index_name)?;
            Ok(ActionOutcome::Done)
        }
        LifecycleAction::Delete(_) => {
            engine.delete_index(index_name).await?;
            engine.managed_indices.remove(index_name);
            Ok(ActionOutcome::IndexDeleted)
        }
        LifecycleAction::ReadOnly(_) => {
            let idx = engine.get_index(index_name)?;
            idx.set_block("write").await?;
            Ok(ActionOutcome::Done)
        }
        LifecycleAction::ReplicaCount(_) => Err(EngineError::Common(
            xerj_common::XerjError::internal(format!(
                "the 'replica_count' lifecycle action is not supported: XERJ is a \
                 single-shard, single-node engine with no per-index configurable \
                 replica count to change (number_of_replicas is accepted in index \
                 settings for wire compatibility but never consumed) — policy for \
                 index '{index_name}' cannot proceed past this action"
            )),
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tick — the background job's one unit of work.
// ─────────────────────────────────────────────────────────────────────────────

/// Run one lifecycle pass over every managed index: execute the current
/// state's next pending action (if any), or — once all of a state's
/// actions have completed — evaluate its transitions in order and move to
/// the first one whose conditions are met.
pub async fn tick(engine: &Engine) {
    let snapshot: Vec<(String, ManagedIndexState)> = engine
        .managed_indices
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    let mut any_change = false;
    for (index_name, mut managed) in snapshot {
        let Some(policy) = engine
            .ism_policies
            .get(&managed.policy_id)
            .map(|e| e.value().clone())
        else {
            if !managed.failed {
                managed.failed = true;
                managed.info_message = format!("policy '{}' no longer exists", managed.policy_id);
                managed.last_updated_ms = now_ms();
                engine.managed_indices.insert(index_name, managed);
                any_change = true;
            }
            continue;
        };
        let Some(state_def) = policy.state(&managed.current_state).cloned() else {
            managed.failed = true;
            managed.info_message = format!(
                "state '{}' no longer exists in policy '{}'",
                managed.current_state, managed.policy_id
            );
            managed.last_updated_ms = now_ms();
            engine.managed_indices.insert(index_name, managed);
            any_change = true;
            continue;
        };

        let state_age_ms = now_ms() - managed.state_entered_at_ms;

        if managed.next_action_index < state_def.actions.len() {
            let action = &state_def.actions[managed.next_action_index];
            match execute_action(engine, &index_name, action, state_age_ms).await {
                Ok(ActionOutcome::Done) => {
                    managed.next_action_index += 1;
                    managed.failed = false;
                    managed.info_message = format!("completed action '{}'", action.name());
                    managed.last_updated_ms = now_ms();
                    engine.managed_indices.insert(index_name, managed);
                    any_change = true;
                }
                Ok(ActionOutcome::Waiting) => {
                    managed.info_message =
                        format!("waiting on '{}' action conditions", action.name());
                    managed.last_updated_ms = now_ms();
                    engine.managed_indices.insert(index_name, managed);
                    any_change = true;
                }
                Ok(ActionOutcome::IndexDeleted) => {
                    // Already removed from managed_indices by execute_action.
                    any_change = true;
                }
                Err(e) => {
                    managed.failed = true;
                    managed.info_message = format!("action '{}' failed: {e}", action.name());
                    managed.last_updated_ms = now_ms();
                    engine.managed_indices.insert(index_name, managed);
                    any_change = true;
                }
            }
            continue;
        }

        // All actions in this state are done — evaluate transitions in
        // order; first met condition wins. No transitions = terminal state.
        let Some(facts) = gather_index_facts(engine, &index_name, state_age_ms).await else {
            managed.failed = true;
            managed.info_message = "index no longer exists".to_string();
            managed.last_updated_ms = now_ms();
            engine.managed_indices.insert(index_name, managed);
            any_change = true;
            continue;
        };
        let mut transitioned = false;
        for t in &state_def.transitions {
            let met = match &t.conditions {
                None => true,
                Some(c) => conditions_met(c, &facts).unwrap_or(false),
            };
            if met {
                managed.current_state = t.state_name.clone();
                managed.state_entered_at_ms = now_ms();
                managed.next_action_index = 0;
                managed.failed = false;
                managed.info_message = format!("transitioned to state '{}'", t.state_name);
                managed.last_updated_ms = now_ms();
                engine.managed_indices.insert(index_name, managed);
                transitioned = true;
                any_change = true;
                break;
            }
        }
        if !transitioned {
            // Nothing to do this tick; leave last_updated_ms untouched so
            // explain doesn't churn on every quiet tick.
        }
    }

    if any_change {
        engine.persist_managed_indices();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Explain — the read model both REST surfaces render from.
// ─────────────────────────────────────────────────────────────────────────────

/// Build the `explain` JSON for one managed index, matching the fields real
/// ISM's `GET _plugins/_ism/explain/{index}` reports (a practical subset —
/// `policy_seq_no`/`policy_primary_term` are omitted since this engine has
/// no policy versioning).
pub fn explain_json(index_name: &str, managed: &ManagedIndexState) -> Value {
    json!({
        "index": index_name,
        "index.plugins.index_state_management.policy_id": managed.policy_id,
        "policy_id": managed.policy_id,
        "state": {
            "name": managed.current_state,
            "start_time": managed.state_entered_at_ms,
        },
        "action": {
            "name": managed.info_message,
            "failed": managed.failed,
        },
        "info": {
            "message": managed.info_message,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_handles_all_units() {
        assert_eq!(parse_duration_ms("500ms").unwrap(), 500);
        assert_eq!(parse_duration_ms("7s").unwrap(), 7_000);
        assert_eq!(parse_duration_ms("5m").unwrap(), 300_000);
        assert_eq!(parse_duration_ms("2h").unwrap(), 7_200_000);
        assert_eq!(parse_duration_ms("30d").unwrap(), 30 * 86_400_000);
        assert!(parse_duration_ms("30x").is_err());
        assert!(parse_duration_ms("abc").is_err());
    }

    #[test]
    fn parse_size_handles_all_units() {
        assert_eq!(parse_size_bytes("100b").unwrap(), 100);
        assert_eq!(parse_size_bytes("1kb").unwrap(), 1024);
        assert_eq!(parse_size_bytes("50mb").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size_bytes("2gb").unwrap(), 2 * 1024 * 1024 * 1024);
        assert!(parse_size_bytes("1zz").is_err());
    }

    #[test]
    fn conditions_met_empty_conditions_always_true() {
        let facts = IndexFacts {
            age_ms: 0,
            size_bytes: 0,
            doc_count: 0,
        };
        assert!(conditions_met(&LifecycleConditions::default(), &facts).unwrap());
    }

    #[test]
    fn conditions_met_requires_every_declared_condition() {
        let facts = IndexFacts {
            age_ms: 10_000,
            size_bytes: 100,
            doc_count: 5,
        };
        let conditions = LifecycleConditions {
            min_index_age: Some("1s".to_string()),
            min_size: None,
            min_doc_count: Some(10),
        };
        // age condition met (10s >= 1s) but doc_count condition not met (5 < 10)
        assert!(!conditions_met(&conditions, &facts).unwrap());

        let conditions2 = LifecycleConditions {
            min_index_age: Some("1s".to_string()),
            min_size: None,
            min_doc_count: Some(5),
        };
        assert!(conditions_met(&conditions2, &facts).unwrap());
    }

    #[test]
    fn policy_validate_rejects_dangling_transition() {
        let policy = LifecyclePolicy {
            description: None,
            default_state: "hot".to_string(),
            states: vec![LifecycleState {
                name: "hot".to_string(),
                actions: vec![],
                transitions: vec![LifecycleTransition {
                    state_name: "warm".to_string(),
                    conditions: None,
                }],
            }],
        };
        assert!(policy.validate().is_err());
    }

    #[test]
    fn policy_validate_rejects_unknown_default_state() {
        let policy = LifecyclePolicy {
            description: None,
            default_state: "nope".to_string(),
            states: vec![LifecycleState {
                name: "hot".to_string(),
                actions: vec![],
                transitions: vec![],
            }],
        };
        assert!(policy.validate().is_err());
    }

    #[test]
    fn policy_validate_accepts_well_formed_policy() {
        let policy = LifecyclePolicy {
            description: Some("test".to_string()),
            default_state: "hot".to_string(),
            states: vec![
                LifecycleState {
                    name: "hot".to_string(),
                    actions: vec![LifecycleAction::Rollover(RolloverAction::default())],
                    transitions: vec![LifecycleTransition {
                        state_name: "delete".to_string(),
                        conditions: Some(LifecycleConditions {
                            min_index_age: Some("30d".to_string()),
                            min_size: None,
                            min_doc_count: None,
                        }),
                    }],
                },
                LifecycleState {
                    name: "delete".to_string(),
                    actions: vec![LifecycleAction::Delete(EmptyParams {})],
                    transitions: vec![],
                },
            ],
        };
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn action_json_round_trips_expected_shape() {
        let action = LifecycleAction::Rollover(RolloverAction {
            conditions: LifecycleConditions {
                min_doc_count: Some(100),
                ..Default::default()
            },
        });
        let v = serde_json::to_value(&action).unwrap();
        assert_eq!(v["rollover"]["min_doc_count"], 100);

        let delete = LifecycleAction::Delete(EmptyParams {});
        let v = serde_json::to_value(&delete).unwrap();
        assert_eq!(v, json!({"delete": {}}));

        let ro = LifecycleAction::ReadOnly(EmptyParams {});
        let v = serde_json::to_value(&ro).unwrap();
        assert_eq!(v, json!({"read_only": {}}));
    }

    #[test]
    fn translate_ilm_maps_phases_to_states_in_order() {
        let ilm = json!({
            "policy": {
                "phases": {
                    "hot": {
                        "min_age": "0ms",
                        "actions": { "rollover": { "max_size": "50gb", "max_docs": 100 } }
                    },
                    "delete": {
                        "min_age": "30d",
                        "actions": { "delete": {} }
                    }
                }
            }
        });
        let policy = translate_ilm_policy(&ilm).unwrap();
        assert_eq!(policy.default_state, "hot");
        assert_eq!(policy.states.len(), 2);
        assert_eq!(policy.states[0].name, "hot");
        assert_eq!(policy.states[0].transitions.len(), 1);
        assert_eq!(policy.states[0].transitions[0].state_name, "delete");
        assert_eq!(
            policy.states[0].transitions[0]
                .conditions
                .as_ref()
                .unwrap()
                .min_index_age,
            Some("30d".to_string())
        );
        assert_eq!(policy.states[1].name, "delete");
        assert!(policy.states[1].transitions.is_empty());
        match &policy.states[0].actions[0] {
            LifecycleAction::Rollover(r) => {
                assert_eq!(r.conditions.min_size, Some("50gb".to_string()));
                assert_eq!(r.conditions.min_doc_count, Some(100));
            }
            other => panic!("expected rollover action, got {other:?}"),
        }
    }

    #[test]
    fn translate_ilm_skips_unmapped_actions_errors_on_unknown() {
        let ilm = json!({
            "policy": {
                "phases": {
                    "hot": {
                        "actions": { "set_priority": { "priority": 100 } }
                    }
                }
            }
        });
        let policy = translate_ilm_policy(&ilm).unwrap();
        assert!(policy.states[0].actions.is_empty());

        let ilm_bad = json!({
            "policy": { "phases": { "hot": { "actions": { "totally_made_up": {} } } } }
        });
        assert!(translate_ilm_policy(&ilm_bad).is_err());
    }
}
