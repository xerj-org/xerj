//! Fail fast when the node's data volume is already at the flood-stage
//! watermark (#2077).
//!
//! The watermark lives on the **node**: `limits.disk_flood_stage_percent`
//! (default 95) measured against the filesystem that holds the node's data
//! directory. Autoindex and the node are not necessarily the same machine, so
//! this probe reads `GET /_cat/allocation?format=json` — the endpoint that
//! already reports that volume's used percent and free bytes — plus the
//! effective threshold and the latched write block the node adds to that
//! JSON. It does not `stat` the client's disk.
//!
//! A node that does not send `disk.flood_stage` (an older XERJ, Elasticsearch,
//! or the HTTP doubles in this crate's tests) is not guessed at. Those runs
//! behave as they did before: the write itself still reports the block if one
//! engages later.
//!
//! Memory writes (#2284) hit the same watermark later, as a nested
//! `cluster_block_exception`, because `/_memory` stringifies the document
//! error. [`explain_memory_flood`] turns that body into the same kind of
//! sentence this module already prints for autoindex: free space, the
//! threshold, and the config override — plus the data-dir path and the
//! `--data-dir` move, which is the recovery the memory field report needed.

use serde_json::Value;

use crate::esclient::Es;

const ALLOCATION_PATH: &str = "/_cat/allocation?format=json";

/// One row of `GET /_cat/allocation?format=json`.
///
/// The budget gate reads free space and the flood threshold from this same
/// response. It does not issue a second probe, and it does not fill in a
/// threshold the node left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Volume {
    pub total: Option<u64>,
    pub avail: Option<u64>,
    pub used_percent: Option<u64>,
    pub flood_stage: Option<u8>,
    pub threshold_enabled: bool,
    pub flood_blocked: bool,
}

/// The one allocation read: the parsed volume, and the refusal when the
/// node says writes will fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Probe {
    pub volume: Option<Volume>,
    pub refusal: Option<String>,
}

/// `None` fields when the node cannot be asked. A stats blip must not abort
/// a run whose disk is fine, and the write path still refuses if the block
/// is actually engaged.
pub(crate) fn probe(es: &Es) -> Probe {
    let Ok((status, body)) = es.request_json("GET", ALLOCATION_PATH, None) else {
        return Probe {
            volume: None,
            refusal: None,
        };
    };
    if !(200..300).contains(&status) {
        return Probe {
            volume: None,
            refusal: None,
        };
    }
    Probe {
        volume: volume_from_allocation(&body),
        refusal: refusal_from_allocation(&body),
    }
}

/// Parsed disk columns from one allocation body. `None` when the body is
/// not an allocation row (the HTTP doubles answer `{"acknowledged": true}`).
pub(crate) fn volume_from_allocation(body: &Value) -> Option<Volume> {
    let row = body.as_array()?.first()?;
    let flood_stage = row
        .get("disk.flood_stage")
        .and_then(parse_u64)
        .and_then(|n| u8::try_from(n.min(100)).ok());
    Some(Volume {
        total: row.get("disk.total").and_then(parse_bytes),
        avail: row.get("disk.avail").and_then(parse_bytes),
        used_percent: row.get("disk.percent").and_then(parse_u64),
        flood_stage,
        threshold_enabled: row
            .get("disk.threshold_enabled")
            .and_then(parse_bool)
            .unwrap_or(true),
        flood_blocked: row
            .get("disk.flood_blocked")
            .and_then(parse_bool)
            .unwrap_or(false),
    })
}

/// Decide from one `/_cat/allocation?format=json` body.
///
/// `Some` is the error text for a volume already at or over the watermark,
/// or for a write block the node has not released yet (the 1% hysteresis
/// below the threshold still refuses writes). `None` means index.
pub(crate) fn refusal_from_allocation(body: &Value) -> Option<String> {
    let row = body.as_array()?.first()?;
    // Absent threshold: this response is not the XERJ flood-stage report.
    // Do not assume 95. The node's config may be 99 (the #2077 workaround)
    // or 0 (watermark off), and a hard-coded default would refuse both.
    let flood_raw = parse_u64(row.get("disk.flood_stage")?)?;
    let flood = u8::try_from(flood_raw.min(100)).ok()?;
    let enabled = row
        .get("disk.threshold_enabled")
        .and_then(parse_bool)
        .unwrap_or(true);
    let blocked = row
        .get("disk.flood_blocked")
        .and_then(parse_bool)
        .unwrap_or(false);
    let used = row.get("disk.percent").and_then(parse_u64);
    let free = row.get("disk.avail").and_then(parse_bytes);

    let over = enabled && flood > 0 && used.is_some_and(|pct| pct >= u64::from(flood));
    if !blocked && !over {
        return None;
    }
    Some(refusal_message(free, used, flood, blocked && !over))
}

