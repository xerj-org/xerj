//! Apply committed cluster-metadata commands to the local engine.
//!
//! The HTTP layer proposes. This module runs on the Raft task, after the
//! entry has committed, and materialises the same index create, delete,
//! mapping, or settings change on every member. It must not propose.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use xerj_cluster::{ApplyEffect, ClusterCommand, MetadataApplier, ProposeError};
use xerj_common::types::{FieldConfig, Schema};

use crate::{Engine, EngineError, Result};

/// What the accepting node resolved for `PUT /{index}` before proposing.
///
/// Templates are evaluated there. Peers create this schema and do not run
/// their own templates, so two nodes with different local templates still
/// converge.
#[derive(Debug, Clone)]
pub struct ReplicatedIndexSpec {
    pub name: String,
    pub schema: Schema,
    pub settings: Value,
    pub mappings: Value,
    pub index_settings: Value,
    pub aliases: Value,
}

/// Engine applier installed on the cluster node before the runner starts.
pub struct EngineMetadataApplier {
    pub engine: Arc<Engine>,
}

#[async_trait::async_trait]
impl MetadataApplier for EngineMetadataApplier {
    async fn apply_metadata(&self, command: &ClusterCommand) -> ApplyEffect {
        apply_committed(&self.engine, command.as_metadata()).await
    }

    async fn backfill_local_replicas(&self) {
        self.engine.backfill_out_of_sync_replicas().await;
    }
}

pub(crate) fn propose_error(err: ProposeError) -> EngineError {
    EngineError::Common(xerj_common::XerjError::cluster_unavailable(err.to_string()))
}

pub(crate) fn effect_result(index: &str, effect: ApplyEffect) -> Result<()> {
    match effect {
        ApplyEffect::Applied => Ok(()),
        ApplyEffect::AlreadyExists => Err(EngineError::Common(
            xerj_common::XerjError::index_already_exists(index),
        )),
        ApplyEffect::NotFound => Err(EngineError::Common(
            xerj_common::XerjError::index_not_found(index),
        )),
        ApplyEffect::Failed(reason) => Err(EngineError::Common(xerj_common::XerjError::internal(
            reason,
        ))),
    }
}

fn is_already_exists(err: &EngineError) -> bool {
    matches!(
        err,
        EngineError::Common(xerj_common::XerjError::IndexAlreadyExists { .. })
    )
}

fn is_not_found(err: &EngineError) -> bool {
    matches!(
        err,
        EngineError::Common(xerj_common::XerjError::IndexNotFound { .. })
    )
}

