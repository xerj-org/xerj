//! BVH motion capture — ONE metadata record per file, motion data never read.
//!
//! A BVH file is a small skeleton HIERARCHY header followed by a numeric
//! MOTION block that can run to hundreds of MB (one line per frame). The
//! frames carry no retrieval value — they are float soup — but the header
//! does: joint names make the skeleton searchable ("which clips animate
//! LeftHand?"), and frame count/time give the clip's duration. So this
//! extractor streams the header, captures that metadata, and STOPS as soon
//! as the two `Frames:`/`Frame Time:` lines after `MOTION` are read — the
//! motion block is never pulled off disk.
//!
//! Emitted fields: `title`, `joints` (names, document order), `joint_count`,
//! `frames`, `frame_time_s`, `duration_s`. All extractor vocabulary
//! (`FieldOrigin::Extractor`), so every BVH file clusters into one dataset.

use super::{ExtractStats, FieldOrigin, RawRecord, Sink};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

/// Skeleton headers are a few KB; a "header" that runs past this is not a
/// BVH we understand, and the file junks rather than streaming megabytes.
const HEADER_CAP: usize = 1 << 20;

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let mut r = super::open_reader(path, gzip, None)?;
    let mut joints: Vec<String> = Vec::new();
    let mut frames: Option<i64> = None;
    let mut frame_time: Option<f64> = None;
    let mut in_motion = false;
    let mut consumed = 0usize;
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        let n = super::jsonl::read_capped_line(&mut r, &mut line)?;
        if n == 0 {
            break;
        }
        consumed += n;
        if consumed > HEADER_CAP {
            stats.junk += 1;
            return Ok(stats);
        }
        let text = String::from_utf8_lossy(&line);
        let t = text.trim();
        if !in_motion {
            if let Some(name) = t
                .strip_prefix("ROOT ")
                .or_else(|| t.strip_prefix("JOINT "))
            {
                let name = name.trim();
                if !name.is_empty() && name.len() <= 200 {
                    joints.push(name.to_string());
                }
            } else if t == "MOTION" {
                in_motion = true;
            }
            continue;
        }
        if let Some(v) = t.strip_prefix("Frames:") {
            frames = v.trim().parse().ok();
        } else if let Some(v) = t.strip_prefix("Frame Time:") {
            frame_time = v.trim().parse().ok();
        }
        if frames.is_some() && frame_time.is_some() {
            break; // metadata complete — the motion block is never read
        }
    }
    if joints.is_empty() {
        stats.junk += 1;
        return Ok(stats);
    }

    let title = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("motion")
        .to_string();
    let mut fields = Map::new();
    fields.insert("title".into(), Value::String(title));
    fields.insert(
        "joints".into(),
        Value::Array(joints.iter().map(|j| Value::String(j.clone())).collect()),
    );
    fields.insert(
        "joint_count".into(),
        Value::Number((joints.len() as u64).into()),
    );
    if let Some(f) = frames {
        fields.insert("frames".into(), Value::Number(f.into()));
    }
    if let Some(ft) = frame_time {
        if let Some(num) = serde_json::Number::from_f64(ft) {
            fields.insert("frame_time_s".into(), Value::Number(num));
        }
        if let (Some(f), true) = (frames, ft.is_finite()) {
            if let Some(num) = serde_json::Number::from_f64(f as f64 * ft) {
                fields.insert("duration_s".into(), Value::Number(num));
            }
        }
    }
    stats.records += 1;
    sink(RawRecord {
        fields,
        locator: "bvh".into(),
        group: None,
        origin: FieldOrigin::Extractor,
    });
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "HIERARCHY\nROOT Hips\n{\n  OFFSET 0 90 0\n  CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation\n  JOINT LeftUpLeg\n  {\n    OFFSET 8.5 0 -2.5\n    CHANNELS 3 Zrotation Xrotation Yrotation\n    End Site\n    {\n      OFFSET 6.9 0 0\n    }\n  }\n}\nMOTION\nFrames: 120\nFrame Time: 0.033333\n0.0 90.0 0.0 1.1 2.2 3.3 4.4 5.5 6.6\n0.1 90.1 0.1 1.2 2.3 3.4 4.5 5.6 6.7\n";

    fn run(text: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.bvh");
        std::fs::write(&path, text).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    #[test]
    fn one_metadata_record_per_file_and_no_motion_rows() {
        let (stats, recs) = run(SAMPLE);
        assert_eq!((stats.records, stats.junk), (1, 0));
        let r = &recs[0];
        assert_eq!(r.fields["title"], "clip.bvh");
        assert_eq!(r.fields["joints"], serde_json::json!(["Hips", "LeftUpLeg"]));
        assert_eq!(r.fields["joint_count"], 2);
        assert_eq!(r.fields["frames"], 120);
        assert_eq!(r.fields["duration_s"].as_f64().unwrap().round(), 4.0);
        assert_eq!(r.locator, "bvh");
    }

    /// The point of the format: a clip with a multi-GB motion block must cost
    /// only its header. The reader stops after `Frame Time:`, so the frames
    /// after it are never even pulled through the BufReader into the record.
    #[test]
    fn extraction_stops_at_the_motion_header_not_the_motion_data() {
        let mut text = SAMPLE.to_string();
        text.push_str(&"9.9 ".repeat(500_000));
        let (stats, recs) = run(&text);
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].fields["frames"], 120);
    }

    #[test]
    fn a_hierarchy_without_joints_is_junk_not_an_empty_record() {
        let (stats, recs) = run("HIERARCHY\nMOTION\nFrames: 1\nFrame Time: 0.03\n1 2 3\n");
        assert_eq!(stats.records, 0);
        assert_eq!(stats.junk, 1);
        assert!(recs.is_empty());
    }

    #[test]
    fn a_runaway_header_is_junk_rather_than_streamed_forever() {
        let mut text = String::from("HIERARCHY\nROOT Hips\n{\n");
        for i in 0..40_000 {
            text.push_str(&format!("  JOINT J{i}\n  {{\n  OFFSET 0 0 0\n"));
        }
        let (stats, _) = run(&text);
        assert_eq!(stats.records, 0);
        assert_eq!(stats.junk, 1);
    }
}
