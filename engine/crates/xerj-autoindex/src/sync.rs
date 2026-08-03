//! Pure, inert corpus-generation planning and durable replay.
//!
//! Nothing in this module performs HTTP I/O or changes autoindex behaviour.
//! It defines the complete state an eventual synchronization executor must
//! durably bind before publication.

use crate::state::{DuplicateFile, Plan};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub const EXECUTION_IDENTITY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManifestPath {
    /// Reversible platform-native path identity (`unix:`/`windows:` encoding).
    pub path_id: String,
    pub rel: String,
    /// Canonical election ranks real paths before followed symlinks.
    pub is_symlink: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestGroup {
    /// Stable logical identity across rename/replacement.
    pub group_id: String,
    /// Collision-safe document identity assigned only after byte verification.
    pub content_id: String,
    /// Raw full-content digest. Never treated as globally unique by itself.
    pub content_digest: String,
    pub content_size: u64,
    pub canonical: ManifestPath,
    #[serde(default)]
    pub aliases: Vec<ManifestPath>,
    #[serde(default)]
    pub dataset_slugs: Vec<String>,
    pub expected_records: u64,
    pub expected_passages: u64,
    pub expected_vectors: u64,
}

impl ManifestGroup {
    fn all_paths(&self) -> impl Iterator<Item = &ManifestPath> {
        std::iter::once(&self.canonical).chain(self.aliases.iter())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum SourceExecutionPolicy {
    DurableSnapshot {
        reference: String,
        snapshot_digest: String,
    },
    AbortOnSourceChange {
        inventory_digest: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub version: u32,
    pub root_identity: String,
    pub url: String,
    pub prefix: String,
    pub follow_symlinks: bool,
    pub chunker_identity: String,
    pub embedding_backend: String,
    pub embedding_model: String,
    pub embedding_tokenizer: String,
    pub embedding_dimension: usize,
    pub graph_enabled: bool,
    pub brain: String,
    pub detector_identity: String,
    pub schema_identity: String,
    pub index_identity: String,
    pub source_policy: SourceExecutionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationManifest {
    pub generation: u64,
    /// Missing only for a legacy generation-0 plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionIdentity>,
    pub plan: Plan,
    pub groups: Vec<ManifestGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommittedManifest {
    pub generation: u64,
    pub manifest_digest: String,
    pub plan: Plan,
    pub groups: Vec<ManifestGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionIdentity>,
}

#[derive(Debug, Clone)]
pub enum LegacyBootstrap {
    Ready(Box<CommittedManifest>),
    MigrationRequired { reasons: Vec<String> },
}

impl CommittedManifest {
    pub fn bootstrap_legacy(plan: Plan) -> Result<LegacyBootstrap> {
        let mut reasons = Vec::new();
        for (file_key, file) in &plan.files {
            if file.content_digest.is_none() {
                reasons.push(format!("{file_key}: missing full content digest"));
            }
            if !native_path_id_supported(&file.path_id) {
                reasons.push(format!(
                    "{file_key}: missing or lossy native canonical path identity"
                ));
            }
            if file.is_symlink.is_none() {
                reasons.push(format!(
                    "{file_key}: legacy plan did not persist canonical symlink rank"
                ));
            }
            // A Plan is not itself a generation manifest: it has no stable
            // group ID, content byte length, or validated output counts.
            reasons.push(format!(
                "{file_key}: legacy plan has no complete manifest group"
            ));
        }
        for alias in &plan.duplicate_files {
            if !native_path_id_supported(&alias.path_id) {
                reasons.push(format!(
                    "{}: missing or lossy native alias path identity",
                    alias.rel
                ));
            }
            if alias.is_symlink.is_none() {
                reasons.push(format!(
                    "{}: legacy plan did not persist alias symlink rank",
                    alias.rel
                ));
            }
        }
        reasons.sort();
        reasons.dedup();
        if !reasons.is_empty() {
            return Ok(LegacyBootstrap::MigrationRequired { reasons });
        }
        let manifest = GenerationManifest {
            generation: 0,
            execution: None,
            plan: plan.clone(),
            groups: Vec::new(),
        };
        Ok(LegacyBootstrap::Ready(Box::new(Self {
            generation: 0,
            manifest_digest: manifest_digest(&manifest)?,
            plan,
            groups: Vec::new(),
            execution: None,
        })))
    }

    fn from_generation(manifest: GenerationManifest, digest: String) -> Self {
        Self {
            generation: manifest.generation,
            manifest_digest: digest,
            plan: manifest.plan,
            groups: manifest.groups,
            execution: manifest.execution,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperationKind {
    Upsert,
    Delete,
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct SyncOperation {
    pub operation_id: String,
    pub kind: SyncOperationKind,
    pub group_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_content_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperationState {
    Started,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSync {
    pub tx_id: String,
    pub base_generation: u64,
    pub base_manifest_digest: String,
    pub desired_manifest_digest: String,
    pub operation_hash: String,
    pub desired: GenerationManifest,
    pub operations: Vec<SyncOperation>,
    #[serde(default)]
    pub operation_states: BTreeMap<String, SyncOperationState>,
    #[serde(default)]
    pub validated: bool,
}

impl PendingSync {
    pub fn new(
        tx_id: String,
        base: &CommittedManifest,
        mut desired: GenerationManifest,
    ) -> Result<Self> {
        canonicalize_manifest_order(&mut desired);
        validate_manifest(&desired, false)?;
        validate_supported_manifest_delta(base, &desired)?;
        let operations = plan_operations(&base.groups, &desired.groups);
        let pending = Self {
            tx_id,
            base_generation: base.generation,
            base_manifest_digest: base.manifest_digest.clone(),
            desired_manifest_digest: manifest_digest(&desired)?,
            operation_hash: operation_hash(&operations)?,
            desired,
            operations,
            operation_states: BTreeMap::new(),
            validated: false,
        };
        pending.validate_against(base)?;
        Ok(pending)
    }

    pub fn validate_against(&self, base: &CommittedManifest) -> Result<()> {
        anyhow::ensure!(
            self.base_generation == base.generation
                && self.base_manifest_digest == base.manifest_digest,
            "sync_begin base does not match committed authority"
        );
        anyhow::ensure!(
            self.desired.generation == self.base_generation + 1,
            "desired generation must immediately follow its base"
        );
        validate_manifest(&self.desired, false)?;
        validate_supported_manifest_delta(base, &self.desired)?;
        anyhow::ensure!(
            self.desired_manifest_digest == manifest_digest(&self.desired)?,
            "desired manifest digest does not match sync_begin payload"
        );
        let expected = plan_operations(&base.groups, &self.desired.groups);
        anyhow::ensure!(
            self.operations == expected,
            "sync operation list was not derived exactly from base and desired manifests"
        );
        anyhow::ensure!(
            self.operation_hash == operation_hash(&expected)?,
            "sync operation hash does not match derived operation list"
        );
        let operation_ids: HashSet<&str> = expected
            .iter()
            .map(|operation| operation.operation_id.as_str())
            .collect();
        anyhow::ensure!(
            self.operation_states
                .keys()
                .all(|operation_id| operation_ids.contains(operation_id.as_str())),
            "sync state references an operation absent from sync_begin"
        );
        Ok(())
    }

    pub fn apply_operation_state(
        &mut self,
        tx_id: &str,
        operation_id: &str,
        state: SyncOperationState,
    ) -> Result<()> {
        anyhow::ensure!(
            self.tx_id == tx_id,
            "operation state belongs to another sync"
        );
        anyhow::ensure!(
            self.operations
                .iter()
                .any(|operation| operation.operation_id == operation_id),
            "operation state references unknown operation {operation_id}"
        );
        let legal = match self.operation_states.get(operation_id) {
            None => state == SyncOperationState::Started,
            Some(previous) => {
                previous == &state
                    || previous == &SyncOperationState::Started
                        && state == SyncOperationState::Committed
            }
        };
        anyhow::ensure!(
            legal,
            "illegal operation transition {:?} -> {state:?}",
            self.operation_states.get(operation_id)
        );
        self.operation_states
            .insert(operation_id.to_string(), state);
        Ok(())
    }

    pub fn all_operations_committed(&self) -> bool {
        self.operations.iter().all(|operation| {
            self.operation_states.get(&operation.operation_id)
                == Some(&SyncOperationState::Committed)
        })
    }

    pub fn apply_validated(&mut self, tx_id: &str, digest: &str) -> Result<()> {
        anyhow::ensure!(self.tx_id == tx_id, "validation belongs to another sync");
        anyhow::ensure!(
            self.desired_manifest_digest == digest,
            "validation manifest digest does not match sync_begin"
        );
        anyhow::ensure!(
            self.all_operations_committed(),
            "cannot validate sync with unfinished operations"
        );
        self.validated = true;
        Ok(())
    }

    pub fn apply_commit(self, tx_id: &str, digest: &str) -> Result<CommittedManifest> {
        anyhow::ensure!(self.tx_id == tx_id, "commit belongs to another sync");
        anyhow::ensure!(
            self.desired_manifest_digest == digest,
            "commit manifest digest does not match sync_begin"
        );
        anyhow::ensure!(self.validated, "cannot commit an unvalidated sync");
        anyhow::ensure!(
            self.all_operations_committed(),
            "cannot commit sync with unfinished operations"
        );
        Ok(CommittedManifest::from_generation(
            self.desired,
            self.desired_manifest_digest,
        ))
    }
}

fn validate_supported_manifest_delta(
    base: &CommittedManifest,
    desired: &GenerationManifest,
) -> Result<()> {
    // Binding complete identity while migrating the legacy generation zero is
    // the only configuration transition this vocabulary can represent. A
    // durable snapshot reference and digest are generation inputs, not engine
    // configuration, so they must advance with each desired generation.
    if base.generation > 0 || base.execution.is_some() {
        let same_execution_configuration = |left: &ExecutionIdentity, right: &ExecutionIdentity| {
            let mut left = left.clone();
            let mut right = right.clone();
            if let (
                SourceExecutionPolicy::DurableSnapshot { .. },
                SourceExecutionPolicy::DurableSnapshot { .. },
            ) = (&left.source_policy, &right.source_policy)
            {
                left.source_policy = SourceExecutionPolicy::DurableSnapshot {
                    reference: String::new(),
                    snapshot_digest: String::new(),
                };
                right.source_policy = left.source_policy.clone();
            }
            left == right
        };
        anyhow::ensure!(
            match (&base.execution, &desired.execution) {
                (Some(left), Some(right)) => same_execution_configuration(left, right),
                (None, None) => true,
                _ => false,
            },
            "execution identity or source policy changed without a supported operation"
        );
    }
    anyhow::ensure!(
        base.plan.alias_paths_indexed == desired.plan.alias_paths_indexed,
        "plan flags changed without a supported operation"
    );
    anyhow::ensure!(
        canonical_json_bytes(&base.plan.junk_files)?
            == canonical_json_bytes(&desired.plan.junk_files)?,
        "junk plan changed without a supported operation"
    );
    let normalize_datasets = |datasets: &[crate::state::PlanDataset]| {
        let mut datasets = datasets.to_vec();
        for dataset in &mut datasets {
            // Membership operations legitimately change this projection.
            dataset.file_count = 0;
        }
        datasets.sort_by(|left, right| {
            left.slug
                .cmp(&right.slug)
                .then_with(|| left.index.cmp(&right.index))
                .then_with(|| left.group.cmp(&right.group))
        });
        for dataset in &mut datasets {
            dataset
                .specs
                .sort_by(|left, right| left.name.cmp(&right.name));
        }
        datasets
    };
    anyhow::ensure!(
        canonical_json_bytes(&normalize_datasets(&base.plan.datasets))?
            == canonical_json_bytes(&normalize_datasets(&desired.plan.datasets))?,
        "dataset schema/index plan changed without a schema-generation operation"
    );
    Ok(())
}

pub fn plan_operations(base: &[ManifestGroup], desired: &[ManifestGroup]) -> Vec<SyncOperation> {
    let base: BTreeMap<&str, &ManifestGroup> = base
        .iter()
        .map(|group| (group.group_id.as_str(), group))
        .collect();
    let desired: BTreeMap<&str, &ManifestGroup> = desired
        .iter()
        .map(|group| (group.group_id.as_str(), group))
        .collect();
    let mut operations = Vec::new();
    for (group_id, old) in &base {
        match desired.get(group_id) {
            None => operations.push(operation(SyncOperationKind::Delete, group_id, None)),
            Some(new)
                if old.content_id != new.content_id
                    || old.content_digest != new.content_digest
                    || old.content_size != new.content_size =>
            {
                operations.push(operation(
                    SyncOperationKind::Upsert,
                    group_id,
                    Some(new.content_id.clone()),
                ));
            }
            Some(new) if *old != *new => operations.push(operation(
                SyncOperationKind::Metadata,
                group_id,
                Some(new.content_id.clone()),
            )),
            Some(_) => {}
        }
    }
    for (group_id, new) in desired {
        if !base.contains_key(group_id) {
            operations.push(operation(
                SyncOperationKind::Upsert,
                group_id,
                Some(new.content_id.clone()),
            ));
        }
    }
    operations.sort();
    operations
}

fn operation(
    kind: SyncOperationKind,
    group_id: &str,
    desired_content_id: Option<String>,
) -> SyncOperation {
    SyncOperation {
        operation_id: format!("{kind:?}:{group_id}").to_ascii_lowercase(),
        kind,
        group_id: group_id.to_string(),
        desired_content_id,
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct DesiredContentGroup {
    pub content_id: String,
    pub content_digest: String,
    pub content_size: u64,
    pub paths: Vec<ManifestPath>,
    pub dataset_slugs: Vec<String>,
    pub expected_records: u64,
    pub expected_passages: u64,
    pub expected_vectors: u64,
}

/// Content identity is collision-safe and byte-verified by the inventory
/// layer. Raw digest equality alone never merges groups.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reconcile_groups(
    previous: &[ManifestGroup],
    mut desired: Vec<DesiredContentGroup>,
) -> Result<Vec<ManifestGroup>> {
    validate_groups(previous)?;
    desired.sort_by(|left, right| {
        left.content_id
            .cmp(&right.content_id)
            .then_with(|| canonical_path(&left.paths).cmp(&canonical_path(&right.paths)))
    });
    let old_by_content: HashMap<&str, &ManifestGroup> = previous
        .iter()
        .map(|group| (group.content_id.as_str(), group))
        .collect();
    anyhow::ensure!(
        old_by_content.len() == previous.len(),
        "collision-safe content IDs must be unique"
    );
    let surviving_content: HashSet<String> = desired
        .iter()
        .map(|group| group.content_id.clone())
        .collect();
    let mut old_by_path = HashMap::new();
    for group in previous {
        for path in group.all_paths() {
            anyhow::ensure!(
                old_by_path.insert(path.path_id.as_str(), group).is_none(),
                "native path identity belongs to multiple committed groups"
            );
        }
    }
    let mut claimed = HashSet::new();
    let mut seen_paths = HashSet::new();
    let mut result = Vec::new();
    for mut candidate in desired {
        normalize_paths(&mut candidate.paths, &mut seen_paths)?;
        let canonical = candidate.paths.remove(0);
        let aliases = candidate.paths;
        let content_owner = old_by_content.get(candidate.content_id.as_str()).copied();
        let mut path_owners: BTreeSet<&str> = std::iter::once(&canonical)
            .chain(aliases.iter())
            .filter_map(|path| old_by_path.get(path.path_id.as_str()).copied())
            .filter(|owner| !surviving_content.contains(owner.content_id.as_str()))
            .map(|owner| owner.group_id.as_str())
            .collect();
        let group_id = if let Some(owner) = content_owner {
            anyhow::ensure!(
                path_owners.is_empty() || path_owners.contains(owner.group_id.as_str()),
                "desired group has conflicting content and path identities"
            );
            owner.group_id.clone()
        } else if path_owners.len() == 1 {
            path_owners.pop_first().unwrap().to_string()
        } else if path_owners.is_empty() {
            format!(
                "axg1-{:032x}",
                xxhash_rust::xxh3::xxh3_128(
                    format!("{}\0{}", candidate.content_id, canonical.path_id).as_bytes()
                )
            )
        } else {
            anyhow::bail!("desired content merges multiple historical groups");
        };
        anyhow::ensure!(claimed.insert(group_id.clone()), "group claimed twice");
        candidate.dataset_slugs.sort();
        candidate.dataset_slugs.dedup();
        result.push(ManifestGroup {
            group_id,
            content_id: candidate.content_id,
            content_digest: candidate.content_digest,
            content_size: candidate.content_size,
            canonical,
            aliases,
            dataset_slugs: candidate.dataset_slugs,
            expected_records: candidate.expected_records,
            expected_passages: candidate.expected_passages,
            expected_vectors: candidate.expected_vectors,
        });
    }
    result.sort_by(|left, right| left.group_id.cmp(&right.group_id));
    Ok(result)
}

#[cfg_attr(not(test), allow(dead_code))]
fn canonical_path(paths: &[ManifestPath]) -> (bool, &str) {
    paths
        .iter()
        .map(|path| (path.is_symlink, path.path_id.as_str()))
        .min()
        .unwrap_or((true, ""))
}

#[cfg_attr(not(test), allow(dead_code))]
fn normalize_paths(paths: &mut [ManifestPath], seen: &mut HashSet<String>) -> Result<()> {
    anyhow::ensure!(!paths.is_empty(), "desired content group has no paths");
    for path in paths.iter() {
        anyhow::ensure!(
            native_path_id_supported(&path.path_id),
            "path {} has no supported reversible native identity",
            path.rel
        );
        anyhow::ensure!(
            seen.insert(path.path_id.clone()),
            "native path identity occurs more than once"
        );
    }
    paths.sort_by(|left, right| {
        (left.is_symlink, left.path_id.as_str()).cmp(&(right.is_symlink, right.path_id.as_str()))
    });
    Ok(())
}

fn native_path_id_supported(path_id: &str) -> bool {
    let valid_hex = |hex: &str, width: usize| {
        !hex.is_empty()
            && hex.len().is_multiple_of(width)
            && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    path_id
        .strip_prefix("unix:")
        .is_some_and(|hex| valid_hex(hex, 2))
        || path_id
            .strip_prefix("windows:")
            .is_some_and(|hex| valid_hex(hex, 4))
}

fn validate_manifest(manifest: &GenerationManifest, legacy: bool) -> Result<()> {
    if manifest.generation == 0 && legacy {
        anyhow::ensure!(
            manifest.execution.is_none(),
            "legacy generation zero cannot invent execution identity"
        );
    } else {
        let execution = manifest
            .execution
            .as_ref()
            .context("new generation requires complete execution identity")?;
        anyhow::ensure!(
            execution.version == EXECUTION_IDENTITY_VERSION,
            "unsupported execution identity version"
        );
        for (name, value) in [
            ("root_identity", execution.root_identity.as_str()),
            ("url", execution.url.as_str()),
            ("prefix", execution.prefix.as_str()),
            ("chunker_identity", execution.chunker_identity.as_str()),
            ("embedding_backend", execution.embedding_backend.as_str()),
            ("embedding_model", execution.embedding_model.as_str()),
            (
                "embedding_tokenizer",
                execution.embedding_tokenizer.as_str(),
            ),
            ("detector_identity", execution.detector_identity.as_str()),
            ("schema_identity", execution.schema_identity.as_str()),
            ("index_identity", execution.index_identity.as_str()),
        ] {
            anyhow::ensure!(!value.is_empty(), "execution identity {name} is empty");
        }
        match &execution.source_policy {
            SourceExecutionPolicy::DurableSnapshot {
                reference,
                snapshot_digest,
            } => anyhow::ensure!(
                !reference.is_empty() && !snapshot_digest.is_empty(),
                "durable source snapshot identity is incomplete"
            ),
            SourceExecutionPolicy::AbortOnSourceChange { inventory_digest } => {
                anyhow::ensure!(
                    !inventory_digest.is_empty(),
                    "abort-on-change policy needs inventory digest"
                )
            }
        }
    }
    validate_groups(&manifest.groups)?;
    validate_plan_projection(&manifest.plan, &manifest.groups)
}

fn validate_groups(groups: &[ManifestGroup]) -> Result<()> {
    let mut group_ids = HashSet::new();
    let mut content_ids = HashSet::new();
    let mut path_ids = HashSet::new();
    for group in groups {
        anyhow::ensure!(group_ids.insert(&group.group_id), "duplicate group ID");
        anyhow::ensure!(
            content_ids.insert(&group.content_id),
            "duplicate collision-safe content ID"
        );
        anyhow::ensure!(
            !group.content_digest.is_empty(),
            "group has no raw content digest"
        );
        anyhow::ensure!(
            native_path_id_supported(&group.canonical.path_id),
            "canonical path identity is unsupported"
        );
        anyhow::ensure!(
            !group.canonical.is_symlink || group.aliases.iter().all(|p| p.is_symlink),
            "a symlink cannot be canonical while a real path survives"
        );
        anyhow::ensure!(
            group.aliases.iter().all(|alias| {
                (group.canonical.is_symlink, group.canonical.path_id.as_str())
                    < (alias.is_symlink, alias.path_id.as_str())
            }),
            "canonical path does not have the lowest canonical rank"
        );
        for path in group.all_paths() {
            anyhow::ensure!(
                path_ids.insert(&path.path_id),
                "native path identity occurs in multiple groups"
            );
        }
        anyhow::ensure!(
            group.aliases.windows(2).all(|pair| {
                (pair[0].is_symlink, pair[0].path_id.as_str())
                    < (pair[1].is_symlink, pair[1].path_id.as_str())
            }),
            "aliases are not in canonical rank order"
        );
        anyhow::ensure!(
            group.dataset_slugs.windows(2).all(|pair| pair[0] < pair[1]),
            "dataset assignments are not canonical"
        );
    }
    anyhow::ensure!(
        groups
            .windows(2)
            .all(|pair| pair[0].group_id < pair[1].group_id),
        "manifest groups are not canonical"
    );
    Ok(())
}

fn validate_plan_projection(plan: &Plan, groups: &[ManifestGroup]) -> Result<()> {
    let dataset_slugs: HashSet<&str> = plan
        .datasets
        .iter()
        .map(|dataset| dataset.slug.as_str())
        .collect();
    anyhow::ensure!(
        dataset_slugs.len() == plan.datasets.len(),
        "plan has duplicate dataset slugs"
    );
    let groups_by_content: HashMap<&str, &ManifestGroup> = groups
        .iter()
        .map(|group| (group.content_id.as_str(), group))
        .collect();
    anyhow::ensure!(
        plan.files.len() == groups.len(),
        "plan files and manifest groups disagree"
    );
    for (content_id, assignment) in &plan.files {
        let group = groups_by_content
            .get(content_id.as_str())
            .context("plan file has no manifest group")?;
        anyhow::ensure!(
            assignment.rel == group.canonical.rel
                && assignment.path_id == group.canonical.path_id
                && assignment.is_symlink == Some(group.canonical.is_symlink)
                && assignment.content_digest.as_deref() == Some(group.content_digest.as_str()),
            "plan canonical projection disagrees with manifest group"
        );
        let mut assigned: Vec<String> = assignment
            .assignments
            .iter()
            .map(|(_, slug)| slug.clone())
            .collect();
        assigned.sort();
        assigned.dedup();
        anyhow::ensure!(
            assigned == group.dataset_slugs,
            "plan dataset projection disagrees with manifest group"
        );
        anyhow::ensure!(
            assigned
                .iter()
                .all(|slug| dataset_slugs.contains(slug.as_str())),
            "group references absent dataset schema"
        );
    }
    let expected_aliases: BTreeSet<(String, String, String, bool, String)> = groups
        .iter()
        .flat_map(|group| {
            group.aliases.iter().map(|alias| {
                (
                    group.content_id.clone(),
                    alias.rel.clone(),
                    alias.path_id.clone(),
                    alias.is_symlink,
                    group.canonical.rel.clone(),
                )
            })
        })
        .collect();
    let actual_aliases: BTreeSet<(String, String, String, bool, String)> = plan
        .duplicate_files
        .iter()
        .map(|alias: &DuplicateFile| -> Result<_> {
            Ok((
                alias.file_key.clone(),
                alias.rel.clone(),
                alias.path_id.clone(),
                alias
                    .is_symlink
                    .context("plan alias has no persisted symlink rank")?,
                alias.duplicate_of.clone(),
            ))
        })
        .collect::<Result<_>>()?;
    anyhow::ensure!(
        actual_aliases == expected_aliases,
        "plan alias projection disagrees with manifest groups"
    );
    Ok(())
}

fn canonicalize_manifest_order(manifest: &mut GenerationManifest) {
    manifest.plan.datasets.sort_by(|left, right| {
        left.slug
            .cmp(&right.slug)
            .then_with(|| left.index.cmp(&right.index))
            .then_with(|| left.group.cmp(&right.group))
    });
    for dataset in &mut manifest.plan.datasets {
        dataset
            .specs
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
    for assignment in manifest.plan.files.values_mut() {
        assignment.assignments.sort();
        assignment.assignments.dedup();
    }
    manifest.plan.junk_files.sort_by(|left, right| {
        left.file_key
            .cmp(&right.file_key)
            .then_with(|| left.rel.cmp(&right.rel))
    });
    manifest.plan.duplicate_files.sort_by(|left, right| {
        left.file_key
            .cmp(&right.file_key)
            .then_with(|| left.path_id.cmp(&right.path_id))
            .then_with(|| left.rel.cmp(&right.rel))
    });
    for group in &mut manifest.groups {
        group.aliases.sort_by(|left, right| {
            (left.is_symlink, left.path_id.as_str())
                .cmp(&(right.is_symlink, right.path_id.as_str()))
        });
        group.dataset_slugs.sort();
        group.dataset_slugs.dedup();
    }
    manifest
        .groups
        .sort_by(|left, right| left.group_id.cmp(&right.group_id));
}

pub fn manifest_digest(manifest: &GenerationManifest) -> Result<String> {
    let mut normalized = manifest.clone();
    canonicalize_manifest_order(&mut normalized);
    let encoded = canonical_json_bytes(&normalized)?;
    Ok(format!(
        "axm1-{:032x}",
        xxhash_rust::xxh3::xxh3_128(&encoded)
    ))
}

pub fn operation_hash(operations: &[SyncOperation]) -> Result<String> {
    let mut normalized = operations.to_vec();
    normalized.sort();
    let encoded = canonical_json_bytes(&normalized)?;
    Ok(format!(
        "axo1-{:032x}",
        xxhash_rust::xxh3::xxh3_128(&encoded)
    ))
}

fn canonical_json_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    fn canonicalize(value: Value) -> Value {
        match value {
            Value::Object(object) => {
                let sorted: BTreeMap<String, Value> = object
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
            scalar => scalar,
        }
    }
    Ok(serde_json::to_vec(&canonicalize(serde_json::to_value(
        value,
    )?))?)
}

pub fn apply_begin(
    committed: &CommittedManifest,
    pending: &mut Option<PendingSync>,
    begin: PendingSync,
) -> Result<()> {
    anyhow::ensure!(pending.is_none(), "another sync is already pending");
    begin.validate_against(committed)?;
    *pending = Some(begin);
    Ok(())
}

pub fn is_sync_record_kind(kind: &str) -> bool {
    matches!(
        kind,
        "sync_begin" | "sync_operation_state" | "sync_validated" | "sync_commit" | "sync_abort"
    )
}

pub fn replay_record(
    record: &Value,
    committed: &mut Option<CommittedManifest>,
    pending: &mut Option<PendingSync>,
) -> Result<()> {
    let kind = record_str(record, "kind")?;
    match kind {
        "sync_begin" => {
            let begin: PendingSync =
                serde_json::from_value(record.clone()).context("decode durable sync_begin")?;
            apply_begin(
                committed
                    .as_ref()
                    .context("sync_begin has no committed base manifest")?,
                pending,
                begin,
            )
        }
        "sync_operation_state" => {
            let tx = record_str(record, "tx_id")?;
            let operation = record_str(record, "operation_id")?;
            let state = serde_json::from_value(
                record
                    .get("state")
                    .cloned()
                    .context("sync operation state is missing")?,
            )?;
            pending
                .as_mut()
                .context("operation state has no pending sync")?
                .apply_operation_state(tx, operation, state)
        }
        "sync_validated" => pending
            .as_mut()
            .context("validation has no pending sync")?
            .apply_validated(
                record_str(record, "tx_id")?,
                record_str(record, "desired_manifest_digest")?,
            ),
        "sync_commit" => {
            let active = pending.take().context("commit has no pending sync")?;
            *committed = Some(active.apply_commit(
                record_str(record, "tx_id")?,
                record_str(record, "desired_manifest_digest")?,
            )?);
            Ok(())
        }
        "sync_abort" => {
            let active = pending.as_ref().context("abort has no pending sync")?;
            anyhow::ensure!(
                active.tx_id == record_str(record, "tx_id")?
                    && active.desired_manifest_digest
                        == record_str(record, "desired_manifest_digest")?,
                "abort does not bind the pending transaction and manifest"
            );
            *pending = None;
            Ok(())
        }
        _ => anyhow::bail!("unknown sync record kind {kind}"),
    }
}

fn record_str<'a>(record: &'a Value, field: &str) -> Result<&'a str> {
    record
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("sync record has no {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FileAssignment, PlanDataset};

    fn path(id_hex: &str, symlink: bool) -> ManifestPath {
        ManifestPath {
            path_id: format!("unix:{id_hex}"),
            rel: format!("{id_hex}.pdf"),
            is_symlink: symlink,
        }
    }

    fn group(id: &str, content: &str, path: ManifestPath) -> ManifestGroup {
        ManifestGroup {
            group_id: id.into(),
            content_id: content.into(),
            content_digest: format!("digest-{content}"),
            content_size: 10,
            canonical: path,
            aliases: Vec::new(),
            dataset_slugs: vec!["reports".into()],
            expected_records: 1,
            expected_passages: 1,
            expected_vectors: 1,
        }
    }

    fn desired(content: &str, paths: Vec<ManifestPath>) -> DesiredContentGroup {
        DesiredContentGroup {
            content_id: content.into(),
            content_digest: format!("digest-{content}"),
            content_size: 10,
            paths,
            dataset_slugs: vec!["reports".into()],
            expected_records: 1,
            expected_passages: 1,
            expected_vectors: 1,
        }
    }

    #[test]
    fn canonical_prefers_real_then_native_bytes_including_non_utf8_identity() {
        let groups = reconcile_groups(
            &[],
            vec![desired(
                "content",
                vec![path("ff", false), path("01", true), path("80", false)],
            )],
        )
        .unwrap();
        assert_eq!(groups[0].canonical.path_id, "unix:80");
        assert!(!groups[0].canonical.is_symlink);
    }

    #[test]
    fn equal_raw_digest_does_not_merge_distinct_collision_safe_content_ids() {
        let mut a = desired("verified-a", vec![path("61", false)]);
        let mut b = desired("verified-b", vec![path("62", false)]);
        b.content_digest = a.content_digest.clone();
        a.content_size = 9;
        b.content_size = 9;
        let groups = reconcile_groups(&[], vec![a, b]).unwrap();
        assert_eq!(groups.len(), 2);
        assert_ne!(groups[0].content_id, groups[1].content_id);
    }

    #[test]
    fn changed_former_canonical_cannot_steal_surviving_content_group() {
        let mut old = group("logical", "old-content", path("61", false));
        old.aliases.push(path("62", false));
        let result = reconcile_groups(
            &[old],
            vec![
                desired("new-content", vec![path("61", false)]),
                desired("old-content", vec![path("62", false)]),
            ],
        )
        .unwrap();
        assert_eq!(
            result
                .iter()
                .find(|group| group.content_id == "old-content")
                .unwrap()
                .group_id,
            "logical"
        );
    }

    #[test]
    fn operation_planner_covers_add_delete_metadata_and_mixed() {
        let unchanged = group("same", "same", path("01", false));
        let deleted = group("gone", "gone", path("02", false));
        let metadata_old = group("meta", "meta", path("03", false));
        let mut metadata_new = metadata_old.clone();
        metadata_new.expected_records = 2;
        let added = group("new", "new", path("04", false));
        let operations = plan_operations(
            &[deleted, metadata_old, unchanged.clone()],
            &[added, metadata_new, unchanged],
        );
        assert_eq!(
            operations
                .iter()
                .map(|operation| operation.kind.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                SyncOperationKind::Delete,
                SyncOperationKind::Metadata,
                SyncOperationKind::Upsert
            ])
        );
    }

    #[test]
    fn legacy_nonempty_plan_requires_explicit_migration() {
        let mut plan = Plan::default();
        plan.files.insert(
            "key".into(),
            FileAssignment {
                rel: "a.pdf".into(),
                path_id: "unix:61".into(),
                is_symlink: None,
                family: "pdf".into(),
                gzip: false,
                content_digest: Some("digest".into()),
                assignments: Vec::new(),
            },
        );
        assert!(matches!(
            CommittedManifest::bootstrap_legacy(plan).unwrap(),
            LegacyBootstrap::MigrationRequired { .. }
        ));
    }

    #[test]
    fn canonical_digest_ignores_hashmap_and_semantic_group_order() {
        let mut first = Plan::default();
        let mut second = Plan::default();
        for (plan, order) in [(&mut first, ["a", "b"]), (&mut second, ["b", "a"])] {
            for key in order {
                plan.files.insert(
                    key.into(),
                    FileAssignment {
                        rel: format!("{key}.pdf"),
                        path_id: format!("unix:{:02x}", key.as_bytes()[0]),
                        is_symlink: Some(false),
                        family: "pdf".into(),
                        gzip: false,
                        content_digest: Some(format!("digest-{key}")),
                        assignments: Vec::new(),
                    },
                );
            }
        }
        let group_a = group("a", "a", path("61", false));
        let group_b = group("b", "b", path("62", false));
        let a = GenerationManifest {
            generation: 0,
            execution: None,
            plan: first,
            groups: vec![group_a.clone(), group_b.clone()],
        };
        let b = GenerationManifest {
            generation: 0,
            execution: None,
            plan: second,
            groups: vec![group_b, group_a],
        };
        assert_eq!(manifest_digest(&a).unwrap(), manifest_digest(&b).unwrap());
    }

    fn identity(inventory: &str) -> ExecutionIdentity {
        ExecutionIdentity {
            version: EXECUTION_IDENTITY_VERSION,
            root_identity: "unix:root".into(),
            url: "url".into(),
            prefix: "prefix".into(),
            follow_symlinks: false,
            chunker_identity: "chunker-v1".into(),
            embedding_backend: "lexical".into(),
            embedding_model: "feature-hash".into(),
            embedding_tokenizer: "builtin".into(),
            embedding_dimension: 384,
            graph_enabled: false,
            brain: "none".into(),
            detector_identity: "detectors-v1".into(),
            schema_identity: "schema-v1".into(),
            index_identity: "index-v1".into(),
            source_policy: SourceExecutionPolicy::AbortOnSourceChange {
                inventory_digest: inventory.into(),
            },
        }
    }

    #[test]
    fn no_op_plan_or_bound_execution_change_is_refused() {
        let mut legacy_plan = Plan::default();
        legacy_plan.datasets.push(PlanDataset {
            slug: "reports".into(),
            index: "prefix-reports".into(),
            family: "pdf".into(),
            group: None,
            specs: Vec::new(),
            time_field: None,
            semantic_field: None,
            sampled_records: 0,
            file_count: 0,
        });
        let LegacyBootstrap::Ready(legacy) =
            CommittedManifest::bootstrap_legacy(legacy_plan.clone()).unwrap()
        else {
            panic!("empty legacy membership is migratable");
        };
        let first = GenerationManifest {
            generation: 1,
            execution: Some(identity("inventory-a")),
            plan: legacy_plan.clone(),
            groups: Vec::new(),
        };
        let pending = PendingSync::new("bind".into(), &legacy, first.clone()).unwrap();

        let committed =
            CommittedManifest::from_generation(first.clone(), pending.desired_manifest_digest);
        let mut changed_execution = first.clone();
        changed_execution.generation = 2;
        changed_execution.execution = Some(identity("inventory-b"));
        assert!(
            PendingSync::new("changed-execution".into(), &committed, changed_execution).is_err()
        );

        let mut changed_plan = first;
        changed_plan.plan.alias_paths_indexed = true;
        assert!(PendingSync::new("changed-plan".into(), &legacy, changed_plan).is_err());
    }

    #[test]
    fn serialized_malicious_operation_list_is_rejected_against_manifests() {
        let LegacyBootstrap::Ready(base) =
            CommittedManifest::bootstrap_legacy(Plan::default()).unwrap()
        else {
            panic!("empty legacy plan is ready");
        };
        let desired = GenerationManifest {
            generation: 1,
            execution: Some(identity("inventory-a")),
            plan: Plan::default(),
            groups: Vec::new(),
        };
        let valid = PendingSync::new("tx".into(), &base, desired).unwrap();
        let mut encoded = serde_json::to_value(valid).unwrap();
        encoded["operations"] = serde_json::json!([{
            "operation_id": "delete:invented",
            "kind": "delete",
            "group_id": "invented"
        }]);
        let malicious: PendingSync = serde_json::from_value(encoded).unwrap();
        assert!(malicious.validate_against(&base).is_err());
    }
}
