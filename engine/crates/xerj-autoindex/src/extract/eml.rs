//! Email (RFC 5322 / MIME) extraction.
//!
//! An `.eml` (or any `message/rfc822`) file used to fall through to the prose
//! extractor: its headers were dumped as body text, its MIME structure was left
//! raw, and a base64 attachment was indexed verbatim as gibberish — one message
//! exploding into many noise records. This parses the message properly and emits:
//!
//! - ONE (or, for a long body, a few section) record(s) for the MESSAGE itself,
//!   carrying decoded header fields (`email_from`/`email_to`/`email_subject`/
//!   `email_date`/`email_message_id`/…) and the decoded plain-text body.
//! - ONE OR MORE records PER ATTACHMENT, indexed SEPARATELY and linked back to
//!   the parent by `email_message_id`/`email_subject`/`email_from`. A PDF
//!   attachment is routed through the real PDF extractor (per-page records); a
//!   text attachment is sectioned as a document; any other file gets a
//!   name/type/size card so it is still findable by name.
//!
//! Header/body field names are the extractor's own vocabulary
//! (`FieldOrigin::Extractor`): present on every email, so they never move a file
//! between datasets.
//!
//! The per-message work lives in [`emit_message`], which takes BYTES rather
//! than a path, so a container of messages (`extract::mbox` — a Google Takeout
//! or Thunderbird mailbox) produces records of exactly the same shape as a
//! standalone `.eml`: same fields, same locators, only namespaced by the
//! message's position in its container (`m{offset}-msg-s0` vs `msg-s0`).

use super::{
    for_each_section, read_whole, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_RECORDS_PER_FILE,
};
use anyhow::Result;
use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde_json::{Map, Value};
use std::path::Path;

/// Whole-message read cap. Mailbox exports of a single message rarely approach
/// this; a message larger than it is treated as junk rather than parsed.
pub(crate) const MAX_EML: u64 = 64 << 20;

/// A message with more parts than this has its tail dropped (reported via
/// `ExtractStats.truncated`) — a defensive bound on a pathological MIME bomb.
const MAX_ATTACHMENTS: usize = 256;

/// Bytes of an attachment we are willing to route/section. Larger attachments
/// get a name card only (their bytes are not indexed).
const MAX_ATTACH_BYTES: usize = 24 << 20;

/// `References:` ids kept per message. A thread hundreds of replies deep
/// carries one id per ancestor; the thread detector only ever needs the
/// nearest resolvable one, and the tail of the list is the nearest.
const MAX_REFERENCES: usize = 64;

/// Where a message sits when it is not a file of its own.
#[derive(Debug, Default, Clone)]
pub(crate) struct MessageEnvelope<'a> {
    /// Prepended to every locator this message emits. Empty for a standalone
    /// `.eml`; `m{byte offset}-` inside an mbox, so two messages of one
    /// container can never collide on `msg-s0`.
    pub loc_prefix: &'a str,
    /// RFC 3339 delivery time from the container (the mbox `From ` line). Used
    /// for `email_date` ONLY when the message has no parseable `Date:` header
    /// of its own — the header is what the sender wrote and always wins.
    pub fallback_date: Option<String>,
}

/// What [`emit_message`] did with the bytes it was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageOutcome {
    /// Parsed and emitted. `alive` is the sink's last answer: `false` means the
    /// caller must stop extracting (phase-A sampling has what it needs).
    Emitted { alive: bool },
    /// Not parseable as a message at all; nothing was emitted. The caller
    /// decides the fallback (a file is re-read as prose, an mbox entry is
    /// indexed as raw text).
    Unparseable,
}

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let Some(bytes) = read_whole(path, gzip, MAX_EML)? else {
        stats.junk += 1;
        return Ok(stats);
    };
    match emit_message(&bytes, &MessageEnvelope::default(), sink, &mut stats) {
        // Unparseable as a message — fall back to indexing it as prose rather
        // than dropping it, matching the never-fatal contract.
        MessageOutcome::Unparseable => super::extract_as_document(path, gzip, sink),
        MessageOutcome::Emitted { .. } => Ok(stats),
    }
}

