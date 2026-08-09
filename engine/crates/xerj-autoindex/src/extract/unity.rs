//! Unity text-serialized assets — streaming multi-document splitter — and
//! `.meta` sidecars.
//!
//! Unity YAML is a multi-document stream where every document opens with
//! `--- !u!<classId> &<fileID>` (optionally suffixed `stripped` for prefab
//! instances). serde_yaml cannot be used on the whole stream: it resolves
//! anchors internally and never exposes the anchor NAME, and the `fileID`
//! that makes Unity objects joinable lives in that anchor. So the splitter
//! owns the document boundaries (a cheap line prefix match) and hands each
//! body — plain YAML once the `%YAML`/`%TAG` header is dropped — to
//! serde_yaml individually. Owning the boundaries is also what makes it safe
//! to `continue` past a corrupt document: unlike `yaml_x`, there is no
//! iterator that would re-yield the same broken document forever.
//!
//! Files stream line-by-line (real scenes can exceed 200 MB; a single
//! AnimationClip document can be tens of MB), with a 16 MiB per-document
//! cap: over-cap documents are junked and skipped to the next boundary,
//! the rest of the file survives.
//!
//! Emitted fields per object document:
//! - `unity_class`      body's top-level key (authoritative), else class-id table
//! - `unity_class_id`   from the `!u!<id>` tag
//! - `file_id`          from the `&<anchor>` — the join key for local refs
//! - `stripped`         only present (true) on stripped prefab-instance docs
//! - `name`             `m_Name` when present
//! - `ref_guids`        deduped guids of every `{fileID, guid, type}` reference
//! - `script_guid`      the `m_Script` reference's guid (MonoBehaviour → .cs)
//! - flattened body fields
//!
//! `.meta` sidecars emit ONE record: `guid`, `asset_name`, `importer`, and
//! flattened settings. The root-relative `asset_path` (and the
//! `script_path`/`script_class` denormalization on MonoBehaviour records) is
//! stamped by the pipeline, which knows the walk root — extractors stay
//! per-file pure.

use super::{
    flatten_object, yaml_to_json, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_LINE,
};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

/// Per-document cap. One logical unit follows the `MAX_LINE` convention;
/// 16 MiB covers large AnimationClip / font documents while bounding memory.
/// (A 4 MiB cap was considered and rejected: it junks those documents.)
const DOC_CAP: usize = 16 << 20;

const META_CAP: u64 = 16 << 20;

/// Field names this extractor invents on object records. A body field with
/// the same name (possible on MonoBehaviour custom fields) is renamed
/// `data_<name>` rather than dropped, mirroring the `ax_*` collision policy.
const RESERVED: &[&str] = &[
    "unity_class",
    "unity_class_id",
    "file_id",
    "stripped",
    "name",
    "ref_guids",
    "script_guid",
    "script_path",
    "script_class",
];

pub fn extract_unity(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut r = super::open_reader(path, gzip, limit_bytes)?;
    let mut stats = ExtractStats::default();
    let mut offset: u64 = 0;
    let mut line: Vec<u8> = Vec::new();
    let mut doc: Vec<u8> = Vec::new();
    let mut header: Option<DocHeader> = None;
    let mut skipping = false;
    loop {
        line.clear();
        let n = super::jsonl::read_capped_line(&mut r, &mut line)?;
        let eof = n == 0;
        offset += n as u64;
        let boundary = if eof { None } else { parse_boundary(&line) };
        if eof || boundary.is_some() {
            if let Some(h) = header.take() {
                if skipping {
                    skipping = false;
                } else {
                    let truncated =
                        eof && limit_bytes.is_some_and(|l| offset >= l);
                    if !emit_doc(&h, &doc, truncated, &mut stats, sink) {
                        return Ok(stats);
                    }
                }
            }
            doc.clear();
            header = boundary;
            if eof {
                break;
            }
            continue;
        }
        if header.is_none() || skipping {
            continue; // %YAML/%TAG preamble, or discarding an over-cap doc
        }
        if doc.len() + line.len() > DOC_CAP {
            stats.junk += 1;
            skipping = true;
            doc.clear();
            continue;
        }
        doc.extend_from_slice(&line);
    }
    // Empty yield from a FULL read means the file was preamble-only (or
    // junk-shaped throughout) — record it. A sampling read that truncated
    // before the first document completed is not evidence of anything.
    let truncated_run = limit_bytes.is_some_and(|l| offset >= l);
    if stats.records == 0 && stats.junk == 0 && !truncated_run {
        stats.junk += 1;
    }
    Ok(stats)
}