impl Engine {
    /// Propose an index create and wait until this node has applied it.
    ///
    /// The caller's template is applied once, here. Success means the index
    /// exists on this node and a majority has the log entry. No leader or a
    /// timeout leaves the index uncreated.
    pub async fn replicate_index_create(&self, mut spec: ReplicatedIndexSpec) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        if !crate::index_guard::visible(&spec.name) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(&spec.name),
            ));
        }
        let Some(handle) = self.cluster_handle() else {
            return Err(EngineError::Common(xerj_common::XerjError::internal(
                "replicate_index_create called with no cluster handle",
            )));
        };
        spec.schema = self.apply_index_template(&spec.name, spec.schema);
        let mut envelope = serde_json::json!({
            "v": 1,
            "kind": "index_create",
            "schema": spec.schema,
            "settings": spec.settings,
            "mappings": spec.mappings,
            "index_settings": spec.index_settings,
            "aliases": spec.aliases,
        });
        let replicas = xerj_cluster::number_of_replicas_from_settings(&spec.index_settings).max(
            xerj_cluster::number_of_replicas_from_settings(&spec.settings),
        );
        let plan = xerj_cluster::shard_plan(&handle.members(), replicas);
        if !plan.is_empty() {
            envelope["shard_plan"] = serde_json::to_value(&plan)?;
        }
        let schema_json = serde_json::to_string(&envelope)?;
        let effect = handle
            .propose(ClusterCommand::CreateIndex {
                name: spec.name.clone(),
                schema_json,
            })
            .await
            .map_err(propose_error)?;
        effect_result(&spec.name, effect)
    }

    /// Create `name` through the log when this node does not have it yet.
    ///
    /// An `AlreadyExists` commit is success: a concurrent create won, and
    /// this node applied that entry before its own. Used by document
    /// auto-create so a clustered write does not invent a node-local index.
    pub async fn ensure_replicated_index(&self, name: &str) -> Result<()> {
        if self.get_index(name).is_ok() {
            return Ok(());
        }
        // Same template merge as `PUT /{index}` (#2281). Peers apply this
        // spec as given and do not re-read their own templates.
        let (mappings, settings) = self.merge_index_template(name, Value::Null, Value::Null);
        let index_settings = if settings.as_object().is_some_and(|m| !m.is_empty()) {
            settings.clone()
        } else {
            Value::Null
        };
        let spec = ReplicatedIndexSpec {
            name: name.to_string(),
            schema: Schema::empty(),
            settings,
            mappings,
            index_settings,
            aliases: Value::Null,
        };
        match self.replicate_index_create(spec).await {
            Ok(()) => Ok(()),
            Err(e) if is_already_exists(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Ask each primary to copy live documents onto a replica shard this
    /// node holds that is not in the in-sync set.
    ///
    /// One pass. The cluster runner calls it again. A shard that is already
    /// in sync, or whose replica store is not open yet, is skipped. The
    /// primary copies the documents and publishes this node into the in-sync
    /// set. A pass that finds nothing out of sync does not call a peer.
    pub async fn backfill_out_of_sync_replicas(&self) {
        let me = self.routing_node_id();
        for index in self.index_name_list() {
            // Replica stores are hidden indices. The plan lives on the user
            // index, and that is the name the primary copies from.
            if index.starts_with('.') {
                continue;
            }
            for shard in self.local_replica_shards(&index) {
                if self.replica_nodes(&index, shard).iter().any(|id| id == &me) {
                    continue;
                }
                let Some(store) = xerj_cluster::replica_index_name(&index, shard) else {
                    continue;
                };
                if self.get_index(&store).is_err() {
                    continue;
                }
                let Some(primary) = self.shard_primary(&index, shard) else {
                    continue;
                };
                if primary == me {
                    continue;
                }
                let op = xerj_cluster::ShardOp::BackfillReplica {
                    index: index.clone(),
                    shard,
                    replica_id: me.clone(),
                };
                match self.forward_shard(&primary, op).await {
                    Ok(result) if (200..300).contains(&result.status) => {}
                    Ok(result) => {
                        tracing::debug!(
                            index = %index,
                            shard,
                            primary = %primary,
                            status = result.status,
                            body = %result.body,
                            "replica backfill was not applied"
                        );
                    }
                    Err(err) => {
                        tracing::debug!(
                            index = %index,
                            shard,
                            primary = %primary,
                            error = %err,
                            "replica backfill call failed"
                        );
                    }
                }
            }
        }
    }

    /// Forward one shard operation to `owner`. A failure is not a local write.
    pub async fn forward_shard(
        &self,
        owner: &str,
        op: xerj_cluster::ShardOp,
    ) -> Result<xerj_cluster::ShardResult> {
        let Some(handle) = self.cluster_handle() else {
            return Err(EngineError::Common(xerj_common::XerjError::internal(
                "forward_shard called with no cluster handle",
            )));
        };
        handle.shard_call(owner, op).await.map_err(|err| {
            EngineError::Common(xerj_common::XerjError::cluster_unavailable(err.to_string()))
        })
    }

    /// Propose one index's merged mapping. Callers validate before this.
    pub async fn replicate_mapping(
        &self,
        index: &str,
        mapping: &Value,
        additions: &[FieldConfig],
    ) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        if !crate::index_guard::visible(index) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(index),
            ));
        }
        let Some(handle) = self.cluster_handle() else {
            return Err(EngineError::Common(xerj_common::XerjError::internal(
                "replicate_mapping called with no cluster handle",
            )));
        };
        let envelope = serde_json::json!({
            "v": 1,
            "mapping": mapping,
            "add_fields": additions,
        });
        let effect = handle
            .propose(ClusterCommand::UpdateMapping {
                index: index.to_string(),
                mapping_json: serde_json::to_string(&envelope)?,
            })
            .await
            .map_err(propose_error)?;
        effect_result(index, effect)
    }

    /// Propose a `PUT /{index}/_settings` body. Analysis updates are refused
    /// by the handler before this is called. Lifecycle attach stays on the
    /// accepting node; it is not part of the command.
    pub async fn replicate_index_settings(&self, index: &str, body: &Value) -> Result<()> {
        self.ensure_cluster_state_writable()?;
        if !crate::index_guard::visible(index) {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(index),
            ));
        }
        let Some(handle) = self.cluster_handle() else {
            return Err(EngineError::Common(xerj_common::XerjError::internal(
                "replicate_index_settings called with no cluster handle",
            )));
        };
        let effect = handle
            .propose(ClusterCommand::UpdateIndexSettings {
                index: index.to_string(),
                settings_json: serde_json::to_string(body)?,
            })
            .await
            .map_err(propose_error)?;
        effect_result(index, effect)
    }

    /// Merge a settings body into the display map, then apply block keys.
    ///
    /// Same order as `PUT /{index}/_settings` on a single node: the display
    /// copy updates, `Index::apply_block_settings` updates enforcement, and
    /// the block map is copied back so `GET /_settings` matches.
    pub(crate) async fn apply_user_index_settings(&self, name: &str, body: &Value) -> Result<()> {
        if self.get_index(name).is_err() {
            return Err(EngineError::Common(
                xerj_common::XerjError::index_not_found(name),
            ));
        }
        let inner = body.get("index").cloned().unwrap_or_else(|| body.clone());
        let mut existing = self
            .index_settings
            .get(name)
            .map(|v| v.clone())
            .unwrap_or_else(|| serde_json::json!({ "index": {} }));
        let inner_slot = existing
            .as_object_mut()
            .and_then(|m| m.get_mut("index").and_then(Value::as_object_mut));
        match (inner_slot, inner.as_object()) {
            (Some(existing_inner), Some(new_inner)) => {
                for (k, v) in new_inner {
                    existing_inner.insert(k.clone(), v.clone());
                }
                self.index_settings.insert(name.to_string(), existing);
            }
            _ => {
                self.index_settings
                    .insert(name.to_string(), serde_json::json!({ "index": inner }));
            }
        }
        let idx = self.get_index(name)?;
        idx.apply_block_settings(body).await?;
        self.sync_display_blocks(name).await;
        Ok(())
    }

    /// Copy `index.blocks` from the index's settings file into the display map.
    pub(crate) async fn sync_display_blocks(&self, name: &str) {
        let Ok(idx) = self.get_index(name) else {
            return;
        };
        let blocks = idx
            .get_settings()
            .await
            .pointer("/index/blocks")
            .cloned()
            .filter(|b| b.as_object().is_some_and(|m| !m.is_empty()));
        let mut display = self
            .index_settings
            .get(name)
            .map(|v| v.clone())
            .unwrap_or_else(|| serde_json::json!({ "index": {} }));
        if !display.is_object() {
            display = serde_json::json!({ "index": {} });
        }
        let Some(root) = display.as_object_mut() else {
            return;
        };
        if !root.get("index").is_some_and(Value::is_object) {
            root.insert("index".to_string(), serde_json::json!({}));
        }
        let Some(inner) = root.get_mut("index").and_then(Value::as_object_mut) else {
            return;
        };
        inner.retain(|k, _| !k.starts_with("blocks."));
        match blocks {
            Some(b) => {
                inner.insert("blocks".to_string(), b);
            }
            None => {
                inner.remove("blocks");
            }
        }
        self.index_settings.insert(name.to_string(), display);
    }
}