fn refusal_message(free: Option<u64>, used: Option<u64>, flood: u8, hysteresis: bool) -> String {
    let free = match free {
        Some(bytes) => format_bytes(bytes),
        None => "unknown".to_string(),
    };
    let used = match used {
        Some(pct) => format!("{pct}% used"),
        None => "used percent unknown".to_string(),
    };
    let why = if hysteresis {
        "the disk flood-stage write block is still engaged"
    } else {
        "its data volume is already at or over the disk flood-stage watermark"
    };
    format!(
        "the node will refuse every write: {why} (free {free}, {used}, threshold {flood}%). \
         Set limits.disk_flood_stage_percent above the used percent in the node config and \
         restart (0 disables the watermark), or raise it live with \
         PUT /_cluster/settings {{\"persistent\":{{\"cluster.routing.allocation.disk.watermark.flood_stage\":\"<percent>\"}}}}. \
         No files were walked and nothing was written."
    )
}

/// Same scale as the engine's flood-stage log line (`2.0gb`, `512b`).
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1}gb", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}mb", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}kb", bytes as f64 / KB as f64)
    } else {
        format!("{bytes}b")
    }
}

fn parse_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().trim_end_matches('%').parse().ok(),
        _ => None,
    }
}

fn parse_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// How a memory-write error body relates to the flood-stage block.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MemoryFloodClass {
    /// Some other failure. The caller keeps its existing error text.
    Other,
    /// The node already replaced the nested exception with the plain sentence.
    Plain(String),
    /// Raw `cluster_block_exception` for the flood-stage block. Percents are
    /// taken from the reason when it names them (`disk usage 96% exceeded
    /// flood-stage watermark [95%]`).
    Raw {
        used_pct: Option<u64>,
        flood_pct: Option<u8>,
    },
}

/// Disk facts a memory-write client can attach to the plain sentence.
///
/// The server fills these from `statvfs` on its data dir. A client that only
/// has `GET /_cat/allocation?format=json` fills them with
/// [`disk_facts_from_allocation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFacts {
    pub data_dir: String,
    pub free_bytes: Option<u64>,
    pub used_pct: Option<u64>,
    pub flood_pct: Option<u8>,
}

/// `Some` when `body` is a flood-stage write refusal.
///
/// `Plain` is returned unchanged (the server already explained it). A raw
/// nested `cluster_block_exception` is rewritten with `facts`: error-text
/// percents win over `facts`, because those are the numbers the block used.
/// `None` means this is not that block — an explicit `index.blocks.write`,
/// a mapping error, a validation 400 — and the caller must not claim the
/// disk is full.
pub fn explain_memory_flood(body: &Value, facts: Option<&DiskFacts>) -> Option<String> {
    match classify_memory_flood(body) {
        MemoryFloodClass::Other => None,
        MemoryFloodClass::Plain(text) => Some(text),
        MemoryFloodClass::Raw {
            used_pct,
            flood_pct,
        } => {
            let facts = facts.cloned().unwrap_or(DiskFacts {
                data_dir: String::new(),
                free_bytes: None,
                used_pct: None,
                flood_pct: None,
            });
            Some(memory_flood_stage_message(
                &facts.data_dir,
                facts.free_bytes,
                used_pct.or(facts.used_pct),
                flood_pct.or(facts.flood_pct),
            ))
        }
    }
}