/// The message parser, built once. mail-parser leaves headers it has no
/// registered grammar for RAW — undecoded — and Gmail writes a non-ASCII label
/// as an RFC 2047 encoded-word (`=?UTF-8?B?…?=`), so the two Gmail headers read
/// below are registered as unstructured text.
fn parser() -> &'static MessageParser {
    static PARSER: std::sync::OnceLock<MessageParser> = std::sync::OnceLock::new();
    PARSER.get_or_init(|| {
        MessageParser::default()
            .header_text("X-Gmail-Labels")
            .header_text("X-GM-THRID")
    })
}

/// Parse ONE message from `bytes` and emit its message + attachment records.
///
/// Shared by the `.eml` path above and the mbox container (`extract::mbox`),
/// which is what keeps the two record shapes identical.
pub(crate) fn emit_message(
    bytes: &[u8],
    env: &MessageEnvelope<'_>,
    sink: Sink,
    stats: &mut ExtractStats,
) -> MessageOutcome {
    let Some(msg) = parser().parse(bytes) else {
        return MessageOutcome::Unparseable;
    };
    let pre = env.loc_prefix;

    // ── message header fields (decoded: encoded-words, charset) ──
    let subject = msg.subject().unwrap_or("").trim().to_string();
    let mut headers = Map::new();
    put(&mut headers, "email_subject", subject.clone());
    put(&mut headers, "email_from", addr_string(msg.from()));
    put(&mut headers, "email_to", addr_string(msg.to()));
    put(&mut headers, "email_cc", addr_string(msg.cc()));
    match (msg.date(), env.fallback_date.as_deref()) {
        (Some(d), _) => put(&mut headers, "email_date", d.to_rfc3339()),
        (None, Some(fallback)) => put(&mut headers, "email_date", fallback.to_string()),
        (None, None) => {}
    }
    if let Some(id) = msg.message_id() {
        put(&mut headers, "email_message_id", id.to_string());
    }
    if let Some(irt) = msg.in_reply_to().as_text() {
        put(&mut headers, "email_in_reply_to", irt.to_string());
    }
    // `References:` — the ancestor chain, oldest first. Kept as a list so the
    // thread detector can still resolve a parent when `In-Reply-To` is absent
    // or names a message that is not in the corpus. The TAIL is kept when the
    // list is over the cap: the nearest ancestors are the useful ones.
    if let Some(refs) = msg.references().as_text_list() {
        let skip = refs.len().saturating_sub(MAX_REFERENCES);
        let ids: Vec<Value> = refs
            .iter()
            .skip(skip)
            .map(|r| r.trim())
            .filter(|r| !r.is_empty())
            .map(|r| Value::String(r.to_string()))
            .collect();
        if !ids.is_empty() {
            headers.insert("email_references".into(), Value::Array(ids));
        }
    }
    // Gmail (Google Takeout) stamps every exported message with its labels and
    // its conversation id. Labels are how a reader separates `Inbox` from
    // `Spam`/`Trash` in an "All mail Including Spam and Trash" export, so they
    // are indexed rather than used to silently drop mail. Absent everywhere
    // else, which is why both are optional fields and not part of the shape.
    if let Some(labels) = msg.header("X-Gmail-Labels").and_then(|h| h.as_text()) {
        let labels: Vec<Value> = labels
            .split(',')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| Value::String(l.to_string()))
            .collect();
        if !labels.is_empty() {
            headers.insert("email_labels".into(), Value::Array(labels));
        }
    }
    if let Some(thread) = msg.header("X-GM-THRID").and_then(|h| h.as_text()) {
        put(&mut headers, "email_thread_id", thread.trim().to_string());
    }

    let title = if subject.is_empty() {
        "(no subject)".to_string()
    } else {
        subject.clone()
    };

    // ── message body: prefer decoded text/plain, else strip the HTML part ──
    let body = msg
        .body_text(0)
        .map(|c| c.to_string())
        .or_else(|| msg.body_html(0).map(|h| strip_html(&h)))
        .unwrap_or_default();

    // Emit the message itself as document section(s) carrying the headers.
    if !emit_email_doc(
        &headers,
        &title,
        body.trim(),
        &format!("{pre}msg"),
        sink,
        stats,
    ) {
        return MessageOutcome::Emitted { alive: false };
    }

    // ── attachments — each indexed as its own document(s) ──
    for (n, part) in msg.attachments().enumerate() {
        if n >= MAX_ATTACHMENTS {
            stats.truncated = true;
            break;
        }
        let name = part
            .attachment_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("attachment-{n}"));
        let ctype = part
            .content_type()
            .map(|c| {
                let mut s = c.ctype().to_string();
                if let Some(sub) = c.subtype() {
                    s.push('/');
                    s.push_str(sub);
                }
                s
            })
            .unwrap_or_default();
        let data: &[u8] = match &part.body {
            PartType::Binary(b) | PartType::InlineBinary(b) => b.as_ref(),
            PartType::Text(t) | PartType::Html(t) => t.as_bytes(),
            _ => &[],
        };
        let link = attach_link(&headers, &name, &ctype, data.len());

        let alive = if is_pdf(&name, data) && data.len() <= MAX_ATTACH_BYTES {
            route_pdf(data, &format!("{pre}att{n}-"), &link, sink, stats)
        } else if is_texty(&ctype, data) && data.len() <= MAX_ATTACH_BYTES {
            let text = String::from_utf8_lossy(data);
            emit_email_doc(
                &link,
                &name,
                text.trim(),
                &format!("{pre}att{n}"),
                sink,
                stats,
            )
        } else {
            // Unindexable body — a name/type/size card keeps it discoverable.
            stats.records += 1;
            sink(RawRecord {
                fields: link,
                locator: format!("{pre}att{n}-card"),
                group: None,
                origin: FieldOrigin::Extractor,
            })
        };
        if !alive {
            return MessageOutcome::Emitted { alive: false };
        }
    }

    MessageOutcome::Emitted { alive: true }
}