struct DocHeader {
    class_id: i64,
    /// Raw anchor text (handles negative 64-bit ids verbatim).
    file_id: String,
    stripped: bool,
}

/// `--- !u!<classId> &<fileID>[ stripped]` — cheap manual parse, no regex on
/// the hot per-line path.
fn parse_boundary(line: &[u8]) -> Option<DocHeader> {
    let s = std::str::from_utf8(line).ok()?.trim_end();
    let rest = s.strip_prefix("--- !u!")?;
    let (id_part, rest) = rest.split_once(" &")?;
    let class_id: i64 = id_part.trim().parse().ok()?;
    let (anchor, tail) = match rest.split_once(' ') {
        Some((a, t)) => (a, t.trim()),
        None => (rest, ""),
    };
    let anchor = anchor.trim();
    if anchor.is_empty()
        || !anchor
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    Some(DocHeader {
        class_id,
        file_id: anchor.to_string(),
        stripped: tail == "stripped",
    })
}

fn emit_doc(
    h: &DocHeader,
    body: &[u8],
    truncated_by_sampling: bool,
    stats: &mut ExtractStats,
    sink: Sink,
) -> bool {
    let text = String::from_utf8_lossy(body);
    let parsed: serde_yaml::Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            if !truncated_by_sampling {
                stats.junk += 1;
            }
            return true;
        }
    };
    let jv = yaml_to_json(parsed);
    let (class_from_body, inner) = match jv {
        Value::Object(m) if m.len() == 1 => {
            let (k, v) = m.into_iter().next().expect("len checked");
            (Some(k), v)
        }
        other => (None, other),
    };
    let unity_class = class_from_body
        .unwrap_or_else(|| class_name(h.class_id).map(str::to_string).unwrap_or_else(|| format!("class_{}", h.class_id)));

    let mut refs: Vec<String> = Vec::new();
    let mut script_guid: Option<String> = None;
    collect_guid_refs(&inner, &mut refs);
    if let Some(script) = inner.get("m_Script") {
        if let Some(g) = script.get("guid").and_then(Value::as_str) {
            script_guid = Some(g.to_string());
        }
    }

    let mut fields = match inner {
        Value::Object(m) => flatten_object(m),
        Value::Null => Map::new(),
        other => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            m
        }
    };
    for k in RESERVED {
        if let Some(v) = fields.remove(*k) {
            fields.insert(format!("data_{k}"), v);
        }
    }
    if let Some(Value::String(n)) = fields.get("m_Name") {
        if !n.is_empty() {
            fields.insert("name".into(), Value::String(n.clone()));
        }
    }
    fields.insert("unity_class".into(), Value::String(unity_class.clone()));
    fields.insert("unity_class_id".into(), Value::Number(h.class_id.into()));
    fields.insert("file_id".into(), Value::String(h.file_id.clone()));
    if h.stripped {
        fields.insert("stripped".into(), Value::Bool(true));
    }
    if !refs.is_empty() {
        let mut seen = std::collections::HashSet::new();
        let deduped: Vec<Value> = refs
            .into_iter()
            .filter(|g| seen.insert(g.clone()))
            .map(Value::String)
            .collect();
        fields.insert("ref_guids".into(), Value::Array(deduped));
    }
    if let Some(g) = script_guid {
        fields.insert("script_guid".into(), Value::String(g));
    }

    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: format!("u{}", h.file_id),
        group: Some(unity_class),
        // Body field names are Unity's serialization vocabulary plus per-script
        // custom fields; letting them cluster would split MonoBehaviours into
        // schema-similarity datasets and re-home documents when scripts change
        // (issue #178). Family+group clustering gives one stable dataset per
        // Unity class instead.
        origin: FieldOrigin::Extractor,
    })
}

/// Walk the parsed document for `{fileID, guid, type}` reference mappings and
/// collect every guid, including inside arrays (`m_Modifications` — prefab
/// overrides — is where the highest-value references live).
fn collect_guid_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if m.contains_key("fileID") {
                if let Some(g) = m.get("guid").and_then(Value::as_str) {
                    out.push(g.to_string());
                }
            }
            for vv in m.values() {
                collect_guid_refs(vv, out);
            }
        }
        Value::Array(a) => {
            for vv in a {
                collect_guid_refs(vv, out);
            }
        }
        _ => {}
    }
}

