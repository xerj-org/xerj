//! Server-log families: CLF/combined access logs, app logs
//! (`YYYY-MM-DD HH:MM:SS,ms LEVEL [thread] message`), RFC3164 syslog (with an
//! optional `<PRI>`, the form desk phones actually send), RFC5424
//! (`<PRI>VERSION TS host app procid msgid SD msg`), and — since #1504 —
//! ISO-prefixed syslog (`journalctl -o short-iso` and friends:
//! `TS host prog[pid]: message`, with an optional `<PRI>` whose severity is
//! decoded) and `xferlog` (`vsftpd`/wu-ftpd transfer records, 14 fields).
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
    rfc5424: Regex,
    isosyslog: Regex,
    xferlog: Regex,
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
        // Optional `<PRI>`: desk-phone and BSD syslog write
        // `<PRI>Mon dd hh:mm:ss host tag[pid]: msg` and the PRI-less form
        // already recognised. A single space before `[pid]` is accepted
        // (`SIP [990]:`) because that is how several phone stacks print the
        // tag; the space is part of the bracket, so `host notes: prose`
        // still parses as a tag with no pid, the same as before.
        syslog: Regex::new(
            r"^(?:<(\d{1,3})>)?([A-Z][a-z]{2} +\d+ \d{2}:\d{2}:\d{2}) (\S+) ([\w./-]+)(?: ?\[(\d+)\])?: (.*)$",
        )
        .unwrap(),
        // RFC 5424: `<PRI>VERSION SP TIMESTAMP SP HOST SP APP SP PROCID SP
        // MSGID SP STRUCTURED-DATA [SP MSG]`. VERSION is what separates this
        // from PRI-prefixed ISO syslog (`<PRI>TS host tag[pid]: msg`), which
        // has no version digit between the priority and the timestamp.
        // NILVALUE `-` is allowed for APP, PROCID, MSGID and STRUCTURED-DATA.
        rfc5424: Regex::new(
            r"^<(\d{1,3})>(\d+) (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?:Z|[+-]\d{2}:\d{2})?) (\S+) (\S+) (\S+) (\S+) (?:-|\[(?:\\.|[^\]])*\])(?: (.*))?$",
        )
        .unwrap(),
        // #1504: `journalctl -o short-iso` / rsyslog ISO templates —
        // `TS host TAG[pid]: message`, ISO-8601 timestamp (offset optional),
        // an optional leading `<PRI>` (facility*8 + severity) decoded into
        // `severity`/`facility`.
        //
        // Deliberately strict, because the Logs probe runs before CSV and
        // prose detection and an ISO timestamp opens half the log lines in
        // the world (#1504 review):
        //   * inter-field separators are single literal spaces, so
        //     tab-separated TSV and zap dev logs do not match;
        //   * the tag starts with a letter or `(` — an `IPv4:port` token
        //     (ELB access logs) starts with a digit and does not match;
        //   * a tag is either bracketed with a numeric pid, or a bare tag
        //     from the small set syslog(3) writers emit without LOG_PID
        //     (`kernel`, `sudo`, `su`, `audit`) or a parenthesised systemd
        //     unit/executable name (`(sd-pam)`). A bare English word —
        //     `2026-10-16T09:06:58 retro notes: we shipped` — is prose.
        isosyslog: Regex::new(
            r"^(?:<(\d{1,3})>)?(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?(?:Z|[+-]\d{2}:?\d{2})?) (\S+) (?:([A-Za-z(][^\s\[\]:]*)\[(\d+)\]|(kernel|sudo|su|audit|\([^)]*\))): ?(.*)$",
        )
        .unwrap(),
        // #1504: vsftpd/wu-ftpd `xferlog` — fixed 14-field transfer records,
        // weekday-first date. Field order: date, transfer-time(s), remote
        // host, bytes, path, transfer type (a|b), special action flag
        // (_|C|U|T), direction (o|i|d), access mode (a|r|g), user, service,
        // auth method (0|1), authenticated user id (*|name), status (c|i).
        // vsftpd writes only a|r and o|i; `g` (guest, wu-ftpd) and `d`
        // (delete, ProFTPD) are accepted because the PR covers wu-ftpd too.
        // The path is lazy, not `\S+`: vsftpd writes the filename verbatim,
        // spaces included, and the nine fixed fields after it anchor the end.
        xferlog: Regex::new(
            r"^([A-Z][a-z]{2} [A-Z][a-z]{2} +\d+ \d{2}:\d{2}:\d{2} \d{4}) (\d+) (\S+) (\d+) (\S.*?) ([ab]) ([_CUT*]) ([oid]) ([agr]) (\S+) (\S+) ([01]) (\S+) ([ci])\s*$",
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
    Rfc5424,
    IsoSyslog,
    Xferlog,
}

