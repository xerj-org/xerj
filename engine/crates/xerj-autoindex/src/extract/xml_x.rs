//! XML — pull-parsed (quick-xml), O(depth) memory.
//! The record element is elected generically: the most frequent tag (in the
//! first 4096 start events) that carries structure (attributes or element
//! children). No repeating structured tag → the whole document is one record.

use super::{sanitize_field_name, ExtractStats, RawRecord, Sink, MAX_FIELDS_PER_RECORD};
use anyhow::Result;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;

struct State {
    record_tag: Option<String>,
    capture: Option<Map<String, Value>>,
    stack: Vec<String>,
    root_fields: Map<String, Value>,
    root_stack: Vec<String>,
    ordinal: u64,
}

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let record_tag = elect_record_tag(path, gzip)?;

    let r = super::open_reader(path, gzip, None)?;
    let mut reader = Reader::from_reader(r);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut st = State {
        record_tag,
        capture: None,
        stack: Vec::new(),
        root_fields: Map::new(),
        root_stack: Vec::new(),
        ordinal: 0,
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                handle_open(&e, false, &mut st);
            }
            Ok(Event::Empty(e)) => {
                if let Some(fields) = handle_open(&e, true, &mut st) {
                    // `<record …/>` self-closing: complete record, no End event.
                    if !fields.is_empty() {
                        stats.records += 1;
                        let loc = format!("e{}", st.ordinal);
                        st.ordinal += 1;
                        if !sink(RawRecord {
                            fields,
                            locator: loc,
                            group: None,
                        }) {
                            return Ok(stats);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if let Some(m) = st.capture.as_mut() {
                    if st.stack.is_empty() && st.record_tag.as_deref() == Some(name.as_str()) {
                        let fields = std::mem::take(m);
                        st.capture = None;
                        if !fields.is_empty() {
                            stats.records += 1;
                            let loc = format!("e{}", st.ordinal);
                            st.ordinal += 1;
                            if !sink(RawRecord {
                                fields,
                                locator: loc,
                                group: None,
                            }) {
                                return Ok(stats);
                            }
                        }
                    } else {
                        st.stack.pop();
                    }
                } else if st.record_tag.is_none() {
                    st.root_stack.pop();
                }
            }
            Ok(Event::Text(t)) => {
                let txt = t.unescape().unwrap_or_default().trim().to_string();
                if !txt.is_empty() {
                    handle_text(&txt, &mut st);
                }
            }
            Ok(Event::CData(t)) => {
                let txt = String::from_utf8_lossy(&t).trim().to_string();
                if !txt.is_empty() {
                    handle_text(&txt, &mut st);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => {
                stats.junk += 1;
                break;
            }
        }
        buf.clear();
    }

    if st.record_tag.is_none() && !st.root_fields.is_empty() {
        stats.records += 1;
        sink(RawRecord {
            fields: st.root_fields,
            locator: "doc".into(),
            group: None,
        });
    }
    Ok(stats)
}

/// Returns Some(fields) when a self-closing record element completes
/// immediately (no End event will follow).
fn handle_open(e: &BytesStart, empty: bool, st: &mut State) -> Option<Map<String, Value>> {
    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
    let attrs: Vec<(String, String)> = e
        .attributes()
        .flatten()
        .map(|a| {
            (
                String::from_utf8_lossy(a.key.as_ref()).to_string(),
                String::from_utf8_lossy(&a.value).to_string(),
            )
        })
        .collect();
    if st.capture.is_none() && st.record_tag.as_deref() == Some(name.as_str()) {
        let mut m = Map::new();
        for (k, v) in &attrs {
            insert_field(&mut m, &sanitize_field_name(k), v);
        }
        if empty {
            return Some(m);
        }
        st.capture = Some(m);
        st.stack.clear();
    } else if st.capture.is_some() {
        if !empty {
            st.stack.push(name.clone());
        }
        let mut prefix_parts = st.stack.clone();
        if empty {
            prefix_parts.push(name.clone());
        }
        let prefix = prefix_parts.join("_");
        let m = st.capture.as_mut().unwrap();
        for (k, v) in &attrs {
            let key = if prefix.is_empty() {
                sanitize_field_name(k)
            } else {
                sanitize_field_name(&format!("{prefix}_{k}"))
            };
            insert_field(m, &key, v);
        }
    } else if st.record_tag.is_none() {
        if !empty {
            st.root_stack.push(name.clone());
        }
        let mut parts: Vec<String> = if st.root_stack.len() > 1 {
            st.root_stack[1..].to_vec()
        } else {
            Vec::new()
        };
        if empty && !st.root_stack.is_empty() {
            parts.push(name.clone());
        }
        let prefix = parts.join("_");
        for (k, v) in &attrs {
            let key = if prefix.is_empty() {
                sanitize_field_name(k)
            } else {
                sanitize_field_name(&format!("{prefix}_{k}"))
            };
            insert_field(&mut st.root_fields, &key, v);
        }
    }
    None
}

fn handle_text(txt: &str, st: &mut State) {
    if let Some(m) = st.capture.as_mut() {
        let key = if st.stack.is_empty() {
            "text".to_string()
        } else {
            sanitize_field_name(&st.stack.join("_"))
        };
        insert_field(m, &key, txt);
    } else if st.record_tag.is_none() && !st.root_stack.is_empty() {
        let key = if st.root_stack.len() == 1 {
            "text".to_string()
        } else {
            sanitize_field_name(&st.root_stack[1..].join("_"))
        };
        insert_field(&mut st.root_fields, &key, txt);
    }
}

/// Multi-valued fields become arrays.
fn insert_field(m: &mut Map<String, Value>, key: &str, v: &str) {
    if m.len() >= MAX_FIELDS_PER_RECORD && !m.contains_key(key) {
        return;
    }
    match m.get_mut(key) {
        None => {
            m.insert(key.to_string(), Value::String(v.to_string()));
        }
        Some(Value::Array(a)) => a.push(Value::String(v.to_string())),
        Some(prev) => {
            let old = prev.take();
            *prev = Value::Array(vec![old, Value::String(v.to_string())]);
        }
    }
}

fn elect_record_tag(path: &Path, gzip: bool) -> Result<Option<String>> {
    let r = super::open_reader(path, gzip, Some(4 << 20))?;
    let mut reader = Reader::from_reader(r);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    // tag -> (count, has_structure)
    let mut counts: HashMap<String, (usize, bool)> = HashMap::new();
    let mut parents: Vec<String> = Vec::new();
    let mut seen = 0usize;
    loop {
        let ev = reader.read_event_into(&mut buf);
        let (name, has_attr, empty) = match &ev {
            Ok(Event::Start(e)) => (
                String::from_utf8_lossy(e.name().as_ref()).to_string(),
                e.attributes().flatten().next().is_some(),
                false,
            ),
            Ok(Event::Empty(e)) => (
                String::from_utf8_lossy(e.name().as_ref()).to_string(),
                e.attributes().flatten().next().is_some(),
                true,
            ),
            Ok(Event::End(_)) => {
                parents.pop();
                buf.clear();
                continue;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {
                buf.clear();
                continue;
            }
        };
        if !parents.is_empty() {
            let entry = counts.entry(name.clone()).or_insert((0, false));
            entry.0 += 1;
            entry.1 |= has_attr;
            if let Some(p) = parents.last() {
                if let Some(pe) = counts.get_mut(p) {
                    pe.1 = true;
                }
            }
        }
        if !empty {
            parents.push(name);
        }
        seen += 1;
        if seen >= 4096 {
            break;
        }
        buf.clear();
    }
    Ok(counts
        .into_iter()
        .filter(|(_, (n, structured))| *n >= 3 && *structured)
        .max_by_key(|(_, (n, _))| *n)
        .map(|(k, _)| k))
}
