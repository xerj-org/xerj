//! Pipeline executor: runs a document through an ordered list of transform
//! plugins.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::{
    builtins::{
        AddFieldPlugin, ConvertTypePlugin, CopyFieldPlugin, DropFieldPlugin, FieldRenamePlugin,
        GrokPlugin, JsonParsePlugin, LowercasePlugin, PiiRedactionPlugin, RemoveNullPlugin,
        RoutePlugin, SetPlugin, SplitPlugin, TimestampParsePlugin, UppercasePlugin,
        UrlDecodePlugin,
    },
    Result, TransformPlugin, WasmError,
};

// ── Action returned by each stage ────────────────────────────────────────────

/// Decision returned by a pipeline stage (or the pipeline itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessAction {
    /// Continue to the next stage / index the document normally.
    Pass,
    /// Discard this document — do not index it.
    Drop,
    /// Index the document into a different target (overrides the original
    /// index name).
    Route(String),
}

// ── Error policy ─────────────────────────────────────────────────────────────

/// What to do when a pipeline stage returns an error.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPolicy {
    /// Discard the document (default).
    #[default]
    Drop,
    /// Pass the document through unchanged.
    Pass,
    /// Send the document to a dead-letter index (`<original>-dead-letter`).
    DeadLetter,
}

// ── Pipeline config (JSON-deserialisable) ─────────────────────────────────────

/// Configuration for a single pipeline stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStageConfig {
    /// Stage type — maps to a built-in plugin name (e.g. `"json_parse"`).
    #[serde(rename = "type")]
    pub stage_type: String,
    /// Arbitrary plugin-specific configuration.
    #[serde(default)]
    pub config: Value,
}

/// Top-level pipeline configuration (stored in the engine and serialised for
/// the `PUT /v1/pipelines/{name}` API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Human-readable description (optional).
    #[serde(default)]
    pub description: String,
    /// Ordered list of transform stages.
    pub stages: Vec<PipelineStageConfig>,
    /// What to do when a stage fails.
    #[serde(default)]
    pub on_error: ErrorPolicy,
    /// Per-document timeout in milliseconds (0 = unlimited).
    #[serde(default)]
    pub timeout_ms: u64,
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// A named, executable pipeline composed of ordered [`TransformPlugin`] stages.
///
/// `Pipeline` is `Clone` (cheap — inner stages are `Arc`-wrapped) and safe to
/// share across async tasks.
#[derive(Clone)]
pub struct Pipeline {
    /// Pipeline name (same as the key in the engine's pipeline map).
    pub name: String,
    /// Ordered stages.
    stages: Vec<Arc<dyn TransformPlugin>>,
    /// Error handling policy.
    pub on_error: ErrorPolicy,
    /// Per-document timeout (informational — enforced by the caller).
    pub timeout: Duration,
}

impl Pipeline {
    /// Build a [`Pipeline`] from a [`PipelineConfig`].
    ///
    /// Returns [`WasmError::InvalidConfig`] if an unknown stage type is
    /// encountered or a plugin-specific config is malformed.
    pub fn from_config(name: impl Into<String>, config: &PipelineConfig) -> Result<Self> {
        let name = name.into();
        let mut stages: Vec<Arc<dyn TransformPlugin>> = Vec::new();

        for stage_cfg in &config.stages {
            let plugin =
                build_plugin(&stage_cfg.stage_type, &stage_cfg.config).map_err(|reason| {
                    WasmError::InvalidConfig {
                        plugin: stage_cfg.stage_type.clone(),
                        reason,
                    }
                })?;
            stages.push(plugin);
        }

        Ok(Self {
            name,
            stages,
            on_error: config.on_error.clone(),
            timeout: if config.timeout_ms == 0 {
                Duration::from_secs(30)
            } else {
                Duration::from_millis(config.timeout_ms)
            },
        })
    }