/// Emit `body` as one or more section records, each stamped with `base_fields`
/// (message headers or attachment-link fields) plus `title`/`body`/`section`.
///
/// Returns the sink's last answer: `false` = stop extracting.
pub(crate) fn emit_email_doc(
    base_fields: &Map<String, Value>,
    title: &str,
    body: &str,
    loc_prefix: &str,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    // Collect sections first so we know whether to stamp a `section` field.
    // One past the cap is collected so that "exactly at the cap" and "over the
    // cap" are distinguishable: only the latter dropped anything (#381).
    let mut secs: Vec<String> = Vec::new();
    for_each_section(body, &mut |s| {
        secs.push(s);
        secs.len() <= MAX_RECORDS_PER_FILE
    });
    if secs.len() > MAX_RECORDS_PER_FILE {
        secs.truncate(MAX_RECORDS_PER_FILE);
        stats.truncated = true;
    }
    if secs.is_empty() {
        secs.push(String::new());
    }
    let multi = secs.len() > 1;
    for (i, sec) in secs.into_iter().enumerate() {
        let mut fields = base_fields.clone();
        fields.insert("title".into(), Value::String(title.to_string()));
        fields.insert("body".into(), Value::String(sec));
        if multi {
            fields.insert("section".into(), Value::Number((i as u64).into()));
        }
        stats.records += 1;
        if !sink(RawRecord {
            fields,
            locator: format!("{loc_prefix}-s{i}"),
            group: None,
            origin: FieldOrigin::Extractor,
        }) {
            return false;
        }
    }
    true
}

/// Route a PDF attachment's bytes through the real PDF extractor, re-tagging
/// each emitted page record with the parent-email link fields and a locator
/// namespaced to this attachment (`prefix` = `att{n}-`, or `m{offset}-att{n}-`
/// inside an mbox).
///
/// Returns the sink's last answer: `false` = stop extracting.
fn route_pdf(
    data: &[u8],
    prefix: &str,
    link: &Map<String, Value>,
    sink: Sink,
    stats: &mut ExtractStats,
) -> bool {
    use std::io::Write;
    let Ok(mut tmp) = tempfile::Builder::new().suffix(".pdf").tempfile() else {
        stats.junk += 1;
        return true;
    };
    if tmp.write_all(data).is_err() {
        stats.junk += 1;
        return true;
    }
    let mut alive = true;
    let mut pages = 0u64;
    let mut inner = |mut rec: RawRecord| -> bool {
        for (k, v) in link.iter() {
            rec.fields.entry(k.clone()).or_insert_with(|| v.clone());
        }
        rec.locator = format!("{prefix}{}", rec.locator);
        rec.origin = FieldOrigin::Extractor;
        pages += 1;
        alive = sink(rec);
        alive
    };
    // The PDF extractor increments its own record count; we count via `inner`
    // instead, so start from what it reports and keep only the delta semantics
    // simple: ignore its stats.records, trust `inner`.
    let parsed = super::pdf::extract(tmp.path(), &mut inner);
    stats.records += pages;
    if pages == 0 && alive {
        // The parser failed, timed out, or found no text layer (a scanned
        // PDF). The attachment still EXISTS, and "the contract Dana sent" has
        // to stay findable by name — so it gets the same name/type/size card
        // any other unindexable attachment gets, instead of vanishing. Counted
        // as junk only when the parser actually errored: an image-only PDF is
        // not a failure of the file.
        if parsed.is_err() {
            stats.junk += 1;
        }
        stats.records += 1;
        alive = sink(RawRecord {
            fields: link.clone(),
            locator: format!("{prefix}card"),
            group: None,
            origin: FieldOrigin::Extractor,
        });
    }
    alive
}

