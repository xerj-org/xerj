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

use super::{emit_document_with_fields, read_whole, ExtractStats, FieldOrigin, RawRecord, Sink};
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

/// Bare addresses kept per address header (`email_to_address`, …). A mailing
/// list blast can name thousands of recipients; the full header text stays in
/// `email_to`, and the filterable list is bounded like every other per-message
/// list here.
const MAX_ADDRESSES: usize = 1024;

/// Characters of the subject repeated at the head of the searchable body. The
/// whole subject is always in `email_subject`; this bounds what a pathological
/// 100 KB subject can add to the first section.
const SUBJECT_IN_BODY_CHARS: usize = 512;

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

/// An unstructured header mail-parser has no registered grammar for, decoded.
///
/// The default parser leaves such headers RAW, and Gmail writes a non-ASCII
/// label as an RFC 2047 encoded-word (`=?UTF-8?B?…?=`). Registering a custom
/// grammar on the parser is not an option: a non-empty `header_map` REPLACES
/// the built-in dispatch, and `From`/`To`/`Date` silently stop being parsed.
/// `header_as` re-reads just this header's bytes as text instead.
fn text_header(msg: &mail_parser::Message<'_>, name: &'static str) -> Option<String> {
    msg.header_as(name, mail_parser::HeaderForm::Text)
        .into_iter()
        .find_map(|v| v.as_text().map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
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
    // Raw 8-bit bytes in the HEADER block (no RFC 2047 encoded-word, not
    // UTF-8) are what a generation of European desktop clients wrote. The
    // parser reads headers as UTF-8, so `Grüße` became `Gr\u{fffd}\u{fffd}e`
    // and `Zoë Müller` lost her name — while the body of the same message was
    // already rescued by `undeclared_8bit_body`. Same situation, same rule:
    // UTF-8, else Windows-1252. Only the header block is re-read; the body
    // bytes are passed through untouched so every MIME offset stays valid.
    let transcoded = transcode_8bit_headers(bytes);
    let bytes = transcoded.as_deref().unwrap_or(bytes);
    let Some(msg) = MessageParser::default().parse(bytes) else {
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
    // `email_from` is the header as a person reads it — `Dana Klein
    // <dklein@amazon.com>` — and it is a keyword, so a `term` filter on the
    // bare address matches NOTHING (review finding on PR #949: the documented
    // "every message from one sender" query returned 0 hits on every mailbox
    // whose senders have display names, which is nearly all of them). The
    // bare, lower-cased addresses are what a filter is written against, so
    // they get fields of their own; the display form stays for reading.
    for (field, header) in [
        ("email_from_address", msg.from()),
        ("email_to_address", msg.to()),
        ("email_cc_address", msg.cc()),
    ] {
        let list = addr_list(header);
        if !list.is_empty() {
            headers.insert(field.into(), Value::Array(list));
        }
    }
    if let Some(d) = msg.date() {
        put(&mut headers, "email_date", d.to_rfc3339());
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
    if let Some(labels) = text_header(&msg, "X-Gmail-Labels") {
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
    if let Some(thread) = text_header(&msg, "X-GM-THRID") {
        // Gmail writes the conversation id in decimal; its own API and URLs
        // (`#all/18c0f2a5b3d4e6f7`) use the same number in hex. Hex is kept
        // because it is the form a user can paste back into Gmail, and because
        // a column of 19-digit decimals is inferred as `long`: the id is a
        // u64, and `coerce_value` sends a digit string past i64::MAX through
        // f64, where it SATURATES — every such thread would silently share
        // one id. A hex string is never inferred as a number.
        let id = thread
            .parse::<u64>()
            .map(|n| format!("{n:x}"))
            .unwrap_or(thread);
        put(&mut headers, "email_thread_id", id);
    }

    let title = if subject.is_empty() {
        "(no subject)".to_string()
    } else {
        subject.clone()
    };

    // ── message body: prefer decoded text/plain, else strip the HTML part ──
    let body = undeclared_8bit_body(&msg)
        .or_else(|| msg.body_text(0).map(|c| c.to_string()))
        .map(|t| unix_eol(&t))
        .or_else(|| msg.body_html(0).map(|h| strip_html(&h)))
        .unwrap_or_default();

    // Nothing at all: no header this extractor reads, no body text, no parts.
    // mail-parser accepts a lone blank line as a (header-less, body-less)
    // message, which is what a separator-only mbox entry or a truncated file
    // hands it — and "(no subject)" with an empty body is not a document, it
    // is a hole in every result list. The caller's fallback decides: a file is
    // re-read as prose, an mbox entry with no text is junk.
    //
    // Tested BEFORE the container's fallback date is applied: that date comes
    // from the mbox separator, not from the message, and every mbox entry has
    // one — counting it would make an empty entry look like it had a header.
    if headers.is_empty() && body.trim().is_empty() && msg.attachments().next().is_none() {
        return MessageOutcome::Unparseable;
    }
    if !headers.contains_key("email_date") {
        if let Some(fallback) = env.fallback_date.as_deref() {
            put(&mut headers, "email_date", fallback.to_string());
        }
    }

    // The subject opens the searchable text. `title` and `email_subject` are
    // inferred as KEYWORD on a mailbox (few distinct values per record: every
    // section and attachment of a thread repeats them), and a keyword matches
    // only as a whole — so `xerj search Rechnung` and `match` on any field
    // found nothing for a word that was only in a Subject, which is the first
    // place a person looks (review finding on PR #949). A subject is a line of
    // the message a reader sees above the body, so that is where it goes: the
    // first paragraph of the first section, once, never on attachments.
    let searchable = with_subject(&subject, body.trim());

    // Emit the message itself as document section(s) carrying the headers.
    if !emit_document_with_fields(
        &headers,
        &title,
        &searchable,
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
            let text = unix_eol(&String::from_utf8_lossy(data));
            emit_document_with_fields(
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
        // The PDF extractor titles a page after its file's stem, and the file
        // here is a temp file: every page was titled `.tmpXXXXXX`, a different
        // name on every run. The attachment has a real name — the same title a
        // text attachment gets — and it makes the record stream deterministic.
        if let Some(name) = link.get("attachment_name") {
            rec.fields.insert("title".into(), name.clone());
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
        "email_from_address",
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

/// The bare addresses of an address header, lower-cased, in header order,
/// without repeats — the form a `term` filter is written against. The domain
/// is case-insensitive by definition and the local part is in practice; a
/// filter that had to reproduce the sender's capitalisation would miss.
fn addr_list(a: Option<&mail_parser::Address>) -> Vec<Value> {
    let Some(a) = a else { return Vec::new() };
    let mut out: Vec<Value> = Vec::new();
    for addr in a.iter() {
        if out.len() >= MAX_ADDRESSES {
            break;
        }
        let Some(email) = addr.address() else {
            continue;
        };
        let email = email.trim().to_lowercase();
        if email.is_empty() {
            continue;
        }
        let v = Value::String(email);
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// `subject` + blank line + `body`: the text a message is searched by. Cut by
/// CHARACTERS, never bytes (panic = abort, and subjects are where CJK lives).
fn with_subject(subject: &str, body: &str) -> String {
    if subject.is_empty() {
        return body.to_string();
    }
    let head: String = subject.chars().take(SUBJECT_IN_BODY_CHARS).collect();
    if body.is_empty() {
        head
    } else {
        format!("{head}\n\n{body}")
    }
}

/// The message with an undeclared 8-bit HEADER BLOCK re-read as Windows-1252,
/// or `None` when the block is already UTF-8 (the overwhelmingly common case,
/// which costs one validation pass over the headers and no copy).
///
/// The block ends at the first empty line. Everything after it is copied
/// byte for byte, so part offsets inside the body keep their meaning.
fn transcode_8bit_headers(bytes: &[u8]) -> Option<Vec<u8>> {
    let end = header_block_end(bytes);
    let head = bytes.get(..end)?;
    if std::str::from_utf8(head).is_ok() {
        return None;
    }
    let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(head);
    let mut out = Vec::with_capacity(text.len() + bytes.len().saturating_sub(end));
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(bytes.get(end..)?);
    Some(out)
}

/// Index just past the header block: the first `\n` that is followed by an
/// empty line (`\n` or `\r\n`), or the whole input when there is none.
fn header_block_end(bytes: &[u8]) -> usize {
    // An input that OPENS with an empty line has no header block at all; what
    // follows is body, and the body has its own rule.
    if matches!(bytes, [b'\n', ..] | [b'\r', b'\n', ..]) {
        return 0;
    }
    let mut from = 0usize;
    while let Some(i) = memchr::memchr(b'\n', bytes.get(from..).unwrap_or(&[])) {
        let nl = from + i;
        match bytes.get(nl + 1..) {
            Some([b'\n', ..]) | Some([b'\r', b'\n', ..]) => return nl + 1,
            _ => from = nl + 1,
        }
    }
    bytes.len()
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

/// The text body re-decoded from its RAW bytes — only when the sender declared
/// no charset at all AND the bytes are not UTF-8.
///
/// With nothing declared, mail-parser reads such a body as UTF-8 and every
/// 8-bit byte becomes U+FFFD: `café` is indexed as `caf`, `€420` loses its
/// sign, and neither is ever found again. That is what pre-MIME mail and a
/// generation of desktop clients wrote, and it is what sits in the old end of
/// a lifelong mailbox. autoindex already has a rule for undeclared 8-bit TEXT
/// FILES — UTF-8, else Windows-1252 (`sniff::decode`) — and this applies the
/// same rule to the same situation.
///
/// Narrow on purpose. A DECLARED charset is the sender's word and is left to
/// the parser; a quoted-printable or base64 part is left alone too, because
/// its bytes in `raw_message` are the ENCODED form. Windows-1252 is right for
/// Western European mail and wrong for undeclared Cyrillic or CJK legacy
/// encodings, which are not detected — those were unreadable before this and
/// still are, which is documented rather than hidden.
fn undeclared_8bit_body(msg: &mail_parser::Message<'_>) -> Option<String> {
    let id = *msg.text_body.first()?;
    let part = msg.parts.get(usize::try_from(id).ok()?)?;
    // `text_body` can name an HTML part (the parser renders it to text on
    // request); only a real text/plain body is re-read from its bytes.
    if !matches!(part.body, PartType::Text(_)) || part.encoding != mail_parser::Encoding::None {
        return None;
    }
    if part
        .content_type()
        .is_some_and(|c| c.attribute("charset").is_some())
    {
        return None;
    }
    // Offsets come from the parser and index the same buffer; `get` rather
    // than slicing anyway — this is untrusted input under panic = abort.
    let raw = msg
        .raw_message
        .get(usize::try_from(part.offset_body).ok()?..usize::try_from(part.offset_end).ok()?)?;
    if std::str::from_utf8(raw).is_ok() {
        return None;
    }
    // No BOM handling: this is a mail body, not a file, and `FF FE` here is two
    // bytes of text.
    let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(raw);
    Some(text.into_owned())
}

/// CRLF → LF. Mail is CRLF on the wire (RFC 5322 §2.1) and that is how most
/// mailboxes store it, but it is transport framing, not content — and the
/// section splitter finds paragraphs by `\n\n`, which `\r\n\r\n` does not
/// contain. Left alone, every long CRLF body is ONE paragraph, so it is cut
/// hard at the size limit, mid-word and with no overlap, instead of between
/// paragraphs. Only the pair is rewritten; a lone `\r` is left as the sender
/// wrote it.
fn unix_eol(text: &str) -> String {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    }
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

    /// Gmail's conversation id is a u64 written in decimal. Left as a digit
    /// string it is inferred `long`, and ids past i64::MAX saturate onto ONE
    /// value in coercion — so it is stored as hex, the form Gmail's URLs use.
    #[test]
    fn gmail_thread_ids_stay_distinct_past_i64_max() {
        let thread = |thrid: &str| -> Option<String> {
            let eml = format!(
                "From: a@x.org\r\nTo: b@x.org\r\nSubject: t\r\nMessage-ID: <t@x.org>\r\n\
                 X-GM-THRID: {thrid}\r\n\r\nbody\r\n"
            );
            let recs = run(eml.as_bytes());
            field(&recs[0], "email_thread_id").map(str::to_string)
        };
        assert_eq!(
            thread("1787654321098765432").as_deref(),
            Some("18cf06223648b478")
        );
        // Two ids above i64::MAX (9223372036854775807) that f64 cannot tell apart.
        let a = thread("18446744073709551614");
        let b = thread("18446744073709551615");
        assert_eq!(a.as_deref(), Some("fffffffffffffffe"));
        assert_eq!(b.as_deref(), Some("ffffffffffffffff"));
        assert_ne!(a, b);
        // Not a u64 at all: kept as written rather than dropped or panicking.
        assert_eq!(
            thread("99999999999999999999999").as_deref(),
            Some("99999999999999999999999")
        );
        assert_eq!(thread("thread-設計").as_deref(), Some("thread-設計"));
    }

    /// A CRLF body must section exactly like the same body with LF endings:
    /// between paragraphs, with overlap — not hard-cut mid-word because
    /// `\r\n\r\n` hides every paragraph break from the splitter.
    #[test]
    fn a_crlf_body_sections_on_paragraphs_like_an_lf_body() {
        // Multi-byte text on purpose: a hard cut lands inside it (panic = abort).
        let para = "設計書 Überweisung مرحبا term sheet ".repeat(40);
        let body_lf = vec![para.trim_end().to_string(); 30].join("\n\n");
        let head = "From: a@x.org\nTo: b@x.org\nSubject: long\nMessage-ID: <l@x.org>\n\
                    Content-Type: text/plain; charset=utf-8\n\n";
        let lf = format!("{head}{body_lf}\n");
        let crlf = lf.replace('\n', "\r\n");
        let bodies = |eml: &str| -> Vec<String> {
            run(eml.as_bytes())
                .iter()
                .map(|r| field(r, "body").unwrap().to_string())
                .collect()
        };
        let (from_lf, from_crlf) = (bodies(&lf), bodies(&crlf));
        assert!(from_lf.len() > 1, "long enough to be sectioned at all");
        assert_eq!(
            from_crlf, from_lf,
            "line endings must not change the sections"
        );
        assert!(from_crlf.iter().all(|b| !b.contains('\r')));
        // Paragraph-aligned: every section ends where a paragraph ends.
        for b in &from_crlf {
            assert!(
                b.trim_end().ends_with("term sheet"),
                "cut mid-paragraph: …{:?}",
                b.chars().rev().take(30).collect::<String>()
            );
        }
    }

    /// Nothing in, nothing out: a blank line is not a "(no subject)" document.
    #[test]
    fn an_empty_message_is_unparseable_not_an_empty_document() {
        // Both envelopes: a standalone file, and an mbox slot — whose separator
        // ALWAYS supplies a fallback date, which must not count as "a header".
        let in_mbox = MessageEnvelope {
            loc_prefix: "m0-",
            fallback_date: Some("2024-01-01T10:00:00Z".into()),
        };
        for env in [MessageEnvelope::default(), in_mbox] {
            for bytes in [&b""[..], b"\n", b"\r\n", b"\r\n\r\n", b"   \n"] {
                let mut stats = ExtractStats::default();
                let mut got = Vec::new();
                let mut sink = |r: RawRecord| {
                    got.push(r);
                    true
                };
                let out = emit_message(bytes, &env, &mut sink, &mut stats);
                assert_eq!(out, MessageOutcome::Unparseable, "{bytes:?} {env:?}");
                assert!(got.is_empty(), "{bytes:?}");
                assert_eq!(stats.records, 0);
            }
        }
        // …but a message that has ONLY a subject, or ONLY a body, is a message.
        for bytes in [&b"Subject: hello\n\n"[..], b"\njust a body, no headers\n"] {
            let mut stats = ExtractStats::default();
            let mut sink = |_r: RawRecord| true;
            let out = emit_message(bytes, &MessageEnvelope::default(), &mut sink, &mut stats);
            assert_eq!(out, MessageOutcome::Emitted { alive: true }, "{bytes:?}");
            assert_eq!(stats.records, 1);
        }
    }

    /// Undeclared 8-bit text is read as Windows-1252, like an undeclared text
    /// FILE — not flattened to U+FFFD. A declared charset is never overridden,
    /// and neither is a transfer-encoded part.
    #[test]
    fn an_undeclared_8bit_body_is_read_as_windows_1252() {
        let head = b"From: a@x.org\nTo: b@x.org\nSubject: old mail\nMessage-ID: <o@x.org>\n";
        let body_of = |rest: &[u8]| -> String {
            let mut eml = head.to_vec();
            eml.extend_from_slice(rest);
            // The subject opens the searchable body (`with_subject`); what is
            // under test here is the decoding of the bytes after it.
            field(&run(&eml)[0], "body")
                .unwrap()
                .strip_prefix("old mail\n\n")
                .expect("the subject opens the body")
                .to_string()
        };
        // cp1252: 0x93/0x94 curly quotes, 0x80 euro, 0xe9 e-acute — with the
        // 8-bit byte as the LAST byte too, where a "tolerate a cut prefix"
        // decoder would drop it.
        let body = body_of(b"\n\x93Quoted\x94 price: \x80420 at the caf\xe9");
        assert_eq!(
            body,
            "\u{201c}Quoted\u{201d} price: \u{20ac}420 at the caf\u{e9}"
        );
        assert!(!body.contains('\u{fffd}'));

        // Valid UTF-8 with no charset stays UTF-8 (not re-read as cp1252).
        assert_eq!(body_of("\ncafé 設計".as_bytes()), "café 設計");
        // A DECLARED charset is the sender's word: latin-1 0xe9 stays é, and a
        // declared-but-wrong utf-8 is left to the parser, not second-guessed.
        assert_eq!(
            body_of(b"Content-Type: text/plain; charset=iso-8859-1\n\ncaf\xe9"),
            "café"
        );
        assert!(
            body_of(b"Content-Type: text/plain; charset=utf-8\n\ncaf\xe9").contains('\u{fffd}')
        );
        // Quoted-printable with no charset: raw bytes are the ENCODED form and
        // are valid ASCII, so the parser's decoding stands.
        assert!(
            body_of(b"Content-Transfer-Encoding: quoted-printable\n\ncaf=E9 ok").contains("ok")
        );
        // FF FE opens the body: two bytes of text, never a UTF-16 BOM.
        assert_eq!(body_of(b"\n\xff\xfeabc"), "\u{ff}\u{fe}abc");
    }

    /// Review finding (PR #949, major): the documented "every message from one
    /// sender" filter — `term email_from = alice@example.org` — returned 0 hits,
    /// because `email_from` is a keyword holding `Display Name <addr>`. The
    /// bare address now has a field of its own, lower-cased, multi-valued, and
    /// copied onto attachments so "PDFs from alice@…" is one filter too.
    #[test]
    fn senders_and_recipients_are_filterable_by_bare_address() {
        let b64 = base64::engine::general_purpose::STANDARD.encode("plain notes");
        let eml = format!(
            "From: =?utf-8?q?Chlo=C3=A9_Lef=C3=A8vre?= <Chloe@Example.COM>\r\n\
             To: Liam O'Connor <liam@example.org>, liam@example.org, \"Team, The\" <team@example.org>\r\n\
             Cc: undisclosed-recipients:;\r\n\
             Subject: addresses\r\nMessage-ID: <addr-1@example.com>\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
             --b\r\nContent-Type: text/plain\r\n\r\nhello\r\n\
             --b\r\nContent-Type: text/plain; name=\"n.txt\"\r\n\
             Content-Disposition: attachment; filename=\"n.txt\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{b64}\r\n--b--\r\n"
        );
        let recs = run(eml.as_bytes());
        let msg = recs.iter().find(|r| r.locator == "msg-s0").unwrap();
        let list = |r: &RawRecord, k: &str| -> Vec<String> {
            r.fields
                .get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        // The display form is unchanged — it is what a person reads.
        assert_eq!(
            field(msg, "email_from"),
            Some("Chloé Lefèvre <Chloe@Example.COM>")
        );
        // The filter form: bare, lower-cased, no repeats, header order.
        assert_eq!(list(msg, "email_from_address"), ["chloe@example.com"]);
        assert_eq!(
            list(msg, "email_to_address"),
            ["liam@example.org", "team@example.org"]
        );
        // An empty group has no address: no field, not an empty array.
        assert!(msg.fields.get("email_cc_address").is_none());
        // The attachment is one `term` filter away from its sender too.
        let att = recs
            .iter()
            .find(|r| field(r, "attachment_name") == Some("n.txt"))
            .unwrap();
        assert_eq!(list(att, "email_from_address"), ["chloe@example.com"]);
    }

    /// Review finding (PR #949, major): a word that exists only in a Subject
    /// was not findable by `xerj search` or any `match` query — `title` and
    /// `email_subject` are keywords on a mailbox. The subject now opens the
    /// searchable body of the message's FIRST section, and only there.
    #[test]
    fn subject_words_are_in_the_searchable_body_once() {
        // Short message: the subject is the first paragraph of the body.
        let recs = run(
            b"From: a@x.org\nSubject: Rechnung 2024-117\nMessage-ID: <s@x.org>\n\nBitte zahlen.\n",
        );
        assert_eq!(
            field(&recs[0], "body"),
            Some("Rechnung 2024-117\n\nBitte zahlen.")
        );
        assert_eq!(field(&recs[0], "email_subject"), Some("Rechnung 2024-117"));
        assert_eq!(field(&recs[0], "title"), Some("Rechnung 2024-117"));

        // Subject only: the subject IS the text. No subject: the body alone —
        // never a literal "(no subject)" in the searchable text.
        let only = run(b"Subject: nur betreff\nMessage-ID: <o@x.org>\n\n");
        assert_eq!(field(&only[0], "body"), Some("nur betreff"));
        let none = run(b"From: a@x.org\nMessage-ID: <n@x.org>\n\njust text\n");
        assert_eq!(field(&none[0], "body"), Some("just text"));

        // Long message + attachment: section 0 carries it, later sections and
        // the attachment do not (one hit per needle, not one per section).
        let para = "term sheet paragraph ".repeat(40);
        let long = vec![para.trim_end().to_string(); 30].join("\n\n");
        let b64 = base64::engine::general_purpose::STANDARD.encode("attached words");
        let eml = format!(
            "From: a@x.org\nSubject: xqsubjectneedle\nMessage-ID: <l2@x.org>\nMIME-Version: 1.0\n\
             Content-Type: multipart/mixed; boundary=\"b\"\n\n\
             --b\nContent-Type: text/plain; charset=utf-8\n\n{long}\n\
             --b\nContent-Type: text/plain; name=\"n.txt\"\n\
             Content-Disposition: attachment; filename=\"n.txt\"\n\
             Content-Transfer-Encoding: base64\n\n{b64}\n--b--\n"
        );
        let recs = run(eml.as_bytes());
        assert!(recs.len() > 2, "sectioned body plus an attachment");
        let with_needle: Vec<&str> = recs
            .iter()
            .filter(|r| field(r, "body").is_some_and(|b| b.contains("xqsubjectneedle")))
            .map(|r| r.locator.as_str())
            .collect();
        assert_eq!(with_needle, ["msg-s0"]);

        // A pathological subject is cut by characters, not bytes, and bounded.
        let huge = "設計".repeat(100_000);
        let eml = format!("Subject: {huge}\nMessage-ID: <h@x.org>\n\nbody text\n");
        let recs = run(eml.as_bytes());
        let body = field(&recs[0], "body").unwrap();
        assert!(body.starts_with("設計設計") && body.ends_with("body text"));
        assert!(
            body.chars().count() <= SUBJECT_IN_BODY_CHARS + 2 + "body text".len(),
            "subject in body not bounded: {} chars",
            body.chars().count()
        );
    }

    /// Review finding (PR #949, minor): raw 8-bit header bytes (no encoded-word,
    /// not UTF-8) were indexed as U+FFFD while the same message's body was
    /// rescued. Same rule for both now: UTF-8, else Windows-1252.
    #[test]
    fn undeclared_8bit_headers_are_read_as_windows_1252() {
        let eml = b"From: Zo\xeb M\xfcller <zoe@example.org>\nTo: b@x.org\n\
                    Subject: Gr\xfc\xdfe xqlatinsubj\nMessage-ID: <h8@x.org>\n\nK\xf6ln xqlatinbody\n";
        let recs = run(eml);
        assert_eq!(field(&recs[0], "email_subject"), Some("Grüße xqlatinsubj"));
        assert_eq!(
            field(&recs[0], "email_from"),
            Some("Zoë Müller <zoe@example.org>")
        );
        assert_eq!(
            field(&recs[0], "body"),
            Some("Grüße xqlatinsubj\n\nKöln xqlatinbody")
        );
        for r in &recs {
            assert!(!r
                .fields
                .values()
                .any(|v| v.to_string().contains('\u{fffd}')));
        }
        // Raw UTF-8 headers (RFC 6532) are already right and are not re-read.
        let utf8 = "From: Zoë Müller <zoe@example.org>\nSubject: Grüße 設計\nMessage-ID: <u8@x.org>\n\nhi\n";
        assert!(transcode_8bit_headers(utf8.as_bytes()).is_none());
        assert_eq!(
            field(&run(utf8.as_bytes())[0], "email_subject"),
            Some("Grüße 設計")
        );
        // Only the HEADER block is re-read: a body in a DECLARED charset keeps
        // its bytes, and its offsets, exactly.
        let mixed =
            b"Subject: caf\xe9\nContent-Type: text/plain; charset=iso-8859-1\n\ncaf\xe9 au lait\n";
        let recs = run(mixed);
        assert_eq!(field(&recs[0], "body"), Some("café\n\ncafé au lait"));
        // No header block at all: nothing to re-read, and no slice panics on a
        // leading blank line, a lone newline, or an empty input.
        assert_eq!(header_block_end(b"\njust a body \xe9\n\nmore"), 0);
        assert_eq!(header_block_end(b"\r\nbody"), 0);
        assert_eq!(header_block_end(b""), 0);
        assert_eq!(header_block_end(b"A: b\r\n\r\nbody"), 6);
        assert_eq!(header_block_end(b"A: b\nB: c"), 9);
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