/// The sentence memory writes show instead of a nested `cluster_block_exception`.
///
/// Same free-space scale and the same `limits.disk_flood_stage_percent` /
/// `PUT /_cluster/settings` recovery as the autoindex refusal (#2077, #823).
/// `data_dir` empty is printed as unknown rather than omitted: the field
/// report's fix was moving that directory to another drive.
pub fn memory_flood_stage_message(
    data_dir: &str,
    free_bytes: Option<u64>,
    used_pct: Option<u64>,
    flood_pct: Option<u8>,
) -> String {
    let free = match free_bytes {
        Some(bytes) => format_bytes(bytes),
        None => "unknown".to_string(),
    };
    let used = match used_pct {
        Some(pct) => format!("{pct}% used"),
        None => "used percent unknown".to_string(),
    };
    let flood = match flood_pct {
        Some(pct) => format!("threshold {pct}%"),
        None => "threshold unknown".to_string(),
    };
    let dir = if data_dir.is_empty() {
        "unknown"
    } else {
        data_dir
    };
    // Hysteresis (#823): the block stays latched for 1% under the threshold.
    // The disk is still the reason; say the block has not released yet.
    let held = match (used_pct, flood_pct) {
        (Some(used), Some(flood)) if used < u64::from(flood) => {
            " The disk flood-stage write block is still engaged."
        }
        _ => "",
    };
    format!(
        "the disk holding the data dir is nearly full, so this memory write was refused \
         (free {free}, {used}, {flood}, data dir {dir}).{held} \
         Free space on that drive, or restart the node with --data-dir on another drive. \
         Set limits.disk_flood_stage_percent above the used percent in the node config and \
         restart (0 disables the watermark), or raise it live with \
         PUT /_cluster/settings {{\"persistent\":{{\"cluster.routing.allocation.disk.watermark.flood_stage\":\"<percent>\"}}}}."
    )
}