// ── field helpers ──────────────────────────────────────────────────────────

fn put(m: &mut Map<String, Value>, k: &str, v: String) {
    if !v.is_empty() {
        m.insert(k.into(), Value::String(v));
    }
}

/// The link fields copied onto every attachment record so a search can find,
/// e.g., "the term sheet that came from Dana Klein".
fn attach_link(
    headers: &Map<String, Value>,
    name: &str,
    ctype: &str,
    size: usize,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("attachment_name".into(), Value::String(name.to_string()));
    if !ctype.is_empty() {
        m.insert(
            "attachment_content_type".into(),
            Value::String(ctype.to_string()),
        );
    }
    m.insert(
        "attachment_bytes".into(),
        Value::Number((size as u64).into()),
    );
    // `email_labels`/`email_thread_id` exist only on Gmail exports; copying
    // them means "not in Spam" filters an attachment the same way it filters
    // the message that carried it.
    for k in [
        "email_subject",
        "email_from",
        "email_date",
        "email_message_id",
        "email_labels",
        "email_thread_id",
    ] {
        if let Some(v) = headers.get(k) {
            m.insert(k.into(), v.clone());
        }
    }
    m
}

fn addr_string(a: Option<&mail_parser::Address>) -> String {
    let Some(a) = a else { return String::new() };
    let mut out: Vec<String> = Vec::new();
    for addr in a.iter() {
        let email = addr.address().unwrap_or("");
        match addr.name() {
            Some(name) if !name.is_empty() && !email.is_empty() => {
                out.push(format!("{name} <{email}>"))
            }
            _ if !email.is_empty() => out.push(email.to_string()),
            Some(name) if !name.is_empty() => out.push(name.to_string()),
            _ => {}
        }
    }
    out.join(", ")
}

fn is_pdf(name: &str, data: &[u8]) -> bool {
    name.to_ascii_lowercase().ends_with(".pdf") || data.starts_with(b"%PDF-")
}

fn is_texty(ctype: &str, data: &[u8]) -> bool {
    if ctype.starts_with("text/")
        || ctype.contains("json")
        || ctype.contains("xml")
        || ctype.contains("csv")
    {
        return true;
    }
    // Fallback: mostly-printable UTF-8 sample. The sample is a BYTE prefix, so
    // it can end inside a multi-byte character; that is a property of where the
    // cut fell, not of the data, and must not turn a Chinese or Arabic text
    // attachment into an unindexed card. An error in the last 3 bytes of a
    // full-size sample is the cut; anywhere else it is not UTF-8.
    let sample = &data[..data.len().min(2048)];
    let utf8 = match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => {
            sample.len() == 2048 && e.error_len().is_none() && e.valid_up_to() + 3 >= sample.len()
        }
    };
    !sample.is_empty()
        && utf8
        && sample
            .iter()
            .filter(|b| **b < 9 || (**b > 13 && **b < 32))
            .count()
            * 20
            < sample.len().max(1)
}

