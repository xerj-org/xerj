//! XML — pull-parsed (quick-xml), O(depth) memory.
//! The record element is elected generically: the most frequent tag (in the
//! first 4096 start events) that carries structure (attributes or element
//! children), ties going to the outermost then the lowest-named tag. No
//! repeating structured tag → the whole document is one record.

use super::{
    sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_FIELDS_PER_RECORD,
};
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
                            origin: FieldOrigin::Data,
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
                                origin: FieldOrigin::Data,
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
            origin: FieldOrigin::Data,
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
    // tag -> (count, has_structure, shallowest depth seen)
    let mut counts: HashMap<String, (usize, bool, usize)> = HashMap::new();
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
            let depth = parents.len();
            let entry = counts.entry(name.clone()).or_insert((0, false, depth));
            entry.0 += 1;
            entry.1 |= has_attr;
            entry.2 = entry.2.min(depth);
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
    // The ordering must be TOTAL: `counts` is a HashMap, so any candidate left
    // tied would be settled by a per-map random hash seed and the same file
    // would index to a different shape on every run. Most occurrences wins;
    // a tie goes to the outermost tag (a wrapper that repeats as often as a
    // child is the record, and the child is one of its fields); a tie at the
    // same depth goes to the lowest name, which is arbitrary but stable.
    Ok(counts
        .into_iter()
        .filter(|(_, (n, structured, _))| *n >= 3 && *structured)
        .min_by(|(a_tag, (a_n, _, a_depth)), (b_tag, (b_n, _, b_depth))| {
            b_n.cmp(a_n)
                .then_with(|| a_depth.cmp(b_depth))
                .then_with(|| a_tag.cmp(b_tag))
        })
        .map(|(k, _)| k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(xml: &str) -> (ExtractStats, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.xml");
        std::fs::write(&path, xml).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, false, &mut |r| {
            recs.push(r);
            true
        })
        .unwrap();
        (stats, recs)
    }

    #[test]
    fn the_repeating_structured_element_becomes_one_record_per_occurrence() {
        let (stats, recs) = run(r#"<catalog site="shop">
                 <item id="1"><name>Widget</name></item>
                 <item id="2"><name>Gadget</name></item>
                 <item id="3"><name>Doohickey</name></item>
                 <item id="4"><name>Thing</name><price cur="USD">4.50</price></item>
               </catalog>"#);
        assert_eq!(stats.records, 4);
        assert_eq!(stats.junk, 0);
        assert_eq!(
            recs.iter().map(|r| r.locator.as_str()).collect::<Vec<_>>(),
            ["e0", "e1", "e2", "e3"]
        );
        assert_eq!(
            recs[0].fields["id"],
            serde_json::json!("1"),
            "attributes of the record element are top-level fields"
        );
        assert_eq!(
            recs[0].fields["name"],
            serde_json::json!("Widget"),
            "child text is keyed by the child element path"
        );
        assert!(
            recs[0].fields.get("site").is_none(),
            "the root element is not part of a record"
        );
        assert_eq!(recs[3].fields["price"], serde_json::json!("4.50"));
        assert_eq!(
            recs[3].fields["price_cur"],
            serde_json::json!("USD"),
            "a child's attribute is prefixed with the child's path"
        );
    }

    #[test]
    fn a_self_closing_element_is_emitted_from_its_attributes_alone() {
        let (stats, recs) =
            run(r#"<rows><row a="1" b="x"/><row a="2" b="y"/><row a="3" b="z"/></rows>"#);
        assert_eq!(stats.records, 3, "no End event must not lose the record");
        assert_eq!(recs[2].fields["a"], serde_json::json!("3"));
        assert_eq!(recs[2].fields["b"], serde_json::json!("z"));
    }

    #[test]
    fn a_child_element_repeated_inside_a_record_becomes_a_multi_valued_field() {
        let (stats, recs) = run(r#"<catalog>
                 <item id="1"><tag>red</tag><tag>blue</tag></item>
                 <item id="2"><tag>green</tag></item>
                 <item id="3"><tag>red</tag></item>
               </catalog>"#);
        assert_eq!(stats.records, 3);
        assert_eq!(recs[0].fields["tag"], serde_json::json!(["red", "blue"]));
        assert_eq!(
            recs[1].fields["tag"],
            serde_json::json!("green"),
            "a single occurrence stays a scalar"
        );
    }

    #[test]
    fn a_document_with_no_repeating_element_becomes_one_record_keyed_by_path() {
        let (stats, recs) = run(
            r#"<config><server><host>localhost</host><port>8080</port></server><debug>true</debug></config>"#,
        );
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].locator, "doc");
        assert_eq!(
            recs[0].fields["server_host"],
            serde_json::json!("localhost")
        );
        assert_eq!(recs[0].fields["server_port"], serde_json::json!("8080"));
        assert_eq!(
            recs[0].fields["debug"],
            serde_json::json!("true"),
            "the root element is stripped from every key"
        );
    }

    #[test]
    fn entities_are_unescaped_and_cdata_is_kept_verbatim() {
        let (_, recs) =
            run(r#"<doc><p>5 &lt; 10 &amp; rising</p><p><![CDATA[<b>raw</b>]]></p></doc>"#);
        let p = recs[0].fields["p"].as_array().unwrap();
        assert!(p.contains(&serde_json::json!("5 < 10 & rising")));
        assert!(p.contains(&serde_json::json!("<b>raw</b>")));
    }

    #[test]
    fn a_truncated_document_is_junk_filed_and_keeps_what_it_parsed() {
        let (stats, recs) =
            run(r#"<catalog><item id="1"><name>A</name></item><item id="2"><name>B<"#);
        assert_eq!(stats.junk, 1);
        assert_eq!(stats.records, 1);
        assert_eq!(
            recs[0].fields["item_id"],
            serde_json::json!(["1", "2"]),
            "everything read before the break is still emitted"
        );

        let (stats, recs) = run("<a><b>1</c></a>");
        assert_eq!(stats.junk, 1, "a mismatched end tag ends the parse");
        assert_eq!(recs[0].fields["b"], serde_json::json!("1"));
    }

    #[test]
    fn text_that_is_not_xml_yields_nothing_at_all() {
        let (stats, recs) = run("not xml at all, just words");
        assert_eq!(
            (stats.records, stats.junk),
            (0, 0),
            "text outside any element is dropped; the caller junk-files the file"
        );
        assert!(recs.is_empty());
    }

    /// Each election builds a fresh `HashMap`, and every fresh map gets its own
    /// random hash seed — so 200 in-process elections over one unchanged file
    /// sample 200 different iteration orders. Before the tie-break was total,
    /// this split the winner between `item` and its equally-frequent `price`
    /// child; the record COUNT held at 3 but the fields changed run to run, so
    /// re-indexing an unchanged file rewrote every document under the same
    /// locator.
    #[test]
    fn a_tied_record_tag_election_elects_the_outermost_tag_every_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tie.xml");
        std::fs::write(
            &path,
            r#"<catalog>
                 <item id="1"><price cur="USD">9.99</price></item>
                 <item id="2"><price cur="EUR">19.99</price></item>
                 <item id="3"><price cur="USD">4.50</price></item>
               </catalog>"#,
        )
        .unwrap();

        let mut shapes = std::collections::BTreeSet::new();
        for _ in 0..200 {
            assert_eq!(
                elect_record_tag(&path, false).unwrap().as_deref(),
                Some("item"),
                "the wrapper is the record; `price` is one of its fields"
            );
            let mut n = 0usize;
            let mut first = String::new();
            extract(&path, false, &mut |r| {
                if n == 0 {
                    let mut keys: Vec<&str> = r.fields.keys().map(|k| k.as_str()).collect();
                    keys.sort_unstable();
                    first = keys.join(",");
                }
                n += 1;
                true
            })
            .unwrap();
            assert_eq!(n, 3);
            shapes.insert(first);
        }
        assert_eq!(
            shapes,
            ["id,price,price_cur"]
                .into_iter()
                .map(String::from)
                .collect::<std::collections::BTreeSet<_>>(),
            "one input must yield exactly one record shape"
        );
    }

    /// Depth cannot separate sibling wrappers, so the last key has to.
    #[test]
    fn a_tie_at_the_same_depth_is_settled_by_the_lowest_tag_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("siblings.xml");
        std::fs::write(
            &path,
            r#"<catalog>
                 <beta id="1"><v>1</v></beta><alpha id="1"><v>1</v></alpha>
                 <beta id="2"><v>2</v></beta><alpha id="2"><v>2</v></alpha>
                 <beta id="3"><v>3</v></beta><alpha id="3"><v>3</v></alpha>
               </catalog>"#,
        )
        .unwrap();

        for _ in 0..200 {
            assert_eq!(
                elect_record_tag(&path, false).unwrap().as_deref(),
                Some("alpha")
            );
        }
        let (stats, recs) = run(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(stats.records, 3, "only the elected tag emits records");
        assert_eq!(recs[0].fields["id"], serde_json::json!("1"));
        assert_eq!(recs[0].fields["v"], serde_json::json!("1"));
    }
}