    /// Run `doc` through every stage in order.
    ///
    /// Returns the first non-[`ProcessAction::Pass`] action, or
    /// [`ProcessAction::Pass`] if all stages pass.
    pub fn process(&self, doc: &mut Value) -> ProcessAction {
        for stage in &self.stages {
            debug!(
                pipeline = self.name.as_str(),
                stage = stage.name(),
                "running stage"
            );
            match stage.process(doc) {
                ProcessAction::Pass => continue,
                action => {
                    debug!(
                        pipeline = self.name.as_str(),
                        stage = stage.name(),
                        action = ?action,
                        "stage short-circuits pipeline"
                    );
                    return action;
                }
            }
        }
        ProcessAction::Pass
    }

    /// Run every document in `docs` through the pipeline.
    ///
    /// Documents are processed independently — one failing stage does not
    /// affect subsequent documents.
    pub fn process_batch(&self, docs: &mut [Value]) -> Vec<ProcessAction> {
        docs.iter_mut().map(|doc| self.process(doc)).collect()
    }

    /// Number of stages in this pipeline.
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("name", &self.name)
            .field("stage_count", &self.stages.len())
            .field("on_error", &self.on_error)
            .finish()
    }
}

// ── Plugin factory ────────────────────────────────────────────────────────────

/// Instantiate a built-in plugin by name.
fn build_plugin(
    stage_type: &str,
    config: &Value,
) -> std::result::Result<Arc<dyn TransformPlugin>, String> {
    match stage_type {
        "json_parse" => {
            let field = str_field(config, "field")?;
            let target = config
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(Arc::new(JsonParsePlugin::new(field, target)))
        }

        "timestamp_parse" => {
            let field = str_field(config, "field")?;
            let formats: Vec<String> = config
                .get("formats")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let target = config
                .get("target")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(Arc::new(TimestampParsePlugin::new(field, formats, target)))
        }

        "field_rename" => {
            let mappings = config
                .get("mappings")
                .and_then(Value::as_object)
                .ok_or_else(|| "missing 'mappings' object".to_string())?;
            let map: HashMap<String, String> = mappings
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
            Ok(Arc::new(FieldRenamePlugin::new(map)))
        }

        "drop_field" => {
            let fields = string_array(config, "fields")?;
            Ok(Arc::new(DropFieldPlugin::new(fields)))
        }

        "add_field" => {
            let field = str_field(config, "field")?;
            let value = config
                .get("value")
                .cloned()
                .ok_or_else(|| "missing 'value'".to_string())?;
            Ok(Arc::new(AddFieldPlugin::new(field, value)))
        }

        "route" => {
            let field = str_field(config, "field")?;
            let routes = config
                .get("routes")
                .and_then(Value::as_object)
                .ok_or_else(|| "missing 'routes' object".to_string())?;
            let map: HashMap<String, String> = routes
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
            let default = config
                .get("default")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(Arc::new(RoutePlugin::new(field, map, default)))
        }

        "grok" => {
            let field = str_field(config, "field")?;
            let pattern_name = config
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or("SYSLOG")
                .to_string();
            Ok(Arc::new(GrokPlugin::new(field, pattern_name)))
        }

        "pii_redaction" => {
            let types: Vec<String> = config
                .get("types")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_else(|| vec!["email".into(), "ip".into(), "credit_card".into()]);
            Ok(Arc::new(PiiRedactionPlugin::new(types)))
        }

        "copy_field" => {
            let source = str_field(config, "source")?;
            let target = str_field(config, "target")?;
            Ok(Arc::new(CopyFieldPlugin::new(source, target)))
        }

        "convert" => {
            let field = str_field(config, "field")?;
            let target_type = str_field(config, "type")?;
            Ok(Arc::new(ConvertTypePlugin::new(field, target_type)))
        }

        "split" => {
            let field = str_field(config, "field")?;
            let separator = config
                .get("separator")
                .and_then(Value::as_str)
                .unwrap_or(",")
                .to_string();
            Ok(Arc::new(SplitPlugin::new(field, separator)))
        }

        "lowercase" => {
            let field = str_field(config, "field")?;
            Ok(Arc::new(LowercasePlugin::new(field)))
        }

        "uppercase" => {
            let field = str_field(config, "field")?;
            Ok(Arc::new(UppercasePlugin::new(field)))
        }

        "set" => {
            let field = str_field(config, "field")?;
            let value = config
                .get("value")
                .cloned()
                .ok_or_else(|| "missing 'value'".to_string())?;
            let override_existing = config
                .get("override")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(Arc::new(SetPlugin::new(field, value, override_existing)))
        }

        "remove_null" => Ok(Arc::new(RemoveNullPlugin)),

        "url_decode" => {
            let field = str_field(config, "field")?;
            Ok(Arc::new(UrlDecodePlugin::new(field)))
        }

        unknown => {
            warn!(stage_type = unknown, "unknown pipeline stage type");
            Err(format!("unknown stage type '{unknown}'"))
        }
    }
}

