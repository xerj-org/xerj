//! XML — pull-parsed (quick-xml), O(depth) memory.
//! The record element is elected generically: the most frequent tag (in the
//! first 4096 start events) that carries structure (attributes or element
//! children), ties going to the outermost then the lowest-named tag. No
//! repeating structured tag → the whole document is one record.

use super::{
    sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink, MAX_FIELDS_PER_RECORD,
    MAX_LINE,
};
use anyhow::Result;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::Reader;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;

/// Resolve one `&…;` general reference to the text it stands for.
///
/// quick-xml >= 0.38 no longer folds entity references into the surrounding
/// `Event::Text`: it emits each one as its own `Event::GeneralRef`, so a
/// consumer that wants `5 &lt; 10` to read as `5 < 10` has to reassemble the
/// text itself. Predefined XML entities and numeric character references
/// resolve here; anything else (an entity a DTD we never read would have
/// declared) yields `None` and the reference is dropped.
///
/// Dropping just the reference is deliberately *better* than what 0.36 did:
/// there, one unknown entity failed `BytesText::unescape()` for the whole node
/// and `unwrap_or_default()` threw the entire text away.
pub(super) fn resolve_general_ref(r: &BytesRef) -> Option<String> {
    match r.resolve_char_ref() {
        Ok(Some(c)) => return Some(c.to_string()),
        Ok(None) => {}
        // Out-of-range or malformed `&#…;` — not text, so contribute nothing.
        Err(_) => return None,
    }
    let name = r.decode().ok()?;
    quick_xml::escape::resolve_predefined_entity(&name).map(str::to_string)
}

/// Largest element depth the XML extractor tracks. `handle_open` clones the
/// whole element stack and joins it into a key prefix on every start event,
/// so with no depth bound a document nested N deep costs O(N) string work
/// per event, O(N^2) for the document, and the record key itself grows with
/// the depth. Past the cap the subtree is skipped (its events are consumed
/// but nothing is pushed or captured) and the rest of the file still
/// extracts. 128 is serde_json's default recursion limit and far past the
/// nesting of real documents, which rarely exceed a few dozen levels.
const MAX_XML_DEPTH: usize = 128;

struct State {
    record_tag: Option<String>,
    capture: Option<Map<String, Value>>,
    stack: Vec<String>,
    root_fields: Map<String, Value>,
    root_stack: Vec<String>,
    ordinal: u64,
    /// Depth of the over-cap subtree currently being skipped, 0 when the
    /// parse is inside the depth bound. A skipped subtree's start events
    /// increment this and its end events decrement it, so the stacks are
    /// never pushed or popped for it.
    skipped: usize,
}

