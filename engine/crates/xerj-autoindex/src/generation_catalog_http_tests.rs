//! Backend-contract tests for whole-generation catalog publication.
//!
//! The in-memory endpoint models the observable HTTP contract: a bulk response
//! may be accepted remotely and lost locally, so applying one exact projection
//! twice must converge. Correlation IDs are supplied as independently observed
//! prior-generation IDs because the current committed manifest does not persist
//! them; a real backend must query them by the prior `run_id`.

use crate::catalog;
use crate::generation_catalog::{
    expected_ax_run, project_generation, CatalogProjection, DatasetCatalogStats,
    GenerationCatalogMetadata,
};
use crate::state::{DuplicateFile, FileAssignment, JunkFile, Plan, PlanDataset};
use crate::sync::{
    CommittedManifest, ExecutionIdentity, GenerationManifest, ManifestGroup, ManifestPath,
    SourceExecutionPolicy, SyncOperationKind, EXECUTION_IDENTITY_VERSION,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct CatalogEndpoint {
    documents: BTreeMap<String, Value>,
    fail_after_accept_once: bool,
}

impl CatalogEndpoint {
    fn publish(
        &mut self,
        projection: &CatalogProjection,
        independently_observed_stale: &BTreeSet<String>,
    ) -> Result<()> {
        for id in projection
            .stale_ids
            .iter()
            .chain(independently_observed_stale)
        {
            self.documents.remove(id);
        }
        self.documents.extend(projection.documents.clone());
        if std::mem::take(&mut self.fail_after_accept_once) {
            anyhow::bail!("response lost after the endpoint accepted the publication");
        }
        Ok(())
    }

    fn desired_readback(&self, projection: &CatalogProjection) -> BTreeMap<String, Value> {
        projection
            .documents
            .keys()
            .filter_map(|id| self.documents.get(id).cloned().map(|doc| (id.clone(), doc)))
            .collect()
    }
}

fn path_id(path: &str) -> String {
    let encoded = path
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("unix:{encoded}")
}

fn dataset() -> PlanDataset {
    PlanDataset {
        slug: "reports".into(),
        index: "ax-reports".into(),
        family: "csv".into(),
        group: None,
        specs: Vec::new(),
        time_field: None,
        semantic_field: None,
        sampled_records: 2,
        file_count: 0,
    }
}

fn assignment(path: &str) -> FileAssignment {
    FileAssignment {
        rel: path.into(),
        path_id: path_id(path),
        is_symlink: Some(false),
        family: "csv".into(),
        gzip: false,
        content_digest: Some(format!("digest-{path}")),
        assignments: vec![(None, "reports".into())],
        as_document: false,
    }
}

fn group(key: &str, path: &str, aliases: &[&str]) -> ManifestGroup {
    ManifestGroup {
        group_id: format!("group-{key}"),
        content_id: key.into(),
        content_digest: format!("digest-{path}"),
        content_size: 20,
        canonical: ManifestPath {
            path_id: path_id(path),
            rel: path.into(),
            is_symlink: false,
        },
        aliases: aliases
            .iter()
            .map(|alias| ManifestPath {
                path_id: path_id(alias),
                rel: (*alias).into(),
                is_symlink: false,
            })
            .collect(),
        dataset_slugs: vec!["reports".into()],
        expected_records: 2,
        expected_passages: 0,
        expected_vectors: 0,
        expected_junk_records: 0,
        expected_records_by_dataset: BTreeMap::from([("reports".to_string(), 2)]),
    }
}

fn junk(key: &str, path: &str, reason: &str) -> JunkFile {
    JunkFile {
        file_key: key.into(),
        rel: path.into(),
        format: "binary".into(),
        status: "junk".into(),
        reason: reason.into(),
        bytes: 9,
    }
}

fn execution(generation_id: &str) -> ExecutionIdentity {
    ExecutionIdentity {
        version: EXECUTION_IDENTITY_VERSION,
        root_identity: "/finance".into(),
        url: "http://engine".into(),
        prefix: "ax".into(),
        follow_symlinks: false,
        chunker_identity: "prepared-records-v1".into(),
        embedding_identity_sha256: "a".repeat(64),
        embedding_backend: "lexical".into(),
        embedding_dimension: Some(384),
        embedding_semantic_contract: "semantic_text-derived-vector.v1".into(),
        embedding_resumable: true,
        graph_enabled: false,
        brain: "none".into(),
        detector_identity: "disabled".into(),
        schema_identity: "schema".into(),
        index_identity: "index".into(),
        source_policy: SourceExecutionPolicy::DurableSnapshot {
            reference: format!("sync-snapshots/{generation_id}"),
            snapshot_digest: format!("snapshot-{generation_id}"),
        },
    }
}

fn generation(
    number: u64,
    generation_id: &str,
    mut plan: Plan,
    groups: Vec<ManifestGroup>,
) -> GenerationManifest {
    for dataset in &mut plan.datasets {
        dataset.file_count = groups
            .iter()
            .filter(|group| group.dataset_slugs.contains(&dataset.slug))
            .count();
    }
    GenerationManifest {
        generation: number,
        execution: Some(execution(generation_id)),
        plan,
        groups,
    }
}

fn committed(generation: GenerationManifest) -> CommittedManifest {
    CommittedManifest {
        generation: generation.generation,
        manifest_digest: format!("manifest-{}", generation.generation),
        plan: generation.plan,
        groups: generation.groups,
        execution: generation.execution,
    }
}

fn metadata(id: &str) -> GenerationCatalogMetadata {
    GenerationCatalogMetadata {
        generation_id: id.into(),
        started: "2026-08-03T12:00:00Z".into(),
    }
}

fn stats(records: u64, bytes: u64) -> BTreeMap<String, DatasetCatalogStats> {
    BTreeMap::from([(
        "reports".into(),
        DatasetCatalogStats {
            record_count: records,
            junk_records: 0,
            bytes,
            formats: vec!["csv".into()],
            time_min: None,
            time_max: None,
            sample_queries: vec![json!({"class": "full_text"})],
            notes: Vec::new(),
        },
    )])
}

fn plan(indexed: &[(&str, &str)], junk_files: Vec<JunkFile>) -> Plan {
    let mut plan = Plan {
        datasets: vec![dataset()],
        junk_files,
        ..Plan::default()
    };
    for (key, path) in indexed {
        plan.files.insert((*key).into(), assignment(path));
    }
    plan
}

fn prior_managed_ids(base: &CommittedManifest) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    ids.extend(
        base.plan
            .files
            .keys()
            .map(|key| catalog::file_id("ax", key)),
    );
    ids.extend(
        base.plan
            .junk_files
            .iter()
            .map(|junk| catalog::file_id("ax", &junk.file_key)),
    );
    ids.extend(base.plan.duplicate_files.iter().map(|alias| {
        catalog::duplicate_file_id("ax", &alias.file_key, &alias.rel, &alias.path_id)
    }));
    ids.extend(
        base.plan
            .datasets
            .iter()
            .map(|dataset| format!("ds:ax:{}", dataset.slug)),
    );
    ids
}