/// Cheap HTML → text for messages that only carry an HTML part: drop tags and
/// collapse whitespace. Not a full renderer — enough to index the words.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::io::Write;

    fn run(eml: &[u8]) -> Vec<RawRecord> {
        let mut tmp = tempfile::Builder::new().suffix(".eml").tempfile().unwrap();
        tmp.write_all(eml).unwrap();
        let mut out = Vec::new();
        let mut sink = |r: RawRecord| {
            out.push(r);
            true
        };
        extract(tmp.path(), false, &mut sink).unwrap();
        out
    }

    fn field<'a>(r: &'a RawRecord, k: &str) -> Option<&'a str> {
        r.fields.get(k).and_then(|v| v.as_str())
    }

    /// Headers become fields, the body is decoded (quoted-printable + charset),
    /// and NONE of the raw MIME/base64 leaks into the message body.
    #[test]
    fn parses_headers_and_decodes_body() {
        let b64 = base64::engine::general_purpose::STANDARD.encode("secret plan attached");
        let eml = format!(
            "From: Dana Klein <dklein@amazon.com>\r\n\
             To: Alex Rivard <alex@xerj.org>\r\n\
             Subject: =?utf-8?q?Acquisition_=E2=80=94_next_steps?=\r\n\
             Date: Tue, 15 Jul 2026 09:12:00 -0700\r\n\
             Message-ID: <deal-1@amazon.com>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
             \r\n\
             --b1\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\
             \r\n\
             Alex =E2=80=94 let's move to a term sheet at $420M.\r\n\
             --b1\r\n\
             Content-Type: application/octet-stream; name=\"notes.txt\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
             \r\n\
             {b64}\r\n\
             --b1--\r\n"
        );
        let recs = run(eml.as_bytes());
        let msg = recs.iter().find(|r| r.locator.starts_with("msg-")).unwrap();
        assert_eq!(
            field(msg, "email_from"),
            Some("Dana Klein <dklein@amazon.com>")
        );
        assert_eq!(
            field(msg, "email_message_id"),
            Some("deal-1@amazon.com").or(Some("<deal-1@amazon.com>"))
        );
        // Encoded-word subject decoded; em-dash present, no `=?utf-8?` left.
        let subj = field(msg, "email_subject").unwrap();
        assert!(
            subj.contains("Acquisition") && subj.contains('\u{2014}'),
            "subj: {subj}"
        );
        // Body decoded (QP `=E2=80=94` -> em-dash), no raw MIME.
        let body = field(msg, "body").unwrap();
        assert!(body.contains("term sheet at $420M"), "body: {body}");
        assert!(
            !body.contains("Content-Transfer-Encoding") && !body.contains("--b1"),
            "MIME leaked: {body}"
        );

        // The attachment is its OWN record, decoded (not base64), linked to the email.
        let att = recs
            .iter()
            .find(|r| field(r, "attachment_name") == Some("notes.txt"))
            .unwrap();
        assert_ne!(
            att.locator, msg.locator,
            "attachment must be a separate document"
        );
        assert_eq!(
            field(att, "email_subject"),
            field(msg, "email_subject"),
            "attachment links to parent"
        );
        let att_body = field(att, "body").unwrap();
        assert!(
            att_body.contains("secret plan attached"),
            "attachment body not decoded: {att_body}"
        );
    }

    /// A non-text, non-PDF attachment gets a name/type/size card so it stays
    /// discoverable, without indexing its bytes as noise.
    #[test]
    fn binary_attachment_gets_a_card() {
        let bytes =
            base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 255, 254, 3, 9, 200]);
        let eml = format!(
            "From: a@x.org\r\nTo: b@x.org\r\nSubject: logo\r\nDate: Wed, 16 Jul 2026 10:00:00 -0700\r\n\
             Message-ID: <m2@x.org>\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"z\"\r\n\r\n\
             --z\r\nContent-Type: text/plain\r\n\r\nsee logo\r\n\
             --z\r\nContent-Type: image/png; name=\"logo.png\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"logo.png\"\r\n\r\n{bytes}\r\n--z--\r\n"
        );
        let recs = run(eml.as_bytes());
        let card = recs
            .iter()
            .find(|r| field(r, "attachment_name") == Some("logo.png"))
            .unwrap();
        assert!(
            card.locator.contains("card"),
            "png should be a name card, got {}",
            card.locator
        );
        assert!(
            card.fields.get("body").is_none(),
            "binary body must not be indexed"
        );
        assert_eq!(field(card, "attachment_content_type"), Some("image/png"));
    }
}