pub fn extract(path: &Path, gzip: bool, sink: Sink) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let record_tag = elect_record_tag(path, gzip)?;

    let r = super::open_reader(path, gzip, None)?;
    let mut reader = Reader::from_reader(r);
    // Deliberately NOT `trim_text(true)`: the reader trims each fragment
    // independently, and `5 &lt; 10` arrives as three fragments, so trimming
    // per fragment would splice it back together as `5<10`. Trim once, at the
    // element boundary, over the reassembled text instead.
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    // Character data accumulates here and is flushed as ONE value when the
    // next structural event arrives. Text, CDATA and entity references are all
    // fragments of the same logical value; emitting them separately would turn
    // every entity-bearing field into an array of pieces.
    let mut pending = String::new();
    let mut st = State {
        record_tag,
        capture: None,
        stack: Vec::new(),
        root_fields: Map::new(),
        root_stack: Vec::new(),
        ordinal: 0,
        skipped: 0,
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if st.skipped > 0 {
                    // Inside an over-cap subtree: count the nesting, drop
                    // the text, push nothing.
                    st.skipped += 1;
                    pending.clear();
                } else if current_depth(&st) >= MAX_XML_DEPTH {
                    // B6: past the cap, skip this subtree rather than
                    // cloning a stack that grows with the depth. The
                    // pending text belongs to the parent, so flush it first.
                    flush_text(&mut pending, &mut st);
                    st.skipped = 1;
                } else {
                    flush_text(&mut pending, &mut st);
                    handle_open(&e, false, &mut st);
                }
            }
            Ok(Event::Empty(e)) => {
                if st.skipped > 0 {
                    pending.clear();
                } else {
                    flush_text(&mut pending, &mut st);
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
            }
            Ok(Event::End(e)) => {
                flush_text(&mut pending, &mut st);
                if st.skipped > 0 {
                    st.skipped -= 1;
                    buf.clear();
                    continue;
                }
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
                if st.skipped == 0 {
                    push_text(&mut pending, &t.xml10_content().unwrap_or_default());
                }
            }
            Ok(Event::GeneralRef(entity)) => {
                if st.skipped == 0 {
                    if let Some(resolved) = resolve_general_ref(&entity) {
                        push_text(&mut pending, &resolved);
                    }
                }
            }
            Ok(Event::CData(t)) => {
                if st.skipped == 0 {
                    push_text(&mut pending, &String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::Eof) => {
                flush_text(&mut pending, &mut st);
                break;
            }
            Ok(_) => {}
            Err(_) => {
                // Everything read before the break is still emitted, so the
                // text already buffered has to land before we stop.
                flush_text(&mut pending, &mut st);
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

/// Depth of the element stack a new child would extend: the record stack
/// while a record is open, the root stack for a record-less document, zero
/// while waiting for the record element (outer wrappers are not tracked, so
/// they grow nothing).
fn current_depth(st: &State) -> usize {
    if st.capture.is_some() {
        st.stack.len()
    } else if st.record_tag.is_none() {
        st.root_stack.len()
    } else {
        0
    }
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

/// Append one character-data fragment to the pending value.
///
/// SECURITY: the fragments of a single value are now concatenated rather than
/// emitted one at a time, so a document that never closes an element could
/// otherwise grow `pending` for the whole file. Stop appending at the same
/// 16 MB bound the line-oriented extractors use; no real XML text node is
/// anywhere near it.
fn push_text(pending: &mut String, fragment: &str) {
    if pending.len() < MAX_LINE {
        pending.push_str(fragment);
    }
}

/// Emit whatever character data has accumulated as one value, keyed by the
/// element path that was open while it was read — so this must run *before*
/// the structural event that triggered it mutates the stack.
fn flush_text(pending: &mut String, st: &mut State) {
    let txt = pending.trim();
    if !txt.is_empty() {
        let txt = txt.to_string();
        handle_text(&txt, st);
    }
    pending.clear();
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

    /// quick-xml >= 0.38 splits `a &amp; b` into Text / GeneralRef / Text, so a
    /// reader that emits each fragment as it arrives turns one field into an
    /// array of pieces — and, with the reader's own `trim_text`, one that has
    /// lost the spaces that separated them. Both failures are silent: the words
    /// are all still there, in the wrong shape. Character references have to
    /// come through the same path.
    #[test]
    fn text_split_around_a_reference_is_reassembled_into_one_value() {
        let (_, recs) = run(
            r#"<doc><row><v>Ben &amp; Jerry &lt;ice&gt; caf&#233; &#x21;</v></row>
               <row><v>plain</v></row><row><v>plain</v></row></doc>"#,
        );
        assert_eq!(
            recs[0].fields["v"],
            serde_json::json!("Ben & Jerry <ice> café !"),
            "one text node is one value, spaces intact, references resolved"
        );
    }

    /// An entity no DTD declared for us resolves to nothing. It must cost only
    /// itself: quick-xml 0.36's `unescape()` failed the whole node, and
    /// `unwrap_or_default()` then threw away every word around it.
    #[test]
    fn an_unresolvable_reference_costs_only_itself() {
        let (_, recs) = run(r#"<doc><row><v>before &nosuchentity; after</v></row>
               <row><v>plain</v></row><row><v>plain</v></row></doc>"#);
        assert_eq!(recs[0].fields["v"], serde_json::json!("before  after"));
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

    /// B6: `handle_open` clones the whole element stack and joins it into a
    /// key prefix on EVERY start event, and `handle_text` joins it again per
    /// text event. With no depth bound, a document nested N deep costs
    /// O(N) string work per event, O(N^2) for the document, and the record
    /// key itself grows with the depth. Keys must stay bounded. Every
    /// nested element carries an attribute, so the record is non-empty and
    /// the bound is exercised on real keys, not on an empty record.
    #[test]
    fn deeply_nested_elements_do_not_grow_record_keys_with_depth() {
        let mut xml = String::from("<r>");
        for _ in 0..2000 {
            xml.push_str("<a n=\"1\">");
        }
        xml.push_str("<v>deep</v>");
        for _ in 0..2000 {
            xml.push_str("</a>");
        }
        xml.push_str("</r>");
        let (_stats, recs) = run(&xml);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].fields["n"], serde_json::json!("1"));
        for k in recs[0].fields.keys() {
            assert!(
                k.len() < 512,
                "record key of {} chars grew with nesting depth: {k:?}",
                k.len()
            );
        }
    }

    /// The depth cap must not eat ordinary nesting: a document nested well
    /// under the cap still extracts its deepest text.
    #[test]
    fn nesting_under_the_cap_still_extracts_the_deepest_text() {
        let mut xml = String::from("<r>");
        for _ in 0..50 {
            xml.push_str("<a>");
        }
        xml.push_str("<v>deep</v>");
        for _ in 0..50 {
            xml.push_str("</a>");
        }
        xml.push_str("</r>");
        let (_stats, recs) = run(&xml);
        assert_eq!(recs.len(), 1);
        assert!(
            recs[0].fields.values().any(|v| v == "deep"),
            "the deepest text must still extract: {:?}",
            recs[0].fields
        );
    }
}