#[derive(Deserialize)]
struct CreateEnvelope {
    v: u32,
    kind: String,
    schema: Schema,
    settings: Value,
    mappings: Value,
    index_settings: Value,
    aliases: Value,
}

#[derive(Deserialize)]
struct MappingEnvelope {
    v: u32,
    mapping: Value,
    add_fields: Vec<FieldConfig>,
}

async fn apply_committed(engine: &Engine, command: &ClusterCommand) -> ApplyEffect {
    match command {
        ClusterCommand::CreateIndex { name, schema_json } => {
            // Install owners before the index is visible. A GET that observes
            // the new index otherwise races a router that is still empty and
            // treats the write as local.
            let plan = xerj_cluster::shard_plan_from_schema_json(schema_json);
            let installed_early = !plan.is_empty() && engine.shard_search_targets(name).is_empty();
            if installed_early {
                engine.install_shard_plan(name, &plan);
            }
            let effect = apply_create(engine, name, schema_json).await;
            if installed_early
                && matches!(effect, ApplyEffect::Failed(_))
                && engine.get_index(name).is_err()
            {
                engine.clear_shard_plan(name);
            }
            effect
        }
        ClusterCommand::DeleteIndex { name } => {
            if let Err(reason) = delete_local_replica_stores(engine, name).await {
                return ApplyEffect::Failed(reason.to_string());
            }
            let effect = apply_delete(engine, name).await;
            engine.clear_shard_plan(name);
            effect
        }
        ClusterCommand::UpdateMapping {
            index,
            mapping_json,
        } => apply_mapping(engine, index, mapping_json).await,
        ClusterCommand::UpdateIndexSettings {
            index,
            settings_json,
        } => apply_settings(engine, index, settings_json).await,
        ClusterCommand::AssignShard {
            index,
            shard,
            node_id,
        } => {
            engine.assign_one_shard(index, *shard, node_id);
            ApplyEffect::Applied
        }
        ClusterCommand::SetShardInSync {
            index,
            shard,
            nodes,
        } => {
            engine.set_shard_in_sync(index, *shard, nodes);
            ApplyEffect::Applied
        }
        // Membership and generic config are stored by ClusterMetadata.
        // They do not change this node's indices.
        _ => ApplyEffect::Applied,
    }
}

