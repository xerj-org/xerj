//! Integration tests for the xerj-wasm pipeline system.

use serde_json::json;
use xerj_wasm::pipeline::{ErrorPolicy, Pipeline, PipelineConfig, PipelineStageConfig, ProcessAction};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn make_pipeline(stages: &[(&str, serde_json::Value)]) -> Pipeline {
    let stage_cfgs: Vec<PipelineStageConfig> = stages
        .iter()
        .map(|(t, c)| PipelineStageConfig {
            stage_type: t.to_string(),
            config: c.clone(),
        })
        .collect();
    let cfg = PipelineConfig {
        description: "integration test".into(),
        stages: stage_cfgs,
        on_error: ErrorPolicy::Drop,
        timeout_ms: 0,
    };
    Pipeline::from_config("test-pipeline", &cfg).expect("pipeline build failed")
}

// ─────────────────────────────────────────────────────────────────────────────
// test_full_pipeline_nginx_log
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_full_pipeline_nginx_log() {
    let pl = make_pipeline(&[
        (
            "grok",
            json!({ "pattern": "NGINX_COMBINED", "field": "message" }),
        ),
        (
            "timestamp_parse",
            json!({ "field": "time_local" }),
        ),
        (
            "pii_redaction",
            json!({ "types": ["ip", "email"] }),
        ),
        (
            "add_field",
            json!({ "field": "pipeline", "value": "nginx-access" }),
        ),
    ]);

    let nginx_line = r#"192.168.1.100 - alice [10/Apr/2026:12:00:00 +0000] "GET /index.html HTTP/1.1" 200 1234 "https://example.com" "Mozilla/5.0""#;
    let mut doc = json!({ "message": nginx_line });

    let action = pl.process(&mut doc);
    assert_eq!(action, ProcessAction::Pass, "pipeline should pass document");

    // Grok fields extracted
    assert_eq!(doc["method"], "GET", "method field not extracted");
    assert_eq!(doc["status"], "200", "status field not extracted");
    assert_eq!(doc["request_uri"], "/index.html", "request_uri not extracted");

    // Timestamp parsed from time_local
    assert!(
        doc["@timestamp"].is_string(),
        "@timestamp not set: {:?}",
        doc["@timestamp"]
    );
    let ts = doc["@timestamp"].as_str().unwrap();
    assert!(ts.contains("2026-04-10"), "timestamp not parsed correctly: {ts}");

    // PII redacted — remote_addr should be masked
    let remote = doc["remote_addr"].as_str().unwrap_or("");
    assert!(
        remote.contains("[REDACTED_IP]"),
        "IP not redacted in remote_addr: {remote}"
    );

    // add_field applied
    assert_eq!(doc["pipeline"], "nginx-access", "pipeline field not added");
}