/// `path.data`, `disk.avail`, `disk.percent`, and `disk.flood_stage` from one
/// allocation body. Missing columns stay empty; this does not invent a
/// threshold the node left out.
pub fn disk_facts_from_allocation(body: &Value) -> DiskFacts {
    let row = body.as_array().and_then(|rows| rows.first());
    let data_dir = row
        .and_then(|row| row.get("path.data"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let volume = volume_from_allocation(body);
    DiskFacts {
        data_dir,
        free_bytes: volume.and_then(|v| v.avail),
        used_pct: volume.and_then(|v| v.used_percent),
        flood_pct: volume.and_then(|v| v.flood_stage),
    }
}

fn classify_memory_flood(body: &Value) -> MemoryFloodClass {
    if let Some(reason) = body.pointer("/error/reason").and_then(Value::as_str) {
        if reason.contains("the disk holding the data dir is nearly full") {
            return MemoryFloodClass::Plain(reason.to_string());
        }
    }
    // `Value`'s JSON keeps the words even when `/_memory` has stringified the
    // inner `cluster_block_exception` into `error.reason`.
    let flat = body.to_string();
    if flat.contains("cluster_block_exception")
        && flat.contains("read_only_allow_delete")
        && (flat.contains("flood-stage") || flat.contains("flood_stage"))
    {
        let (used_pct, flood_pct) = percents_from_flood_text(&flat);
        return MemoryFloodClass::Raw {
            used_pct,
            flood_pct,
        };
    }
    MemoryFloodClass::Other
}

/// `disk usage 96% exceeded flood-stage watermark [95%]` — the governor's
/// block reason. Absent on a block that only names `read_only_allow_delete`.
fn percents_from_flood_text(text: &str) -> (Option<u64>, Option<u8>) {
    let used = text.split("disk usage ").nth(1).and_then(|rest| {
        let num = rest.split('%').next()?.trim();
        num.parse().ok()
    });
    let flood = text
        .split("flood-stage watermark [")
        .nth(1)
        .and_then(|rest| {
            let num = rest.split(['%', ']']).next()?.trim();
            let n: u64 = num.parse().ok()?;
            u8::try_from(n.min(100)).ok()
        });
    (used, flood)
}

/// `/_cat/allocation` emits free space as a raw byte count with a `b` suffix
/// (`"2147483648b"`). Anything else is left unknown rather than guessed.
fn parse_bytes(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => {
            let s = s.trim();
            let s = s
                .strip_suffix('b')
                .or_else(|| s.strip_suffix('B'))
                .unwrap_or(s);
            if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
                s.parse().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(percent: &str, avail: &str, flood: &str, enabled: &str, blocked: &str) -> Value {
        json!([{
            "disk.percent": percent,
            "disk.avail": avail,
            "disk.flood_stage": flood,
            "disk.threshold_enabled": enabled,
            "disk.flood_blocked": blocked,
        }])
    }

    #[test]
    fn at_or_over_the_watermark_refuses_with_free_space_threshold_and_override() {
        let message = refusal_from_allocation(&row("98", "2147483648b", "95", "true", "false"))
            .expect("98% against a 95% watermark must refuse");
        assert!(message.contains("free 2.0gb"), "{message}");
        assert!(message.contains("98% used"), "{message}");
        assert!(message.contains("threshold 95%"), "{message}");
        assert!(
            message.contains("limits.disk_flood_stage_percent"),
            "{message}"
        );
        assert!(message.contains("No files were walked"), "{message}");
        assert!(
            message.contains("at or over the disk flood-stage watermark"),
            "{message}"
        );

        // The watermark is inclusive: used == threshold is already blocked
        // (`disk_used_pct >= flood_pct` in the governor).
        assert!(refusal_from_allocation(&row("95", "1b", "95", "true", "false")).is_some());
    }

    #[test]
    fn a_healthy_volume_and_a_raised_or_disabled_watermark_do_not_refuse() {
        assert!(refusal_from_allocation(&row("40", "1000b", "95", "true", "false")).is_none());
        // One percent under the threshold, block not latched.
        assert!(refusal_from_allocation(&row("94", "1000b", "95", "true", "false")).is_none());
        // The field-report workaround: config raised to 99 while the volume
        // sits at 98. Writes are admitted; autoindex must not refuse.
        assert!(refusal_from_allocation(&row("98", "1000b", "99", "true", "false")).is_none());
        // `limits.disk_flood_stage_percent = 0` turns the watermark off.
        assert!(refusal_from_allocation(&row("98", "1000b", "0", "true", "false")).is_none());
        // `cluster.routing.allocation.disk.threshold_enabled: false`.
        assert!(refusal_from_allocation(&row("98", "1000b", "95", "false", "false")).is_none());
    }

    #[test]
    fn hysteresis_still_refuses_and_a_silent_node_does_not() {
        let message = refusal_from_allocation(&row("94", "4096b", "95", "true", "true"))
            .expect("a latched block under the threshold still refuses writes");
        assert!(
            message.contains("write block is still engaged"),
            "{message}"
        );
        assert!(message.contains("free 4.0kb"), "{message}");
        assert!(message.contains("threshold 95%"), "{message}");

        // Older XERJ / Elasticsearch / the in-crate HTTP doubles: allocation
        // JSON without the threshold is not a reason to stop.
        assert!(refusal_from_allocation(&json!([{
            "disk.percent": "99",
            "disk.avail": "1b"
        }]))
        .is_none());
        assert!(refusal_from_allocation(&json!({"acknowledged": true})).is_none());
        assert!(refusal_from_allocation(&json!([])).is_none());
    }

    #[test]
    fn the_same_row_carries_free_and_total_bytes_for_the_budget() {
        let body = json!([{
            "disk.percent": "40",
            "disk.avail": "1000b",
            "disk.total": "10000b",
            "disk.flood_stage": "95",
            "disk.threshold_enabled": "true",
            "disk.flood_blocked": "false",
        }]);
        let volume = volume_from_allocation(&body).expect("an allocation row");
        assert_eq!(volume.avail, Some(1000));
        assert_eq!(volume.total, Some(10000));
        assert_eq!(volume.used_percent, Some(40));
        assert_eq!(volume.flood_stage, Some(95));
        assert!(volume_from_allocation(&json!({"acknowledged": true})).is_none());
    }

    fn nested_flood_body() -> Value {
        // The shape `/_memory` used to return: the document error JSON
        // stringified into `error.reason`, so the agent saw only
        // `cluster_block_exception`.
        let inner = json!({
            "error": {
                "root_cause": [{
                    "type": "cluster_block_exception",
                    "reason": "index [.xerj-memory-notes] is blocked for read_only_allow_delete (disk usage 96% exceeded flood-stage watermark [95%]) operations"
                }],
                "type": "cluster_block_exception",
                "reason": "index [.xerj-memory-notes] is blocked for read_only_allow_delete (disk usage 96% exceeded flood-stage watermark [95%]) operations"
            },
            "status": 429
        });
        json!({
            "error": {
                "type": "memory_error",
                "reason": format!("failed to store memory: {inner}")
            },
            "status": 429
        })
    }

    #[test]
    fn a_nested_flood_stage_block_becomes_the_plain_disk_sentence() {
        let facts = DiskFacts {
            data_dir: "D:\\xerj-data".to_string(),
            free_bytes: Some(2 * 1024 * 1024 * 1024),
            used_pct: None,
            flood_pct: None,
        };
        let message = explain_memory_flood(&nested_flood_body(), Some(&facts))
            .expect("the nested cluster_block_exception is the flood-stage block");
        assert!(
            message.contains("the disk holding the data dir is nearly full"),
            "{message}"
        );
        assert!(message.contains("free 2.0gb"), "{message}");
        assert!(message.contains("96% used"), "{message}");
        assert!(message.contains("threshold 95%"), "{message}");
        assert!(message.contains("data dir D:\\xerj-data"), "{message}");
        assert!(
            message.contains("restart the node with --data-dir on another drive"),
            "{message}"
        );
        assert!(
            message.contains("limits.disk_flood_stage_percent"),
            "{message}"
        );
        assert!(
            message.contains("cluster.routing.allocation.disk.watermark.flood_stage"),
            "{message}"
        );
        assert!(!message.contains("cluster_block_exception"), "{message}");

        // The sentence, once written, is not composed again.
        let already = json!({
            "error": { "type": "memory_error", "reason": message }
        });
        assert_eq!(
            explain_memory_flood(&already, None).as_deref(),
            Some(already["error"]["reason"].as_str().unwrap())
        );
    }

    #[test]
    fn an_explicit_write_block_is_not_called_a_full_disk() {
        let write = json!({
            "error": {
                "type": "cluster_block_exception",
                "reason": "index [notes] is blocked for write operations"
            },
            "status": 403
        });
        assert!(explain_memory_flood(&write, None).is_none());

        // `read_only_allow_delete` set by the block API, with no flood-stage
        // wording, is that block — not a guess that the disk is full.
        let manual = json!({
            "error": {
                "type": "cluster_block_exception",
                "reason": "index [notes] is blocked for read_only_allow_delete operations"
            },
            "status": 429
        });
        assert!(explain_memory_flood(&manual, None).is_none());
    }

    #[test]
    fn allocation_supplies_the_path_and_free_space_the_error_omits() {
        let body = json!({
            "error": {
                "type": "cluster_block_exception",
                "reason": "index [n] is blocked for read_only_allow_delete (disk usage 96% exceeded flood-stage watermark [95%]) operations"
            }
        });
        let facts = disk_facts_from_allocation(&json!([{
            "disk.percent": "96",
            "disk.avail": "2147483648b",
            "disk.flood_stage": "95",
            "path.data": "/var/lib/xerj"
        }]));
        assert_eq!(facts.data_dir, "/var/lib/xerj");
        assert_eq!(facts.free_bytes, Some(2 * 1024 * 1024 * 1024));
        let message = explain_memory_flood(&body, Some(&facts)).unwrap();
        assert!(message.contains("data dir /var/lib/xerj"), "{message}");
        assert!(message.contains("free 2.0gb"), "{message}");

        // No allocation and no percents in a flood reason still explains,
        // with the unknowns named rather than invented.
        let bare = json!({
            "error": {
                "type": "cluster_block_exception",
                "reason": "read_only_allow_delete flood-stage"
            }
        });
        let message = explain_memory_flood(&bare, None).unwrap();
        assert!(message.contains("free unknown"), "{message}");
        assert!(message.contains("data dir unknown"), "{message}");
        assert!(message.contains("--data-dir"), "{message}");
    }

    #[test]
    fn a_block_still_engaged_under_the_threshold_says_so() {
        let body = json!({
            "error": {
                "type": "cluster_block_exception",
                "reason": "index [n] is blocked for read_only_allow_delete (disk usage 94% exceeded flood-stage watermark [95%]) operations"
            }
        });
        let message = explain_memory_flood(&body, None).unwrap();
        assert!(
            message.contains("The disk flood-stage write block is still engaged."),
            "{message}"
        );
        assert!(
            message.contains("the disk holding the data dir is nearly full"),
            "{message}"
        );
    }

    /// The probe runs before the walk: a flooded node refuses with the
    /// override text and never opens the folder, and a healthy node is not
    /// refused (the walk then reports that the root is a file).
    #[test]
    fn autoindex_refuses_before_walk_only_when_the_node_reports_flood_stage() {
        let flooded = Node::start(true);
        let healthy = Node::start(false);
        let root_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path().join("not-a-folder");
        std::fs::write(&root, "bytes the walk must not have to read\n").unwrap();

        let flooded_state = tempfile::tempdir().unwrap();
        let error = crate::run_index_report(index_cfg(&root, flooded_state.path(), &flooded.url))
            .expect_err("a flooded data volume must fail the run");
        let message = format!("{error:#}");
        assert!(
            message.contains("limits.disk_flood_stage_percent"),
            "{message}"
        );
        assert!(message.contains("free 2.0gb"), "{message}");
        assert!(message.contains("threshold 95%"), "{message}");
        assert!(
            !message.contains("is not a directory"),
            "the walk must not have started: {message}"
        );
        assert!(
            !flooded_state.path().join(".autoindex.lock").exists(),
            "refusing before the walk must not open the state directory"
        );
        assert_eq!(
            flooded.requests(),
            vec![
                ("GET".to_string(), "/".to_string()),
                (
                    "GET".to_string(),
                    "/_cat/allocation?format=json".to_string(),
                ),
            ]
        );

        let healthy_state = tempfile::tempdir().unwrap();
        let error = crate::run_index_report(index_cfg(&root, healthy_state.path(), &healthy.url))
            .expect_err("a file root still cannot be walked");
        let message = format!("{error:#}");
        assert!(
            message.contains("is not a directory"),
            "a healthy volume must reach the walk: {message}"
        );
        assert!(
            !message.contains("limits.disk_flood_stage_percent"),
            "{message}"
        );

        // A dry run writes nothing: a flooded node does not refuse it.
        let dry_state = tempfile::tempdir().unwrap();
        let mut dry = index_cfg(&root, dry_state.path(), &flooded.url);
        dry.dry_run = true;
        if let Err(error) = crate::run_index_report(dry) {
            let message = format!("{error:#}");
            assert!(
                !message.contains("limits.disk_flood_stage_percent"),
                "a dry run must not be refused for disk: {message}"
            );
        }
    }

    fn index_cfg(
        root: &std::path::Path,
        state: &std::path::Path,
        url: &str,
    ) -> crate::cli::IndexCfg {
        let args = vec![
            root.to_string_lossy().to_string(),
            "--url".into(),
            url.into(),
            "--state-dir".into(),
            state.to_string_lossy().to_string(),
            "--prefix".into(),
            "flood2077".into(),
            "--no-graph".into(),
            "--quiet".into(),
        ];
        match crate::cli::parse(args).expect("argv parses") {
            crate::cli::Cmd::Index(cfg) => *cfg,
            other => panic!("expected index, got {other:?}"),
        }
    }

    struct Node {
        url: String,
        requests: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl Node {
        fn start(flooded: bool) -> Self {
            use std::io::{BufRead, BufReader, Read, Write};
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::{Arc, Mutex};
            use std::time::Duration;

            let listener = crate::test_loopback::bind();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let requests_bg = Arc::clone(&requests);
            let stop_bg = Arc::clone(&stop);
            let join = std::thread::spawn(move || {
                while !stop_bg.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(5)))
                                .unwrap();
                            let mut reader = BufReader::new(stream.try_clone().unwrap());
                            let mut request_line = String::new();
                            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                                continue;
                            }
                            let mut content_length = 0usize;
                            loop {
                                let mut line = String::new();
                                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                    break;
                                }
                                if line == "\r\n" || line.is_empty() {
                                    break;
                                }
                                if let Some(value) =
                                    line.to_ascii_lowercase().strip_prefix("content-length:")
                                {
                                    content_length = value.trim().parse().unwrap_or(0);
                                }
                            }
                            if content_length > 0 {
                                let mut body = vec![0; content_length];
                                let _ = reader.read_exact(&mut body);
                            }
                            let mut parts = request_line.split_whitespace();
                            let method = parts.next().unwrap_or("").to_string();
                            let path = parts.next().unwrap_or("").to_string();
                            requests_bg
                                .lock()
                                .unwrap()
                                .push((method.clone(), path.clone()));
                            let (status, body) = if method == "GET" && path == "/" {
                                (200, crate::test_loopback::XERJ_ROOT)
                            } else if method == "GET" && path.starts_with("/_cat/allocation") {
                                if flooded {
                                    (
                                        200,
                                        r#"[{"disk.percent":"98","disk.avail":"2147483648b","disk.flood_stage":"95","disk.threshold_enabled":"true","disk.flood_blocked":"false"}]"#,
                                    )
                                } else {
                                    (
                                        200,
                                        r#"[{"disk.percent":"10","disk.avail":"1073741824b","disk.flood_stage":"95","disk.threshold_enabled":"true","disk.flood_blocked":"false"}]"#,
                                    )
                                }
                            } else {
                                (404, r#"{"error":"absent"}"#)
                            };
                            let reason = if status == 200 { "OK" } else { "Not Found" };
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                }
            });
            Self {
                url,
                requests,
                stop,
                join: Some(join),
            }
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for Node {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
}