async fn apply_create(engine: &Engine, name: &str, schema_json: &str) -> ApplyEffect {
    let envelope: CreateEnvelope = match serde_json::from_str(schema_json) {
        Ok(envelope) => envelope,
        Err(e) => {
            return ApplyEffect::Failed(format!(
                "index create for [{name}] is not a replicated spec: {e}"
            ))
        }
    };
    if envelope.v != 1 || envelope.kind != "index_create" {
        return ApplyEffect::Failed(format!(
            "index create for [{name}] has unsupported envelope v={} kind={}",
            envelope.v, envelope.kind
        ));
    }
    if engine.get_index(name).is_ok() {
        return match ensure_local_replica_stores(engine, name, &envelope.settings).await {
            Ok(()) => ApplyEffect::AlreadyExists,
            Err(reason) => ApplyEffect::Failed(reason.to_string()),
        };
    }
    if let Err(e) =
        engine.create_index_exact(name, envelope.schema.clone(), envelope.settings.clone())
    {
        return if is_already_exists(&e) {
            match ensure_local_replica_stores(engine, name, &envelope.settings).await {
                Ok(()) => ApplyEffect::AlreadyExists,
                Err(reason) => ApplyEffect::Failed(reason.to_string()),
            }
        } else {
            ApplyEffect::Failed(e.to_string())
        };
    }
    if !envelope.mappings.is_null() {
        if let Err(e) = engine.put_index_mapping(name, envelope.mappings) {
            return ApplyEffect::Failed(e.to_string());
        }
    }
    if !envelope.index_settings.is_null() {
        engine
            .index_settings
            .insert(name.to_string(), envelope.index_settings);
    }
    if let Some(aliases) = envelope.aliases.as_object() {
        for alias in aliases.keys() {
            if let Err(e) = engine.add_alias(alias, name) {
                return ApplyEffect::Failed(e.to_string());
            }
        }
        engine
            .index_alias_metadata
            .insert(name.to_string(), envelope.aliases);
        engine.flush_alias_metadata();
    }
    if let Err(reason) = ensure_local_replica_stores(engine, name, &envelope.settings).await {
        return ApplyEffect::Failed(reason.to_string());
    }
    ApplyEffect::Applied
}