// ─────────────────────────────────────────────────────────────────────────────
// test_pipeline_with_routing
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_pipeline_with_routing() {
    let pl = make_pipeline(&[(
        "route",
        json!({
            "field": "level",
            "routes": {
                "ERROR": "logs-errors",
                "WARN":  "logs-warnings"
            },
            "default": "logs-misc"
        }),
    )]);

    // ERROR doc routes to "logs-errors"
    let mut error_doc = json!({ "level": "ERROR", "msg": "boom" });
    assert_eq!(
        pl.process(&mut error_doc),
        ProcessAction::Route("logs-errors".into()),
        "ERROR docs should route to logs-errors"
    );

    // INFO doc falls through to the default
    let mut info_doc = json!({ "level": "INFO", "msg": "ok" });
    assert_eq!(
        pl.process(&mut info_doc),
        ProcessAction::Route("logs-misc".into()),
        "INFO docs should route to logs-misc"
    );

    // WARN doc routes to "logs-warnings"
    let mut warn_doc = json!({ "level": "WARN", "msg": "careful" });
    assert_eq!(
        pl.process(&mut warn_doc),
        ProcessAction::Route("logs-warnings".into()),
        "WARN docs should route to logs-warnings"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// test_pipeline_chaining_all_plugins
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_pipeline_chaining_all_plugins() {
    let pl = make_pipeline(&[
        // json_parse: parse a JSON payload field
        ("json_parse", json!({ "field": "payload", "target": "parsed" })),
        // add_field: add a static tag
        ("add_field", json!({ "field": "env", "value": "production" })),
        // copy_field: copy env → environment
        ("copy_field", json!({ "source": "env", "target": "environment" })),
        // set: only set if missing
        ("set", json!({ "field": "region", "value": "us-east-1", "override": false })),
        // set: override always
        ("set", json!({ "field": "env", "value": "prod", "override": true })),
        // convert: status code to integer
        ("convert", json!({ "field": "status_code", "type": "integer" })),
        // split: split tags
        ("split", json!({ "field": "tags", "separator": "," })),
        // lowercase: lowercase method
        ("lowercase", json!({ "field": "method" })),
        // uppercase: uppercase level
        ("uppercase", json!({ "field": "level" })),
        // url_decode: decode path
        ("url_decode", json!({ "field": "path" })),
        // remove_null: remove null fields
        ("remove_null", json!({})),
        // drop_field: remove payload
        ("drop_field", json!({ "fields": ["null_field"] })),
        // pii_redaction: redact email
        ("pii_redaction", json!({ "types": ["email"] })),
        // field_rename: rename parsed to data
        ("field_rename", json!({ "mappings": { "parsed": "data" } })),
        // timestamp_parse
        ("timestamp_parse", json!({ "field": "ts" })),
        // grok on log line
        ("grok", json!({ "pattern": "SYSLOG", "field": "syslog_line" })),
    ]);

    let mut doc = json!({
        "payload": r#"{"key": "value"}"#,
        "status_code": "404",
        "tags": "web,api,v2",
        "method": "GET",
        "level": "warn",
        "path": "/search%3Fq%3Dhello+world",
        "null_field": null,
        "contact": "email me at user@example.com",
        "ts": "2026-04-10T12:00:00Z",
        "syslog_line": "Apr 10 12:00:00 myhost myapp[999]: test message",
    });

    let action = pl.process(&mut doc);
    assert_eq!(action, ProcessAction::Pass, "all-plugin pipeline should pass");

    // json_parse + field_rename
    assert_eq!(doc["data"]["key"], "value", "json_parse + field_rename failed");
    // add_field
    assert_eq!(doc["environment"], "production", "copy_field failed");
    // set override=false keeps existing
    assert_eq!(doc["region"], "us-east-1", "set failed");
    // set override=true replaces
    assert_eq!(doc["env"], "prod", "set override failed");
    // convert
    assert_eq!(doc["status_code"], 404, "convert to integer failed");
    // split
    let tags = doc["tags"].as_array().expect("split should produce array");
    assert_eq!(tags.len(), 3, "split produced wrong count: {tags:?}");
    // lowercase
    assert_eq!(doc["method"], "get", "lowercase failed");
    // uppercase
    assert_eq!(doc["level"], "WARN", "uppercase failed");
    // url_decode
    assert_eq!(doc["path"], "/search?q=hello world", "url_decode failed");
    // remove_null — null_field removed
    assert!(doc.get("null_field").is_none(), "remove_null failed");
    // pii_redaction
    let contact = doc["contact"].as_str().unwrap();
    assert!(contact.contains("[REDACTED_EMAIL]"), "pii_redaction failed: {contact}");
    // timestamp_parse
    assert!(doc["@timestamp"].is_string(), "@timestamp not set");
    // grok
    assert_eq!(doc["hostname"], "myhost", "grok syslog failed");
}

// ─────────────────────────────────────────────────────────────────────────────
// test_pipeline_error_handling
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_pipeline_error_handling() {
    // Building a pipeline with an unknown plugin type should fail.
    let cfg = PipelineConfig {
        description: String::new(),
        stages: vec![PipelineStageConfig {
            stage_type: "nonexistent_plugin_xyz".into(),
            config: json!({}),
        }],
        on_error: ErrorPolicy::Drop,
        timeout_ms: 0,
    };
    let result = Pipeline::from_config("bad", &cfg);
    assert!(result.is_err(), "building pipeline with unknown plugin should fail");

    // With on_error = Pass, a missing-field stage is a no-op (passes through).
    let pl = make_pipeline(&[
        // drop_field with a field that doesn't exist — should still Pass
        ("drop_field", json!({ "fields": ["nonexistent"] })),
        // add_field always works
        ("add_field", json!({ "field": "survived", "value": true })),
    ]);
    let mut doc = json!({ "msg": "hello" });
    assert_eq!(pl.process(&mut doc), ProcessAction::Pass);
    assert_eq!(doc["survived"], true, "document should survive after no-op stage");
}

// ─────────────────────────────────────────────────────────────────────────────
// test_simulate_pipeline
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_simulate_pipeline() {
    // Simulate: build pipeline, run it in memory, verify results without
    // touching any index.
    let pl = make_pipeline(&[
        ("add_field", json!({ "field": "env", "value": "staging" })),
        ("lowercase", json!({ "field": "level" })),
        ("pii_redaction", json!({ "types": ["email"] })),
    ]);

    let mut docs = vec![
        json!({ "level": "ERROR", "msg": "user@example.com failed" }),
        json!({ "level": "INFO",  "msg": "all good" }),
    ];

    let actions = pl.process_batch(&mut docs);

    // Both docs pass
    assert!(
        actions.iter().all(|a| *a == ProcessAction::Pass),
        "all docs should pass"
    );

    // env field added to both
    assert_eq!(docs[0]["env"], "staging");
    assert_eq!(docs[1]["env"], "staging");

    // level lowercased
    assert_eq!(docs[0]["level"], "error");
    assert_eq!(docs[1]["level"], "info");

    // PII redacted
    let msg0 = docs[0]["msg"].as_str().unwrap();
    assert!(
        msg0.contains("[REDACTED_EMAIL]"),
        "email not redacted: {msg0}"
    );
    // No email in second doc — unchanged
    assert_eq!(docs[1]["msg"], "all good");
}

// ─────────────────────────────────────────────────────────────────────────────
// New plugin unit-level integration tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_copy_field() {
    let pl = make_pipeline(&[("copy_field", json!({ "source": "msg", "target": "original" }))]);
    let mut doc = json!({ "msg": "hello" });
    assert_eq!(pl.process(&mut doc), ProcessAction::Pass);
    assert_eq!(doc["msg"], "hello", "source should be preserved");
    assert_eq!(doc["original"], "hello", "target should be set");
}

#[test]
fn test_convert_types() {
    let pl = make_pipeline(&[
        ("convert", json!({ "field": "n", "type": "integer" })),
        ("convert", json!({ "field": "f", "type": "float" })),
        ("convert", json!({ "field": "s", "type": "string" })),
        ("convert", json!({ "field": "b", "type": "boolean" })),
    ]);
    let mut doc = json!({ "n": "42", "f": "3.14", "s": 99, "b": "true" });
    assert_eq!(pl.process(&mut doc), ProcessAction::Pass);
    assert_eq!(doc["n"], 42, "integer convert failed");
    assert!((doc["f"].as_f64().unwrap() - 3.14).abs() < 0.001, "float convert failed");
    assert_eq!(doc["s"], "99", "string convert failed");
    assert_eq!(doc["b"], true, "boolean convert failed");
}

#[test]
fn test_split_and_case_plugins() {
    let pl = make_pipeline(&[
        ("split", json!({ "field": "tags", "separator": "," })),
        ("lowercase", json!({ "field": "method" })),
        ("uppercase", json!({ "field": "status" })),
    ]);
    let mut doc = json!({ "tags": "a,b,c", "method": "GET", "status": "ok" });
    assert_eq!(pl.process(&mut doc), ProcessAction::Pass);
    assert_eq!(doc["tags"].as_array().unwrap().len(), 3);
    assert_eq!(doc["method"], "get");
    assert_eq!(doc["status"], "OK");
}

#[test]
fn test_set_plugin_no_override() {
    let pl = make_pipeline(&[("set", json!({ "field": "env", "value": "production", "override": false }))]);

    // Field absent → should be set
    let mut doc = json!({ "msg": "hi" });
    pl.process(&mut doc);
    assert_eq!(doc["env"], "production");

    // Field present → should NOT be overwritten
    let mut doc2 = json!({ "env": "staging" });
    pl.process(&mut doc2);
    assert_eq!(doc2["env"], "staging", "set with override=false should not overwrite");
}

#[test]
fn test_set_plugin_with_override() {
    let pl = make_pipeline(&[("set", json!({ "field": "env", "value": "production", "override": true }))]);
    let mut doc = json!({ "env": "staging" });
    pl.process(&mut doc);
    assert_eq!(doc["env"], "production", "set with override=true should overwrite");
}

#[test]
fn test_remove_null_plugin() {
    let pl = make_pipeline(&[("remove_null", json!({}))]);
    let mut doc = json!({ "a": 1, "b": null, "c": "hello", "d": "" });
    pl.process(&mut doc);
    assert!(doc.get("b").is_none(), "null field should be removed");
    assert!(doc.get("d").is_none(), "empty string field should be removed");
    assert_eq!(doc["a"], 1, "non-null field should remain");
    assert_eq!(doc["c"], "hello", "non-null field should remain");
}

#[test]
fn test_url_decode_plugin() {
    let pl = make_pipeline(&[("url_decode", json!({ "field": "path" }))]);
    let mut doc = json!({ "path": "/search%3Fq%3Dhello+world%26lang%3Den" });
    pl.process(&mut doc);
    assert_eq!(doc["path"], "/search?q=hello world&lang=en", "url_decode failed");
}