pub fn extract_meta(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = super::read_whole(path, gzip, META_CAP)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    let (text, _) = crate::sniff::decode_text(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let parsed: serde_yaml::Value = match serde_yaml::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            stats.junk += 1;
            return Ok(stats);
        }
    };
    let Value::Object(m) = yaml_to_json(parsed) else {
        stats.junk += 1;
        return Ok(stats);
    };
    let guid = m.get("guid").and_then(Value::as_str).map(str::to_string);
    let importer = m
        .keys()
        .find(|k| k.ends_with("Importer") || k.ends_with("importer"))
        .cloned();
    let mut fields = flatten_object(m);
    if let Some(g) = guid {
        fields.insert("guid".into(), Value::String(g));
    }
    if let Some(imp) = importer {
        fields.insert("importer".into(), Value::String(imp));
    }
    if let Some(name) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".meta"))
    {
        fields.insert("asset_name".into(), Value::String(name.to_string()));
    }
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: "meta".into(),
        group: None,
        origin: FieldOrigin::Extractor,
    });
    Ok(stats)
}

/// Fallback names for documents whose body failed to yield a top-level key
/// (the body key is authoritative when present). Sourced from Unity's public
/// class-id reference; unknown ids render `class_<id>`.
fn class_name(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "GameObject",
        2 => "Component",
        4 => "Transform",
        20 => "Camera",
        21 => "Material",
        23 => "MeshRenderer",
        25 => "Renderer",
        28 => "Texture2D",
        33 => "MeshFilter",
        43 => "Mesh",
        54 => "Rigidbody",
        61 => "BoxCollider2D",
        64 => "MeshCollider",
        65 => "BoxCollider",
        74 => "AnimationClip",
        81 => "AudioListener",
        82 => "AudioSource",
        90 => "Avatar",
        91 => "AnimatorController",
        95 => "Animator",
        104 => "RenderSettings",
        108 => "Light",
        114 => "MonoBehaviour",
        115 => "MonoScript",
        128 => "Font",
        135 => "SphereCollider",
        136 => "CapsuleCollider",
        137 => "SkinnedMeshRenderer",
        157 => "LightmapSettings",
        196 => "NavMeshSettings",
        198 => "ParticleSystem",
        199 => "ParticleSystemRenderer",
        212 => "SpriteRenderer",
        213 => "Sprite",
        224 => "RectTransform",
        320 => "PlayableDirector",
        850595691 => "LightingSettings",
        1001 => "PrefabInstance",
        1660057539 => "SceneRoots",
        _ => return None,
    })
}

// The `MAX_LINE` import is load-bearing indirectly: `read_capped_line` caps
// each physical line at 16 MiB, and DOC_CAP matches it so a single-line
// document cannot bypass the document cap.
const _: () = assert!(DOC_CAP == MAX_LINE);

#[cfg(test)]
mod tests {
    use super::*;

    const SCENE: &str = "\u{feff}%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &1234567890\nGameObject:\n  m_ObjectHideFlags: 0\n  m_Name: Player\n  m_IsActive: 1\n--- !u!4 &1234567891\nTransform:\n  m_GameObject: {fileID: 1234567890}\n  m_LocalPosition: {x: 0, y: 1.5, z: 0}\n--- !u!114 &1234567892\nMonoBehaviour:\n  m_GameObject: {fileID: 1234567890}\n  m_Script: {fileID: 11500000, guid: abc123def456, type: 3}\n  m_EditorClassIdentifier: \n  speed: 4.5\n  target: {fileID: 5555, guid: fedcba987654, type: 2}\n";

    fn run(text: &str, limit: Option<u64>) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.unity");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract_unity(&path, false, limit, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    fn run_meta(text: &str, name: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract_meta(&path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    #[test]
    fn each_document_becomes_one_record_with_class_and_file_id() {
        let (stats, recs) = run(SCENE, None);
        assert_eq!((stats.records, stats.junk), (3, 0));
        assert_eq!(recs[0].fields["unity_class"], "GameObject");
        assert_eq!(recs[0].fields["unity_class_id"], 1);
        assert_eq!(recs[0].fields["file_id"], "1234567890");
        assert_eq!(recs[0].fields["name"], "Player");
        assert_eq!(recs[0].locator, "u1234567890");
        assert_eq!(recs[0].group.as_deref(), Some("GameObject"));
        assert_eq!(recs[1].fields["unity_class"], "Transform");
        assert_eq!(
            recs[1].fields["m_LocalPosition_y"], 1.5,
            "nested mappings flatten like every other extractor"
        );
    }

    #[test]
    fn monobehaviour_carries_script_guid_and_deduped_ref_guids() {
        let (_, recs) = run(SCENE, None);
        let mb = &recs[2];
        assert_eq!(mb.fields["unity_class"], "MonoBehaviour");
        assert_eq!(mb.fields["script_guid"], "abc123def456");
        assert_eq!(
            mb.fields["ref_guids"],
            serde_json::json!(["abc123def456", "fedcba987654"])
        );
        assert_eq!(mb.fields["speed"], 4.5, "custom script fields survive");
    }