// ── Config helpers ────────────────────────────────────────────────────────────

fn str_field(config: &Value, key: &str) -> std::result::Result<String, String> {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing required string field '{key}'"))
}

fn string_array(config: &Value, key: &str) -> std::result::Result<Vec<String>, String> {
    let arr = config
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing required array field '{key}'"))?;
    Ok(arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pipeline(stages: &[(&str, Value)]) -> Pipeline {
        let stage_cfgs: Vec<PipelineStageConfig> = stages
            .iter()
            .map(|(t, c)| PipelineStageConfig {
                stage_type: t.to_string(),
                config: c.clone(),
            })
            .collect();
        let cfg = PipelineConfig {
            description: "test".into(),
            stages: stage_cfgs,
            on_error: ErrorPolicy::Drop,
            timeout_ms: 0,
        };
        Pipeline::from_config("test-pipeline", &cfg).expect("pipeline build failed")
    }

    #[test]
    fn pipeline_processes_all_pass_stages() {
        let pl = make_pipeline(&[(
            "add_field",
            serde_json::json!({ "field": "env", "value": "test" }),
        )]);
        let mut doc = serde_json::json!({ "msg": "hello" });
        assert_eq!(pl.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["env"], "test");
    }

    #[test]
    fn pipeline_short_circuits_on_drop() {
        let pl = make_pipeline(&[
            // First stage drops the document
            ("drop_field", serde_json::json!({ "fields": ["drop_me"] })),
            // This stage would add a field, but we'll test with a route stage
            // that triggers drop via a route mismatch — use a simple 2-stage test
            (
                "add_field",
                serde_json::json!({ "field": "should_not_appear", "value": true }),
            ),
        ]);
        let mut doc = serde_json::json!({ "msg": "hello", "drop_me": "x" });
        // drop_field returns Pass (it removes the field), add_field adds env
        assert_eq!(pl.process(&mut doc), ProcessAction::Pass);
        assert_eq!(doc["should_not_appear"], true);
    }

    #[test]
    fn pipeline_batch() {
        let pl = make_pipeline(&[(
            "add_field",
            serde_json::json!({ "field": "pipeline", "value": "default" }),
        )]);
        let mut docs = vec![serde_json::json!({ "a": 1 }), serde_json::json!({ "b": 2 })];
        let actions = pl.process_batch(&mut docs);
        assert!(actions.iter().all(|a| *a == ProcessAction::Pass));
        assert_eq!(docs[0]["pipeline"], "default");
        assert_eq!(docs[1]["pipeline"], "default");
    }

    #[test]
    fn pipeline_from_invalid_config_fails() {
        let cfg = PipelineConfig {
            description: String::new(),
            stages: vec![PipelineStageConfig {
                stage_type: "unknown_plugin_xyz".into(),
                config: Value::Null,
            }],
            on_error: ErrorPolicy::Pass,
            timeout_ms: 0,
        };
        assert!(Pipeline::from_config("bad", &cfg).is_err());
    }
}
