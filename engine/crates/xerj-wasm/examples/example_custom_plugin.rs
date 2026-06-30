//! Example custom transform plugin for xerj pipeline
//!
//! To use: implement the TransformPlugin trait, then register
//! with the pipeline factory.

use serde_json::Value;
use xerj_wasm::{ProcessAction, TransformPlugin};

pub struct MyCustomPlugin {
    threshold: f64,
}

impl TransformPlugin for MyCustomPlugin {
    fn name(&self) -> &str {
        "my_custom_filter"
    }

    fn process(&self, doc: &mut Value) -> ProcessAction {
        // Example: drop documents with response_time > threshold
        if let Some(rt) = doc.get("response_time").and_then(|v| v.as_f64()) {
            if rt > self.threshold {
                return ProcessAction::Drop;
            }
        }
        ProcessAction::Pass
    }
}

fn main() {
    use xerj_wasm::pipeline::{ErrorPolicy, Pipeline, PipelineConfig, PipelineStageConfig};

    // Demonstrate building a pipeline programmatically with the built-in factory.
    let config = PipelineConfig {
        description: "nginx access log pipeline".into(),
        stages: vec![
            PipelineStageConfig {
                stage_type: "grok".into(),
                config: serde_json::json!({
                    "pattern": "NGINX_COMBINED",
                    "field": "message"
                }),
            },
            PipelineStageConfig {
                stage_type: "timestamp_parse".into(),
                config: serde_json::json!({
                    "field": "time_local"
                }),
            },
            PipelineStageConfig {
                stage_type: "add_field".into(),
                config: serde_json::json!({
                    "field": "pipeline",
                    "value": "nginx-access"
                }),
            },
            PipelineStageConfig {
                stage_type: "pii_redaction".into(),
                config: serde_json::json!({
                    "types": ["ip", "email"]
                }),
            },
        ],
        on_error: ErrorPolicy::Drop,
        timeout_ms: 5000,
    };

    let pipeline = Pipeline::from_config("nginx-access", &config).expect("pipeline build failed");

    let mut doc = serde_json::json!({
        "message": r#"192.168.1.1 - alice [10/Apr/2026:12:00:00 +0000] "GET /index.html HTTP/1.1" 200 1234 "https://example.com" "Mozilla/5.0""#
    });

    let action = pipeline.process(&mut doc);
    println!("Action: {action:?}");
    println!("Transformed doc: {}", serde_json::to_string_pretty(&doc).unwrap());
}