#[test]
fn junk_only_add_change_and_delete_converge_without_hiding_unchanged_data() {
    let base_generation = generation(
        1,
        "g1",
        plan(&[("keep", "keep.csv")], vec![]),
        vec![group("keep", "keep.csv", &[])],
    );
    let base = committed(base_generation);
    let mut endpoint = CatalogEndpoint::default();

    let added = generation(
        2,
        "g2",
        plan(
            &[("keep", "keep.csv")],
            vec![junk("junk-a", "opaque.bin", "binary")],
        ),
        vec![group("keep", "keep.csv", &[])],
    );
    let projection = project_generation(
        &base,
        &added,
        &metadata("g2"),
        &stats(2, 20),
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();
    endpoint.publish(&projection, &BTreeSet::new()).unwrap();
    assert_eq!(endpoint.documents["file:ax:junk-a"]["status"], "junk");
    assert_eq!(endpoint.documents["file:ax:keep"]["run_id"], "g2");

    let base = committed(added);
    let changed = generation(
        3,
        "g3",
        plan(
            &[("keep", "keep.csv")],
            vec![junk("junk-a", "opaque.bin", "unsupported archive")],
        ),
        vec![group("keep", "keep.csv", &[])],
    );
    let projection = project_generation(
        &base,
        &changed,
        &metadata("g3"),
        &stats(2, 20),
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();
    endpoint.publish(&projection, &BTreeSet::new()).unwrap();
    assert_eq!(
        endpoint.documents["file:ax:junk-a"]["reason"],
        "unsupported archive"
    );

    let base = committed(changed);
    let deleted = generation(
        4,
        "g4",
        plan(&[("keep", "keep.csv")], vec![]),
        vec![group("keep", "keep.csv", &[])],
    );
    let projection = project_generation(
        &base,
        &deleted,
        &metadata("g4"),
        &stats(2, 20),
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();
    assert!(projection.stale_ids.contains("file:ax:junk-a"));
    endpoint.publish(&projection, &BTreeSet::new()).unwrap();
    assert!(!endpoint.documents.contains_key("file:ax:junk-a"));
    assert_eq!(endpoint.documents["file:ax:keep"]["run_id"], "g4");
}

#[test]
fn indexed_and_junk_transitions_replace_the_same_catalog_identity() {
    let indexed = generation(
        1,
        "g1",
        plan(&[("same", "report.csv")], vec![]),
        vec![group("same", "report.csv", &[])],
    );
    let base = committed(indexed);
    let became_junk = generation(
        2,
        "g2",
        plan(
            &[],
            vec![junk("same", "report.csv", "no records extracted")],
        ),
        Vec::new(),
    );
    let mut empty_stats = stats(0, 0);
    empty_stats.get_mut("reports").unwrap().formats.clear();
    let junk_projection = project_generation(
        &base,
        &became_junk,
        &metadata("g2"),
        &empty_stats,
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();
    assert_eq!(junk_projection.documents["file:ax:same"]["status"], "junk");
    assert!(!junk_projection.stale_ids.contains("file:ax:same"));

    let base = committed(became_junk);
    let indexed_again = generation(
        3,
        "g3",
        plan(&[("same", "report.csv")], vec![]),
        vec![group("same", "report.csv", &[])],
    );
    let projection = project_generation(
        &base,
        &indexed_again,
        &metadata("g3"),
        &stats(2, 20),
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();
    assert_eq!(projection.documents["file:ax:same"]["status"], "indexed");
    assert!(!projection.stale_ids.contains("file:ax:same"));
}

#[test]
fn accepted_response_retry_is_idempotent_and_exactly_validated() {
    let base = committed(generation(
        1,
        "g1",
        plan(&[("a", "a.csv")], vec![]),
        vec![group("a", "a.csv", &[])],
    ));
    let desired = generation(
        2,
        "g2",
        plan(&[("a", "a.csv"), ("b", "b.csv")], vec![]),
        vec![group("a", "a.csv", &[]), group("b", "b.csv", &[])],
    );
    let projection = project_generation(
        &base,
        &desired,
        &metadata("g2"),
        &stats(4, 40),
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();
    let mut endpoint = CatalogEndpoint {
        fail_after_accept_once: true,
        ..CatalogEndpoint::default()
    };
    assert!(endpoint.publish(&projection, &BTreeSet::new()).is_err());
    endpoint.publish(&projection, &BTreeSet::new()).unwrap();
    projection
        .validate_observed(&endpoint.desired_readback(&projection))
        .unwrap();
}

#[test]
fn exact_run_dataset_file_ids_and_payloads_are_generation_bound() {
    let base = committed(generation(1, "g1", plan(&[], vec![]), Vec::new()));
    let desired = generation(
        2,
        "g2",
        plan(&[("report", "report.csv")], vec![]),
        vec![group("report", "report.csv", &[])],
    );
    let projection = project_generation(
        &base,
        &desired,
        &metadata("g2"),
        &stats(2, 20),
        &BTreeMap::new(),
        &prior_managed_ids(&base),
    )
    .unwrap();

    assert_eq!(
        projection
            .documents
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "ds:ax:reports".into(),
            "file:ax:report".into(),
            "run:g2".into()
        ])
    );
    assert_eq!(projection.documents["run:g2"]["records_total"], 2);
    assert_eq!(projection.documents["run:g2"]["files_total"], 1);
    assert_eq!(projection.documents["ds:ax:reports"]["record_count"], 2);
    assert_eq!(projection.documents["file:ax:report"]["records"], 2);
    assert!(projection
        .documents
        .values()
        .all(|doc| doc["run_id"] == "g2"));
}

#[test]
fn stale_alias_and_independently_observed_correlation_are_deleted() {
    let alias = DuplicateFile {
        file_key: "report".into(),
        rel: "copy.csv".into(),
        path_id: path_id("copy.csv"),
        is_symlink: Some(false),
        duplicate_of: "report.csv".into(),
        bytes: 20,
    };
    let mut base_plan = plan(&[("report", "report.csv")], vec![]);
    base_plan.duplicate_files.push(alias.clone());
    let base = committed(generation(
        1,
        "g1",
        base_plan,
        vec![group("report", "report.csv", &["copy.csv"])],
    ));
    let desired = generation(
        2,
        "g2",
        plan(&[("report", "report.csv")], vec![]),
        vec![group("report", "report.csv", &[])],
    );
    let stale_correlation = "corr:reports:old".to_string();
    let mut observed_prior = prior_managed_ids(&base);
    observed_prior.insert(stale_correlation.clone());
    let projection = project_generation(
        &base,
        &desired,
        &metadata("g2"),
        &stats(2, 20),
        &BTreeMap::new(),
        &observed_prior,
    )
    .unwrap();
    let alias_id = catalog::duplicate_file_id("ax", &alias.file_key, &alias.rel, &alias.path_id);
    assert!(projection.stale_ids.contains(&alias_id));

    let mut endpoint = CatalogEndpoint::default();
    endpoint
        .documents
        .insert(alias_id.clone(), json!({"doc_kind": "file"}));
    endpoint.documents.insert(
        stale_correlation.clone(),
        json!({"doc_kind": "correlation", "run_id": "g1"}),
    );
    endpoint.publish(&projection, &BTreeSet::new()).unwrap();
    assert!(!endpoint.documents.contains_key(&alias_id));
    assert!(!endpoint.documents.contains_key(&stale_correlation));
}

#[test]
fn metadata_publication_preserves_ax_run_while_upsert_changes_it() {
    let mut live_document = json!({"ax_path": "old.csv", "ax_run": "g1"});
    let preserved = expected_ax_run(
        &SyncOperationKind::Metadata,
        "g2",
        live_document.get("ax_run").and_then(Value::as_str),
    )
    .unwrap()
    .unwrap()
    .to_owned();
    live_document["ax_path"] = Value::String("renamed.csv".into());
    live_document["ax_run"] = Value::String(preserved);
    assert_eq!(live_document["ax_run"], "g1");

    let replacement = expected_ax_run(&SyncOperationKind::Upsert, "g3", Some("g1"))
        .unwrap()
        .unwrap();
    live_document["ax_run"] = Value::String(replacement.into());
    assert_eq!(live_document["ax_run"], "g3");
}