/// Open a hidden replica index on this node for every shard it copies.
///
/// The schema is the user index's schema. `settings` is the create body the
/// primary index was built with, so a custom analyser on that index is the
/// one a failover search uses. Replica placement does not run again: this
/// path calls `create_index_exact`, not the replicated create.
async fn ensure_local_replica_stores(
    engine: &Engine,
    name: &str,
    settings: &Value,
) -> std::result::Result<(), String> {
    let shards = engine.local_replica_shards(name);
    if shards.is_empty() {
        return Ok(());
    }
    let idx = engine
        .get_index(name)
        .map_err(|err| format!("replica store for [{name}]: {err}"))?;
    let schema = idx.schema().await;
    for shard in shards {
        let Some(store) = xerj_cluster::replica_index_name(name, shard) else {
            return Err(format!(
                "replica index name for [{name}] shard {shard} is not a valid index name"
            ));
        };
        if engine.get_index(&store).is_ok() {
            continue;
        }
        engine
            .create_index_exact(&store, schema.clone(), settings.clone())
            .map_err(|err| format!("replica store [{store}]: {err}"))?;
    }
    Ok(())
}

/// Drop this node's replica stores for `name` while the plan is still installed.
async fn delete_local_replica_stores(
    engine: &Engine,
    name: &str,
) -> std::result::Result<(), String> {
    for shard in engine.local_replica_shards(name) {
        let Some(store) = xerj_cluster::replica_index_name(name, shard) else {
            continue;
        };
        match engine.delete_index_local(&store).await {
            Ok(()) => {}
            Err(err) if is_not_found(&err) => {}
            Err(err) => return Err(format!("removing replica store [{store}]: {err}")),
        }
    }
    Ok(())
}

async fn apply_delete(engine: &Engine, name: &str) -> ApplyEffect {
    match engine.delete_index_local(name).await {
        Ok(()) => ApplyEffect::Applied,
        Err(e) if is_not_found(&e) => ApplyEffect::NotFound,
        Err(e) => ApplyEffect::Failed(e.to_string()),
    }
}

async fn apply_mapping(engine: &Engine, index: &str, mapping_json: &str) -> ApplyEffect {
    let envelope: MappingEnvelope = match serde_json::from_str(mapping_json) {
        Ok(envelope) => envelope,
        Err(e) => {
            return ApplyEffect::Failed(format!(
                "mapping update for [{index}] is not a replicated spec: {e}"
            ))
        }
    };
    if envelope.v != 1 {
        return ApplyEffect::Failed(format!(
            "mapping update for [{index}] has unsupported envelope v={}",
            envelope.v
        ));
    }
    let idx = match engine.get_index(index) {
        Ok(idx) => idx,
        Err(_) => return ApplyEffect::NotFound,
    };
    let current = idx.schema().await;
    let additions: Vec<FieldConfig> = envelope
        .add_fields
        .into_iter()
        .filter(|field| current.field(&field.name).is_none())
        .collect();
    if let Err(e) = idx.add_fields(additions).await {
        return ApplyEffect::Failed(e.to_string());
    }
    match engine.put_index_mapping(index, envelope.mapping) {
        Ok(()) => ApplyEffect::Applied,
        Err(e) => ApplyEffect::Failed(e.to_string()),
    }
}

async fn apply_settings(engine: &Engine, index: &str, settings_json: &str) -> ApplyEffect {
    let body: Value = match serde_json::from_str(settings_json) {
        Ok(body) => body,
        Err(e) => {
            return ApplyEffect::Failed(format!("settings update for [{index}] is not JSON: {e}"))
        }
    };
    match engine.apply_user_index_settings(index, &body).await {
        Ok(()) => ApplyEffect::Applied,
        Err(e) if is_not_found(&e) => ApplyEffect::NotFound,
        Err(e) => ApplyEffect::Failed(e.to_string()),
    }
}
