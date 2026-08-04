//! Server-log families: CLF/combined access logs, app logs
//! (`YYYY-MM-DD HH:MM:SS,ms LEVEL [thread] message`), RFC3164 syslog.
//! One template is elected per file from the first 200 lines; unmatched
//! lines under an elected template are treated as continuations (multiline
//! stack traces) and appended to the previous record's message.
//! `key=value` pairs embedded in the message (logfmt convention) are lifted
//! into fields — names come from the DATA, not from this code.

use super::{sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink};
use anyhow::Result;
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::OnceLock;

struct Templates {
    clf: Regex,
    applog: Regex,
    syslog: Regex,
    kv: Regex,
}

fn templates() -> &'static Templates {
    static T: OnceLock<Templates> = OnceLock::new();
    T.get_or_init(|| Templates {
        clf: Regex::new(
            r#"^(\S+) (\S+) (\S+) \[([^\]]+)\] "(\S+) (\S+)(?: (\S+))?" (\d{3}) (\d+|-)(?: "([^"]*)" "([^"]*)")?"#,
        )
        .unwrap(),
        applog: Regex::new(
            r"^(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?)\s+(?:\[([^\]]+)\]\s+)?(TRACE|DEBUG|INFO|WARN(?:ING)?|ERROR|FATAL|CRITICAL)\s*(?:\[([^\]]+)\]\s*)?[:\-]?\s*(.*)$",
        )
        .unwrap(),
        syslog: Regex::new(
            r"^([A-Z][a-z]{2} +\d+ \d{2}:\d{2}:\d{2}) (\S+) ([\w./-]+)(?:\[(\d+)\])?: (.*)$",
        )
        .unwrap(),
        kv: Regex::new(r#"\b([A-Za-z_][A-Za-z0-9_.]*)=("(?:[^"\\]|\\.)*"|\S+)"#).unwrap(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Clf,
    App,
    Syslog,
}

impl Kind {
    /// Tie-break rank for the file-level election, lowest wins. Deliberately
    /// the order `parse_kind` tries the templates in, so an ambiguous FILE
    /// resolves the way an ambiguous LINE does — most specific first. The app
    /// template demands an ISO timestamp *and* a level keyword; CLF demands a
    /// quoted request line *and* a 3-digit status; the syslog pattern is by
    /// far the loosest (`Mon dd HH:MM:SS host prog: rest`) and will claim
    /// lines the other two describe better. Every variant has a distinct
    /// rank, which is what makes the ordering total.
    fn specificity(self) -> u8 {
        match self {
            Kind::App => 0,
            Kind::Clf => 1,
            Kind::Syslog => 2,
        }
    }
}

/// Elect the one template the whole file is parsed with.
///
/// `votes` is a `HashMap`, so this comparator has to be TOTAL: anything left
/// tied would be settled by the map's per-instance random hash seed, and the
/// same file would parse differently on every run. That is not a cosmetic
/// difference here — every line that does not match the elected template is
/// demoted to a continuation and glued onto the previous record's `message`,
/// so a flipped election changes which lines are records at all.
///
/// Most matching lines wins; a tie goes to the most specific template, since
/// electing the loose one drops the fields the specific one would have named.
fn elect_kind(votes: &std::collections::HashMap<Kind, usize>) -> Option<Kind> {
    votes
        .iter()
        .min_by(|(a_kind, a_n), (b_kind, b_n)| {
            b_n.cmp(a_n)
                .then_with(|| a_kind.specificity().cmp(&b_kind.specificity()))
        })
        .map(|(k, _)| *k)
}

/// Used by the sniffer: does this line look like any known log template?
pub fn probe_line(line: &str) -> bool {
    parse_kind(line).is_some()
}

fn parse_kind(line: &str) -> Option<Kind> {
    let t = templates();
    if t.applog.is_match(line) {
        Some(Kind::App)
    } else if t.clf.is_match(line) {
        Some(Kind::Clf)
    } else if t.syslog.is_match(line) {
        Some(Kind::Syslog)
    } else {
        None
    }
}

fn parse_line(line: &str, kind: Kind) -> Option<Map<String, Value>> {
    let t = templates();
    let mut m = Map::new();
    match kind {
        Kind::Clf => {
            let c = t.clf.captures(line)?;
            let g = |i: usize| c.get(i).map(|x| x.as_str().to_string());
            m.insert("ip".into(), Value::String(g(1)?));
            if let Some(id) = g(2).filter(|s| s != "-") {
                m.insert("ident".into(), Value::String(id));
            }
            if let Some(u) = g(3).filter(|s| s != "-") {
                m.insert("user".into(), Value::String(u));
            }
            m.insert("ts".into(), Value::String(g(4)?));
            m.insert("method".into(), Value::String(g(5)?));
            m.insert("path".into(), Value::String(g(6)?));
            if let Some(p) = g(7) {
                m.insert("proto".into(), Value::String(p));
            }
            m.insert(
                "status".into(),
                g(8)?.parse::<i64>().map(|v| Value::Number(v.into())).ok()?,
            );
            if let Some(b) = g(9).filter(|s| s != "-") {
                if let Ok(n) = b.parse::<i64>() {
                    m.insert("bytes".into(), Value::Number(n.into()));
                }
            }
            if let Some(r) = g(10).filter(|s| s != "-" && !s.is_empty()) {
                m.insert("referer".into(), Value::String(r));
            }
            if let Some(a) = g(11).filter(|s| s != "-" && !s.is_empty()) {
                m.insert("agent".into(), Value::String(a));
            }
        }
        Kind::App => {
            let c = t.applog.captures(line)?;
            m.insert("ts".into(), Value::String(c.get(1)?.as_str().to_string()));
            let thread = c.get(2).or(c.get(4)).map(|x| x.as_str().to_string());
            if let Some(th) = thread {
                m.insert("thread".into(), Value::String(th));
            }
            m.insert(
                "level".into(),
                Value::String(c.get(3)?.as_str().to_string()),
            );
            let msg = c.get(5).map(|x| x.as_str().to_string()).unwrap_or_default();
            lift_kv(&msg, &mut m);
        }
        Kind::Syslog => {
            let c = t.syslog.captures(line)?;
            m.insert("ts".into(), Value::String(c.get(1)?.as_str().to_string()));
            m.insert("host".into(), Value::String(c.get(2)?.as_str().to_string()));
            m.insert(
                "program".into(),
                Value::String(c.get(3)?.as_str().to_string()),
            );
            if let Some(pid) = c.get(4) {
                if let Ok(n) = pid.as_str().parse::<i64>() {
                    m.insert("pid".into(), Value::Number(n.into()));
                }
            }
            let msg = c.get(5)?.as_str().to_string();
            lift_kv(&msg, &mut m);
        }
    }
    Some(m)
}

/// Lift `key=value` pairs out of the message (≥2 pairs). If a lifted key is
/// `msg`/`message`, its value becomes the record's `message`; otherwise the
/// raw message is kept.
fn lift_kv(msg: &str, m: &mut Map<String, Value>) {
    let t = templates();
    let pairs: Vec<(String, String)> =
        t.kv.captures_iter(msg)
            .map(|c| {
                let k = c.get(1).unwrap().as_str().to_string();
                let mut v = c.get(2).unwrap().as_str().to_string();
                if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 {
                    v = v[1..v.len() - 1].replace("\\\"", "\"");
                }
                (k, v)
            })
            .collect();
    if pairs.len() >= 2 {
        let mut message_set = false;
        for (k, v) in pairs {
            let key = sanitize_field_name(&k);
            if key == "msg" || key == "message" {
                m.insert("message".into(), Value::String(v));
                message_set = true;
            } else {
                m.entry(key).or_insert(Value::String(v));
            }
        }
        if !message_set {
            m.insert("message".into(), Value::String(msg.to_string()));
        }
    } else if !msg.is_empty() {
        m.insert("message".into(), Value::String(msg.to_string()));
    }
}

pub fn extract(
    path: &Path,
    gzip: bool,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let mut r = super::open_reader(path, gzip, limit_bytes)?;
    let mut stats = ExtractStats::default();

    // Election pass state: elect from the first 200 parsed lines.
    let mut elected: Option<Kind> = None;
    let mut votes: std::collections::HashMap<Kind, usize> = Default::default();
    let mut seen = 0usize;

    let mut offset: u64 = 0;
    let mut line_buf: Vec<u8> = Vec::new();
    let mut pending: Option<RawRecord> = None; // last record, may absorb continuations

    loop {
        line_buf.clear();
        let n = super::jsonl::read_capped_line(&mut r, &mut line_buf)?;
        if n == 0 {
            break;
        }
        let start = offset;
        offset += n as u64;
        let (line, _) = crate::sniff::decode_text(&line_buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        let kind = match elected {
            Some(k) => parse_kind(line).filter(|&pk| pk == k).or(Some(k)),
            None => {
                let pk = parse_kind(line);
                if let Some(k) = pk {
                    *votes.entry(k).or_default() += 1;
                }
                seen += 1;
                if seen >= 200 || votes.values().sum::<usize>() >= 120 {
                    elected = elect_kind(&votes);
                }
                pk.or(elected)
            }
        };
        let parsed = kind.and_then(|k| parse_line(line, k));
        match parsed {
            Some(fields) => {
                if let Some(rec) = pending.take() {
                    stats.records += 1;
                    if !sink(rec) {
                        return Ok(stats);
                    }
                }
                pending = Some(RawRecord {
                    fields,
                    locator: format!("b{start}"),
                    group: None,
                    origin: FieldOrigin::Data,
                });
            }
            None => {
                // continuation line: append to previous message
                match pending.as_mut() {
                    Some(rec) => {
                        let msg = rec
                            .fields
                            .entry("message")
                            .or_insert(Value::String(String::new()));
                        if let Value::String(s) = msg {
                            if s.len() < super::MAX_LINE {
                                s.push('\n');
                                s.push_str(line);
                            }
                        }
                    }
                    None => stats.junk += 1,
                }
            }
        }
    }
    if let Some(rec) = pending {
        stats.records += 1;
        sink(rec);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_and_parse() {
        let clf = r#"198.192.155.112 - - [01/Mar/2026:00:00:03 +0000] "GET /api/v1/x HTTP/1.1" 200 20854 "-" "Mozilla/5.0""#;
        assert_eq!(parse_kind(clf), Some(Kind::Clf));
        let m = parse_line(clf, Kind::Clf).unwrap();
        assert_eq!(m["status"], serde_json::json!(200));
        assert_eq!(m["ts"], serde_json::json!("01/Mar/2026:00:00:03 +0000"));

        let app = r#"2026-03-17 00:00:13,529 INFO  [ingest-svc] tenant=t-n user=u-1153 latency_ms=221 msg="Segment merge.""#;
        assert_eq!(parse_kind(app), Some(Kind::App));
        let m = parse_line(app, Kind::App).unwrap();
        assert_eq!(m["level"], serde_json::json!("INFO"));
        assert_eq!(m["tenant"], serde_json::json!("t-n"));
        assert_eq!(m["message"], serde_json::json!("Segment merge."));
    }

    const APP_LINE: &str = r#"2026-03-17 00:00:13,529 INFO  [ingest-svc] tenant=t-n user=u-1153 latency_ms=221 msg="Segment merge.""#;
    const CLF_LINE: &str = r#"198.192.155.112 - - [01/Mar/2026:00:00:03 +0000] "GET /api/v1/x HTTP/1.1" 200 20854 "-" "Mozilla/5.0""#;

    /// Every election builds its own `HashMap`, and every fresh map gets its
    /// own random hash seed — so 500 in-process elections over one unchanged
    /// tally sample 500 different iteration orders. `max_by_key` settled a tie
    /// by whichever of them came last.
    #[test]
    fn a_tied_template_election_elects_the_most_specific_template_every_run() {
        for _ in 0..500 {
            let votes: std::collections::HashMap<Kind, usize> =
                [(Kind::Clf, 7), (Kind::App, 7), (Kind::Syslog, 7)]
                    .into_iter()
                    .collect();
            assert_eq!(
                elect_kind(&votes),
                Some(Kind::App),
                "a three-way tie goes to the app template, the most constrained one"
            );

            let votes: std::collections::HashMap<Kind, usize> =
                [(Kind::Syslog, 5), (Kind::Clf, 5)].into_iter().collect();
            assert_eq!(
                elect_kind(&votes),
                Some(Kind::Clf),
                "syslog is the loosest template and never wins a tie"
            );

            let votes: std::collections::HashMap<Kind, usize> =
                [(Kind::Syslog, 6), (Kind::App, 5), (Kind::Clf, 5)]
                    .into_iter()
                    .collect();
            assert_eq!(
                elect_kind(&votes),
                Some(Kind::Syslog),
                "specificity is only the tie-break; a real majority still wins"
            );

            assert_eq!(elect_kind(&Default::default()), None);
        }
    }

    /// The end-to-end symptom: a file whose first 120 parsed lines split 60/60
    /// between two templates. Whichever kind is elected, every later line that
    /// does not match it is demoted to a continuation, so the flip changed the
    /// RECORD COUNT of an unchanged file from run to run.
    #[test]
    fn a_file_tied_between_two_templates_parses_identically_every_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tied.log");
        let mut text = String::new();
        for i in 0..120 {
            text.push_str(if i % 2 == 0 { APP_LINE } else { CLF_LINE });
            text.push('\n');
        }
        // Read after the election has been settled by the 120-line tie.
        for _ in 0..10 {
            text.push_str(CLF_LINE);
            text.push('\n');
        }
        std::fs::write(&path, &text).unwrap();

        for _ in 0..200 {
            let mut recs = Vec::new();
            let stats = extract(&path, false, None, &mut |r| {
                recs.push(r);
                true
            })
            .unwrap();
            assert_eq!(
                stats.records, 120,
                "the app template is elected, so the 10 trailing CLF lines are \
                 continuations of record 120, not 10 records of their own"
            );
            assert_eq!(recs[0].fields["level"], serde_json::json!("INFO"));
            assert_eq!(
                recs[119].fields["ip"],
                serde_json::json!("198.192.155.112"),
                "lines parsed before the election keep their own template"
            );
            let tail = recs[119].fields["message"].as_str().unwrap();
            assert_eq!(
                tail.lines().count(),
                11,
                "a CLF record has no message of its own, so the field is the \
                 empty seed plus the 10 absorbed continuation lines"
            );
        }
    }
}