impl Kind {
    /// Tie-break rank for the file-level election, lowest wins. Deliberately
    /// the order `parse_kind` tries the templates in, so an ambiguous FILE
    /// resolves the way an ambiguous LINE does — most specific first. The app
    /// template demands an ISO timestamp *and* a level keyword; CLF demands a
    /// quoted request line *and* a 3-digit status; xferlog demands the fixed
    /// 14-field record shape; RFC5424 demands `<PRI>`, a version and an ISO
    /// timestamp; the ISO-syslog template demands an ISO timestamp *and* the
    /// `prog[pid]:` colon form; the RFC3164 pattern is by far the loosest
    /// (`Mon dd HH:MM:SS host prog: rest`, optional `<PRI>`) and will claim
    /// lines the others describe better. Every variant has a distinct rank,
    /// which is what makes the ordering total.
    fn specificity(self) -> u8 {
        match self {
            Kind::App => 0,
            Kind::Clf => 1,
            Kind::Xferlog => 2,
            Kind::Rfc5424 => 3,
            Kind::IsoSyslog => 4,
            Kind::Syslog => 5,
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
    } else if t.xferlog.is_match(line) {
        Some(Kind::Xferlog)
    } else if t.rfc5424.is_match(line) {
        Some(Kind::Rfc5424)
    } else if t.isosyslog.is_match(line) {
        Some(Kind::IsoSyslog)
    } else if t.syslog.is_match(line) {
        Some(Kind::Syslog)
    } else {
        None
    }
}

/// `<PRI>` = facility*8 + severity (RFC 5424 §6.2.1). Only call this when
/// the writer emitted a priority. The domain is 0..=191 (facility 0..=23 ×
/// 8 + severity 0..=7); `<999>` is not a priority, so it stays unrecorded
/// rather than decoding into a facility of 124. A line with no `<PRI>`
/// (journalctl short-iso, classic RFC3164) must not call this — inventing
/// a severity the file does not contain is a lie about the data.
fn record_priority(pri: u16, m: &mut Map<String, Value>) {
    if pri > 191 {
        return;
    }
    const SEVERITY: [&str; 8] = [
        "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug",
    ];
    m.insert(
        "severity".into(),
        Value::String(SEVERITY[(pri & 7) as usize].to_string()),
    );
    m.insert("facility".into(), Value::Number((pri >> 3).into()));
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
            if let Some(pri) = c.get(1).and_then(|p| p.as_str().parse::<u16>().ok()) {
                record_priority(pri, &mut m);
            }
            m.insert("ts".into(), Value::String(c.get(2)?.as_str().to_string()));
            m.insert("host".into(), Value::String(c.get(3)?.as_str().to_string()));
            m.insert(
                "program".into(),
                Value::String(c.get(4)?.as_str().to_string()),
            );
            if let Some(pid) = c.get(5) {
                if let Ok(n) = pid.as_str().parse::<i64>() {
                    m.insert("pid".into(), Value::Number(n.into()));
                }
            }
            let msg = c.get(6)?.as_str().to_string();
            lift_kv(&msg, &mut m);
        }
        Kind::Rfc5424 => {
            let c = t.rfc5424.captures(line)?;
            if let Some(pri) = c.get(1).and_then(|p| p.as_str().parse::<u16>().ok()) {
                record_priority(pri, &mut m);
            }
            m.insert("ts".into(), Value::String(c.get(3)?.as_str().to_string()));
            m.insert("host".into(), Value::String(c.get(4)?.as_str().to_string()));
            let program = c.get(5)?.as_str();
            if program != "-" {
                m.insert("program".into(), Value::String(program.to_string()));
            }
            if let Some(pid) = c.get(6).filter(|p| p.as_str() != "-") {
                if let Ok(n) = pid.as_str().parse::<i64>() {
                    m.insert("pid".into(), Value::Number(n.into()));
                }
            }
            if let Some(msgid) = c.get(7).filter(|p| p.as_str() != "-") {
                m.insert("msgid".into(), Value::String(msgid.as_str().to_string()));
            }
            let msg = c.get(8).map(|x| x.as_str().to_string()).unwrap_or_default();
            lift_kv(&msg, &mut m);
        }
        Kind::IsoSyslog => {
            let c = t.isosyslog.captures(line)?;
            if let Some(pri) = c.get(1).and_then(|p| p.as_str().parse::<u16>().ok()) {
                record_priority(pri, &mut m);
            }
            m.insert("ts".into(), Value::String(c.get(2)?.as_str().to_string()));
            m.insert("host".into(), Value::String(c.get(3)?.as_str().to_string()));
            // Group 4 is a tagged pid, group 6 a bare no-pid tag.
            let program = c.get(4).or_else(|| c.get(6))?;
            m.insert(
                "program".into(),
                Value::String(program.as_str().to_string()),
            );
            if let Some(pid) = c.get(5) {
                if let Ok(n) = pid.as_str().parse::<i64>() {
                    m.insert("pid".into(), Value::Number(n.into()));
                }
            }
            let msg = c.get(7)?.as_str().to_string();
            lift_kv(&msg, &mut m);
        }
        Kind::Xferlog => {
            let c = t.xferlog.captures(line)?;
            let g = |i: usize| c.get(i).map(|x| x.as_str().to_string());
            m.insert("ts".into(), Value::String(g(1)?));
            if let Ok(n) = g(2)?.parse::<i64>() {
                m.insert("duration_s".into(), Value::Number(n.into()));
            }
            m.insert("host".into(), Value::String(g(3)?));
            if let Ok(n) = g(4)?.parse::<i64>() {
                m.insert("bytes".into(), Value::Number(n.into()));
            }
            m.insert("path".into(), Value::String(g(5)?));
            // The single-letter xferlog fields are spelled out: `b`/`a`,
            // `o`/`i`, `r`/`a` and `c`/`i` collide as raw letters (the same
            // `a` means ascii in one column and anonymous in the next), and a
            // callers-first reader is what this index is for.
            m.insert(
                "transfer_type".into(),
                Value::String(match g(6)?.as_str() {
                    "a" => "ascii".into(),
                    _ => "binary".into(),
                }),
            );
            let action = g(7)?;
            if action != "_" {
                m.insert(
                    "action".into(),
                    Value::String(
                        match action.as_str() {
                            "C" => "compressed",
                            "U" => "uncompressed",
                            "T" => "tar",
                            other => other,
                        }
                        .into(),
                    ),
                );
            }
            m.insert(
                "direction".into(),
                Value::String(
                    match g(8)?.as_str() {
                        "i" => "incoming",
                        // ProFTPD writes `d` for deletes; vsftpd never does.
                        "d" => "delete",
                        _ => "outgoing",
                    }
                    .into(),
                ),
            );
            m.insert(
                "access".into(),
                Value::String(
                    match g(9)?.as_str() {
                        "a" => "anonymous",
                        // `g` is wu-ftpd's guest access; vsftpd writes a|r only.
                        "g" => "guest",
                        _ => "real",
                    }
                    .into(),
                ),
            );
            m.insert("user".into(), Value::String(g(10)?));
            m.insert("service".into(), Value::String(g(11)?));
            m.insert(
                "auth_method".into(),
                Value::String(if g(12)? == "1" { "rfc931" } else { "none" }.into()),
            );
            let auth_user = g(13)?;
            if auth_user != "*" {
                m.insert("auth_user".into(), Value::String(auth_user));
            }
            m.insert(
                "status".into(),
                Value::String(
                    if g(14)? == "i" {
                        "incomplete"
                    } else {
                        "complete"
                    }
                    .into(),
                ),
            );
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

    /// #1504: `journalctl -o short-iso` / ISO-prefixed syslog —
    /// `TS host prog[pid]: message`, with an optional `<PRI>`.
    #[test]
    fn iso_syslog_lines_parse_to_fields() {
        let j = "2026-10-16T09:06:59+0200 pbx vsftpd[1234]: CONNECT: Client \"10.0.0.7\"";
        assert_eq!(parse_kind(j), Some(Kind::IsoSyslog));
        let m = parse_line(j, Kind::IsoSyslog).unwrap();
        assert_eq!(m["ts"], serde_json::json!("2026-10-16T09:06:59+0200"));
        assert_eq!(m["host"], serde_json::json!("pbx"));
        assert_eq!(m["program"], serde_json::json!("vsftpd"));
        assert_eq!(m["pid"], serde_json::json!(1234));
        assert_eq!(
            m["message"],
            serde_json::json!("CONNECT: Client \"10.0.0.7\"")
        );

        // Kernel line: no pid bracket, `Z`-suffixed stamp, colon-separated msg.
        let k = "2026-10-16T09:06:58Z pbx kernel: eth0: link up";
        let m = parse_line(k, Kind::IsoSyslog).unwrap();
        assert_eq!(m["ts"], serde_json::json!("2026-10-16T09:06:58Z"));
        assert_eq!(m["program"], serde_json::json!("kernel"));
        assert!(m.get("pid").is_none(), "no bracket means no pid field");
        assert_eq!(m["message"], serde_json::json!("eth0: link up"));

        // `<PRI>` = facility*8 + severity; 50 = facility 6, severity 2 (crit).
        let p = "<50>2026-10-16T09:06:58Z pbx app[7]: message";
        let m = parse_line(p, Kind::IsoSyslog).unwrap();
        assert_eq!(m["severity"], serde_json::json!("crit"));
        assert_eq!(m["facility"], serde_json::json!(6));
        assert_eq!(m["message"], serde_json::json!("message"));

        // PRI's domain ends at 191; `<999>` is not a priority and must not
        // decode into a facility of 124.
        let over = "<999>2026-10-16T09:06:58Z pbx app[7]: message";
        let m = parse_line(over, Kind::IsoSyslog).unwrap();
        assert!(m.get("severity").is_none());
        assert!(m.get("facility").is_none());
        assert_eq!(m["message"], serde_json::json!("message"));

        // systemd logs a unit's pre-exec process as `(unit)[pid]:`. The parens
        // are part of the tag: real journals carry `(ogon-ops)`, `(sd-pam)`,
        // `(udev-worker)`, and these exec errors are exactly what people grep
        // for.
        let unit = "2026-10-16T17:41:16-05:00 drai (ogon-ops)[2357880]: \
                    ogon-ops.service: Unable to locate executable";
        assert_eq!(parse_kind(unit), Some(Kind::IsoSyslog));
        let m = parse_line(unit, Kind::IsoSyslog).unwrap();
        assert_eq!(m["program"], serde_json::json!("(ogon-ops)"));
        assert_eq!(m["pid"], serde_json::json!(2357880));

        // A parenthesised tag may also appear without a pid.
        let sd_pam = "2026-10-16T17:41:17-05:00 drai (sd-pam): pam_unix session";
        let m = parse_line(sd_pam, Kind::IsoSyslog).unwrap();
        assert_eq!(m["program"], serde_json::json!("(sd-pam)"));
        assert!(m.get("pid").is_none());

        // Through the real extractor: the four-line systemd repro is four
        // records, not two with the failures glued onto "Starting".
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("journal");
        std::fs::write(
            &p,
            "2026-10-16T17:41:15-05:00 drai systemd[1]: Starting ogon-ops.service\n\
             2026-10-16T17:41:16-05:00 drai (ogon-ops)[2357880]: ogon-ops.service: Unable to locate executable\n\
             2026-10-16T17:41:16-05:00 drai (ogon-ops)[2357880]: ogon-ops.service: Failed at step EXEC\n\
             2026-10-16T17:41:17-05:00 drai systemd[1]: ogon-ops.service: Main process exited\n",
        )
        .unwrap();
        let mut recs = Vec::new();
        extract(&p, false, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(recs.len(), 4, "every journal line is a record");
        assert!(recs.iter().all(|r| r.fields.get("message").is_some()));
        assert_eq!(
            recs[1].fields["program"],
            serde_json::json!("(ogon-ops)"),
            "the exec failure stays its own record, attributed to its unit"
        );
    }

    /// #1504: vsftpd/wu-ftpd `xferlog` — 14 fields, single letters spelled out.
    #[test]
    fn xferlog_lines_parse_to_fields() {
        let out =
            "Fri Oct 16 09:06:58 2026 1 10.0.0.7 4242 /uploads/fw.bin b _ o r admin ftp 0 * c";
        assert_eq!(parse_kind(out), Some(Kind::Xferlog));
        let m = parse_line(out, Kind::Xferlog).unwrap();
        assert_eq!(m["ts"], serde_json::json!("Fri Oct 16 09:06:58 2026"));
        assert_eq!(m["duration_s"], serde_json::json!(1));
        assert_eq!(m["host"], serde_json::json!("10.0.0.7"));
        assert_eq!(m["bytes"], serde_json::json!(4242));
        assert_eq!(m["path"], serde_json::json!("/uploads/fw.bin"));
        assert_eq!(m["transfer_type"], serde_json::json!("binary"));
        assert!(m.get("action").is_none(), "`_` is the no-action sentinel");
        assert_eq!(m["direction"], serde_json::json!("outgoing"));
        assert_eq!(m["access"], serde_json::json!("real"));
        assert_eq!(m["user"], serde_json::json!("admin"));
        assert_eq!(m["service"], serde_json::json!("ftp"));
        assert_eq!(m["auth_method"], serde_json::json!("none"));
        assert!(
            m.get("auth_user").is_none(),
            "`*` is the no-rfc931 sentinel"
        );
        assert_eq!(m["status"], serde_json::json!("complete"));

        let anon_in =
            "Sat Oct 17 11:00:00 2026 0 10.0.0.9 0 /pub/README a C i a guest ftp 1 guest c";
        assert_eq!(parse_kind(anon_in), Some(Kind::Xferlog));
        let m = parse_line(anon_in, Kind::Xferlog).unwrap();
        assert_eq!(m["transfer_type"], serde_json::json!("ascii"));
        assert_eq!(m["action"], serde_json::json!("compressed"));
        assert_eq!(m["direction"], serde_json::json!("incoming"));
        assert_eq!(m["access"], serde_json::json!("anonymous"));
        assert_eq!(m["auth_method"], serde_json::json!("rfc931"));
        assert_eq!(m["auth_user"], serde_json::json!("guest"));

        // wu-ftpd writes `g` for guest access; ProFTPD writes `d` for delete.
        let guest_delete =
            "Sun Oct 18 08:30:00 2026 0 10.0.0.9 0 /pub/upload b _ d g guest ftp 0 * c";
        assert_eq!(parse_kind(guest_delete), Some(Kind::Xferlog));
        let m = parse_line(guest_delete, Kind::Xferlog).unwrap();
        assert_eq!(m["access"], serde_json::json!("guest"));
        assert_eq!(m["direction"], serde_json::json!("delete"));
        assert_eq!(m["user"], serde_json::json!("guest"));
    }

    /// vsftpd writes the transferred filename verbatim, spaces included, so
    /// the path is not one whitespace-delimited token. A `\S+` path made such
    /// a line miss the template, and under an elected `Xferlog` it was glued
    /// onto the previous transfer as a `message`: the transfer vanished as a
    /// record of its own.
    #[test]
    fn xferlog_path_with_spaces_is_its_own_record() {
        let spaced =
            "Fri Oct 16 09:07:12 2026 3 10.0.0.7 2048 /uploads/phone config v2.xml b _ i r admin ftp 0 * c";
        assert_eq!(parse_kind(spaced), Some(Kind::Xferlog));
        let m = parse_line(spaced, Kind::Xferlog).unwrap();
        assert_eq!(m["path"], serde_json::json!("/uploads/phone config v2.xml"));
        assert_eq!(m["bytes"], serde_json::json!(2048));
        assert_eq!(m["direction"], serde_json::json!("incoming"));
        assert_eq!(m["user"], serde_json::json!("admin"));
        assert_eq!(m["status"], serde_json::json!("complete"));

        // Through the real extractor: three transfers, three records.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("xferlog");
        std::fs::write(
            &p,
            "Fri Oct 16 09:06:58 2026 1 10.0.0.7 4242 /uploads/fw.bin b _ o r admin ftp 0 * c\n\
             Fri Oct 16 09:07:12 2026 3 10.0.0.7 2048 /uploads/phone config v2.xml b _ i r admin ftp 0 * c\n\
             Sat Oct 17 11:00:00 2026 0 10.0.0.9 0 /pub/README b _ i a guest ftp 0 * c\n",
        )
        .unwrap();
        let mut recs = Vec::new();
        extract(&p, false, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(recs.len(), 3, "every transfer is a record");
        assert!(recs.iter().all(|r| r.fields.get("message").is_none()));
        assert_eq!(
            recs[1].fields["path"],
            serde_json::json!("/uploads/phone config v2.xml")
        );
    }

    /// The detection side must not claim ISO-stamped prose. Each line below
    /// was a false positive found in the #1504 review; the chosen rule is
    /// stated in the `isosyslog` template comment (single spaces, tag starts
    /// with a letter or paren, pid bracket or a bare kernel/security tag).
    #[test]
    fn iso_stamped_prose_is_not_a_log_line() {
        // One-token prose: no host AND tag.
        assert!(!probe_line("2026-10-16T09:06:58 notes from the retro"));
        assert!(!probe_line("2026-10-16T09:06:58 update: we shipped rc.96"));
        // Two-word prose: host=`retro`, tag=`notes` — an English word is not
        // a syslog tag unless it is one of the pidless kernel/security tags.
        assert!(!probe_line(
            "2026-10-16T09:06:58 retro notes: we shipped rc.96"
        ));
        // TSV: tabs are not the syslog field separator.
        assert!(!probe_line(
            "2026-10-16T09:06:58Z\tGET\thttps://example.com/a\t200"
        ));
        // zap dev log, tab-separated: `INFO`/`app/main.go` are not host/tag.
        assert!(!probe_line(
            "2026-10-16T09:06:58.123+0200\tINFO\tapp/main.go:42\tstarted"
        ));
        // ELB access log: the token after the timestamp is `IPv4:port`, which
        // starts with a digit and is not a tag.
        assert!(!probe_line(
            "2015-05-13T23:39:43.945958Z my-lb 192.168.131.39:2817 10.0.0.1:80 0.000022 0.086224"
        ));
        // `git log --format='%aI %h %s'`: host=sha, tag=`feat`, no pid.
        assert!(!probe_line("2026-10-16T09:06:58+02:00 a1b2c3d feat: add x"));
        // RFC3164 lines still probe true (no regression).
        assert!(probe_line("Oct 16 09:06:58 pbx syslogd: restart."));
        // Bare tags syslog(3) writes without LOG_PID still probe true.
        assert!(probe_line("2026-10-16T09:06:58Z pbx kernel: link up"));
        assert!(probe_line(
            "2026-10-16T09:06:58Z pbx sudo: pam_unix(sudo:session): opened"
        ));
    }

    /// Both #1504 templates outrank the loose RFC3164 pattern on a tie, so a
    /// file carrying both kinds parses the tighter one.
    #[test]
    fn iso_syslog_and_xferlog_outrank_rfc3164_on_a_tie() {
        let votes: std::collections::HashMap<Kind, usize> =
            [(Kind::Syslog, 5), (Kind::IsoSyslog, 5)]
                .into_iter()
                .collect();
        assert_eq!(elect_kind(&votes), Some(Kind::IsoSyslog));
        let votes: std::collections::HashMap<Kind, usize> =
            [(Kind::IsoSyslog, 5), (Kind::Xferlog, 5)]
                .into_iter()
                .collect();
        assert_eq!(elect_kind(&votes), Some(Kind::Xferlog));
    }

    /// #1504, still open after the ISO/xferlog templates: desk-phone syslog
    /// is `<PRI>Mon dd hh:mm:ss host tag[pid]: msg` (often with a space
    /// before the pid bracket), and modern syslog is RFC 5424. Neither
    /// matched, so the file fell through to `txt-prose` and the lines were
    /// merged into chunks with no timestamp, host or severity.
    #[test]
    fn pri_rfc3164_and_rfc5424_are_one_record_per_line() {
        let phone = "<131>Oct 16 09:06:58 192.168.1.20 SIP [990]: registration failed";
        assert_eq!(parse_kind(phone), Some(Kind::Syslog));
        let m = parse_line(phone, Kind::Syslog).unwrap();
        assert_eq!(m["ts"], serde_json::json!("Oct 16 09:06:58"));
        assert_eq!(m["host"], serde_json::json!("192.168.1.20"));
        assert_eq!(m["program"], serde_json::json!("SIP"));
        assert_eq!(m["pid"], serde_json::json!(990));
        // 131 = facility 16, severity 3 (err). Not invented when PRI is absent.
        assert_eq!(m["severity"], serde_json::json!("err"));
        assert_eq!(m["facility"], serde_json::json!(16));
        assert_eq!(m["message"], serde_json::json!("registration failed"));

        let classic = "Oct 16 09:06:58 pbx syslogd: restart.";
        let m = parse_line(classic, Kind::Syslog).unwrap();
        assert!(
            m.get("severity").is_none(),
            "a line with no <PRI> does not grow a severity"
        );
        assert_eq!(m["program"], serde_json::json!("syslogd"));

        let over = "<999>Oct 16 09:06:58 phone sipd[1]: ignored priority";
        let m = parse_line(over, Kind::Syslog).unwrap();
        assert!(m.get("severity").is_none());
        assert!(m.get("facility").is_none());

        let rfc =
            "<134>1 2026-10-16T09:06:58.123Z 10.0.0.50 sipd 4321 - - SIP engine not registered";
        assert_eq!(parse_kind(rfc), Some(Kind::Rfc5424));
        let m = parse_line(rfc, Kind::Rfc5424).unwrap();
        assert_eq!(m["ts"], serde_json::json!("2026-10-16T09:06:58.123Z"));
        assert_eq!(m["host"], serde_json::json!("10.0.0.50"));
        assert_eq!(m["program"], serde_json::json!("sipd"));
        assert_eq!(m["pid"], serde_json::json!(4321));
        // 134 = facility 16, severity 6 (info).
        assert_eq!(m["severity"], serde_json::json!("info"));
        assert_eq!(m["message"], serde_json::json!("SIP engine not registered"));
        assert!(m.get("msgid").is_none(), "`-` msgid is omitted");

        let sd = "<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"Application\" eventID=\"1011\"] An application event log entry";
        assert_eq!(parse_kind(sd), Some(Kind::Rfc5424));
        let m = parse_line(sd, Kind::Rfc5424).unwrap();
        assert_eq!(m["program"], serde_json::json!("evntslog"));
        assert!(m.get("pid").is_none(), "`-` procid is omitted");
        assert_eq!(m["msgid"], serde_json::json!("ID47"));
        assert_eq!(
            m["message"],
            serde_json::json!("An application event log entry")
        );

        // PRI-prefixed ISO syslog stays the ISO template, not RFC5424: there
        // is no version digit between the priority and the timestamp.
        let iso = "<50>2026-10-16T09:06:58Z pbx app[7]: message";
        assert_eq!(parse_kind(iso), Some(Kind::IsoSyslog));

        // Not a log: a priority-looking prefix without the timestamp shape.
        assert!(!probe_line("<134> this is not a timestamp host prog: msg"));
        assert!(!probe_line("Oct 16 the notes from the retro"));

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("phones.log");
        std::fs::write(
            &p,
            "<131>Oct 16 09:06:58 192.168.1.20 SIP [990]: registration failed\n\
             <134>Oct 16 09:07:01 192.168.1.20 SIP [990]: call cleared\n\
             <131>Oct 16 09:07:02 192.168.1.21 SIP[991]: SIP engine not registered\n\
             Oct 16 09:07:03 pbx pbx-phone[990]: watchdog reconnect\n",
        )
        .unwrap();
        let mut recs = Vec::new();
        extract(&p, false, None, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        assert_eq!(recs.len(), 4, "each phone line is its own record");
        assert!(recs.iter().all(|r| r.fields.get("ts").is_some()));
        assert!(recs.iter().all(|r| r.fields.get("host").is_some()));
        assert_eq!(recs[0].fields["severity"], serde_json::json!("err"));
        assert!(
            recs[3].fields.get("severity").is_none(),
            "the line without <PRI> does not inherit one"
        );
        assert_eq!(
            recs[2].fields["message"],
            serde_json::json!("SIP engine not registered")
        );
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