    #[test]
    fn stripped_and_negative_file_ids_parse() {
        let text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!4 &-8679921383154817045 stripped\nTransform:\n  m_PrefabInstance: {fileID: 100100000, guid: 0123456789abcdef, type: 3}\n";
        let (stats, recs) = run(text, None);
        assert_eq!((stats.records, stats.junk), (1, 0));
        assert_eq!(recs[0].fields["file_id"], "-8679921383154817045");
        assert_eq!(recs[0].fields["stripped"], true);
        assert_eq!(recs[0].locator, "u-8679921383154817045");
    }

    #[test]
    fn crlf_line_endings_are_tolerated() {
        let text = SCENE.replace('\n', "\r\n");
        let (stats, recs) = run(&text, None);
        assert_eq!((stats.records, stats.junk), (3, 0));
        assert_eq!(recs[2].fields["script_guid"], "abc123def456");
    }

    #[test]
    fn a_corrupt_document_junks_only_itself() {
        let text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &1\nGameObject:\n  m_Name: A\n--- !u!4 &2\n\t{{{ not yaml\n--- !u!1 &3\nGameObject:\n  m_Name: B\n";
        let (stats, recs) = run(text, None);
        assert_eq!(stats.junk, 1);
        assert_eq!(stats.records, 2);
        assert_eq!(recs[1].fields["name"], "B");
    }

    #[test]
    fn an_over_cap_document_is_skipped_and_the_stream_continues() {
        let mut text = String::from("%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!74 &1\nAnimationClip:\n  m_Name: huge\n  data: ");
        text.push_str(&"x".repeat(DOC_CAP + 1024));
        text.push_str("\n--- !u!1 &2\nGameObject:\n  m_Name: after\n");
        let (stats, recs) = run(&text, None);
        assert_eq!(stats.junk, 1, "the oversized doc is junked, not fatal");
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].fields["name"], "after");
    }

    #[test]
    fn a_doc_cut_short_by_a_sampling_limit_is_not_junk() {
        let (stats, _) = run(SCENE, Some(120));
        assert_eq!(stats.junk, 0, "sampling truncation is not corruption");
        assert!(stats.records <= 1);
    }

    #[test]
    fn sampling_early_stop_via_sink_leaves_no_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.unity");
        std::fs::write(&path, SCENE).unwrap();
        let mut n = 0;
        let stats = extract_unity(&path, false, None, &mut |_r| {
            n += 1;
            n < 2
        })
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(stats.junk, 0);
    }

    #[test]
    fn locators_are_idempotent_across_runs() {
        let (_, a) = run(SCENE, None);
        let (_, b) = run(SCENE, None);
        let la: Vec<_> = a.iter().map(|r| r.locator.clone()).collect();
        let lb: Vec<_> = b.iter().map(|r| r.locator.clone()).collect();
        assert_eq!(la, lb);
    }

    #[test]
    fn a_reserved_body_field_is_renamed_not_clobbered() {
        let text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!114 &1\nMonoBehaviour:\n  name: custom-field\n  m_Name: Real\n";
        let (_, recs) = run(text, None);
        assert_eq!(recs[0].fields["data_name"], "custom-field");
        assert_eq!(recs[0].fields["name"], "Real");
    }

    #[test]
    fn unknown_class_id_with_unparseable_body_still_names_the_class() {
        let text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!999999 &1\nsome scalar\n";
        let (stats, recs) = run(text, None);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].fields["unity_class"], "class_999999");
    }

    #[test]
    fn meta_yields_guid_importer_and_asset_name() {
        let text = "fileFormatVersion: 2\nguid: 9f1c4d0ab2e34f6\nMonoImporter:\n  externalObjects: {}\n  serializedVersion: 2\n";
        let (stats, recs) = run_meta(text, "Player.cs.meta");
        assert_eq!((stats.records, stats.junk), (1, 0));
        assert_eq!(recs[0].fields["guid"], "9f1c4d0ab2e34f6");
        assert_eq!(recs[0].fields["importer"], "MonoImporter");
        assert_eq!(recs[0].fields["asset_name"], "Player.cs");
        assert_eq!(recs[0].locator, "meta");
    }

    #[test]
    fn a_file_with_only_preamble_is_junk() {
        let (stats, recs) = run("%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n", None);
        assert_eq!(stats.records, 0);
        assert!(stats.junk >= 1);
        assert!(recs.is_empty());
    }
}
