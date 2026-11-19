//! What `xerj search`, `xerj def` and MCP `xerj_search` say when a search
//! returns no hits (#3269).
//!
//! An index pattern that matches nothing is not an error on an ES-shaped
//! node: `POST /typo-*/_search` answers 200 with `hits: []`. An agent cannot
//! tell that from a real miss, so it gives up. These helpers turn the node's
//! index list into the sentence that tells them apart, and name the prefix
//! the caller most likely meant.
//!
//! Pure functions only: the CLI reads the node with the blocking client and
//! the MCP server with the async one, and both print the same text.

use std::collections::BTreeSet;

/// `zero_hits_reason` when the pattern matched no index holding documents.
pub const REASON_NO_INDEX: &str = "no_index";
/// `zero_hits_reason` when indices hold documents but none matched the query.
pub const REASON_NO_MATCH: &str = "no_match";

/// Most names listed in one line before the rest is counted.
const LIST_CAP: usize = 20;

/// The prefix of one index name: the text before its last `-`.
///
/// `grp-foo-docs` → `grp-foo`. A name with no `-`, or a system index
/// (leading `.`), has none.
pub fn index_prefix(name: &str) -> Option<&str> {
    if name.starts_with('.') {
        return None;
    }
    let (prefix, _) = name.rsplit_once('-')?;
    (!prefix.is_empty()).then_some(prefix)
}

/// Prefixes on the node, sorted and distinct.
///
/// Catalog prefixes are what autoindex stored and win. An index no catalog
/// prefix covers (written by another tool, or by a run that aborted before
/// its catalog documents) adds the prefix cut from its own name, so a node
/// without a catalog still names something.
pub fn node_prefixes(catalog: &[String], indices: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = catalog.iter().filter(|p| !p.is_empty()).cloned().collect();
    for name in indices {
        if name == xerj_common::AUTOINDEX_CATALOG_INDEX || name.starts_with('.') {
            continue;
        }
        if catalog.iter().any(|p| name.starts_with(&format!("{p}-"))) {
            continue;
        }
        if let Some(prefix) = index_prefix(name) {
            out.insert(prefix.to_string());
        }
    }
    out.into_iter().collect()
}

/// Lower-case alphanumerics only: `a_b`, `a-b` and `AB` compare equal.
fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let swap = prev[j] + usize::from(ca != cb);
            cur[j + 1] = swap.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The prefix the caller most likely meant, or `None` when nothing is close.
///
/// In order: the same letters spelled with other separators (`a_b` vs `ab`),
/// a prefix that holds the wanted one as whole `-` segments (`foo` inside
/// `grp-foo`) or is held by it, then the smallest edit distance within a
/// third of the wanted length. Ties go to the shorter, then the earlier name.
pub fn closest_prefix(wanted: &str, prefixes: &[String]) -> Option<String> {
    let wanted = wanted.trim().trim_end_matches("-*").trim_end_matches('*');
    let squashed = squash(wanted);
    if squashed.is_empty() {
        return None;
    }
    let dashed = format!("-{wanted}-");
    let limit = (squashed.chars().count() / 3).max(1);
    prefixes
        .iter()
        .filter(|p| p.as_str() != wanted)
        .filter_map(|p| {
            let s = squash(p);
            let rank = if s == squashed {
                (0, 0)
            } else if format!("-{p}-").contains(&dashed) || dashed.contains(&format!("-{p}-")) {
                (1, 0)
            } else {
                let d = edit_distance(&s, &squashed);
                if d > limit {
                    return None;
                }
                (2, d)
            };
            Some((rank, p.len(), p))
        })
        .min()
        .map(|(_, _, p)| p.clone())
}

fn join_capped(items: &[String]) -> String {
    if items.len() <= LIST_CAP {
        items.join(", ")
    } else {
        format!(
            "{}, … ({} more)",
            items[..LIST_CAP].join(", "),
            items.len() - LIST_CAP
        )
    }
}

/// The sentence for a pattern that matched no index holding documents.
///
/// `empty` lists indices the pattern did match that hold no documents yet;
/// empty when the pattern matched nothing. `prefixes` comes from
/// [`node_prefixes`]; an empty list says `none`.
pub fn no_index_message(
    pattern: &str,
    wanted_prefix: Option<&str>,
    empty: &[String],
    prefixes: &[String],
) -> String {
    let mut msg = if empty.is_empty() {
        format!("no index matches {pattern} — this is not a search result")
    } else {
        format!(
            "every index matching {pattern} is empty ({}) — this is not a search result; \
             an autoindex run may still be writing them",
            join_capped(empty)
        )
    };
    if prefixes.is_empty() {
        msg.push_str(
            "; prefixes on this node: none (index a folder first: xerj autoindex <folder>)",
        );
        return msg;
    }
    msg.push_str(&format!(
        "; prefixes on this node: {}",
        join_capped(prefixes)
    ));
    if let Some(closest) = wanted_prefix.and_then(|w| closest_prefix(w, prefixes)) {
        msg.push_str(&format!(
            "; closest: {closest} (search it with prefix \"{closest}\")"
        ));
    }
    msg
}

/// One retry suggestion for a query that matched nothing in indices that do
/// hold documents.
pub fn no_match_suggestion(query: &str) -> String {
    let words: Vec<&str> = query.split_whitespace().collect();
    let identifiers: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| {
            w.chars()
                .any(|c| c == '_' || c == ':' || c == '.' || c.is_ascii_uppercase())
                && w.chars().any(|c| c.is_ascii_alphabetic())
        })
        .collect();
    if words.len() > 1 && !identifiers.is_empty() {
        format!(
            "retry with only the identifier: \"{}\"",
            identifiers.join(" ")
        )
    } else if words.len() > 2 {
        "retry with two or three distinctive words, or a symbol name, instead of a sentence"
            .to_string()
    } else {
        "retry with a different spelling or a symbol name".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prefixes_come_from_the_catalog_then_from_index_names() {
        assert_eq!(index_prefix("grp-foo-docs"), Some("grp-foo"));
        assert_eq!(index_prefix("plain"), None);
        assert_eq!(index_prefix(".xerj-internal"), None);
        let got = node_prefixes(
            &v(&["sled"]),
            &v(&[
                "sled-docs",
                "sled-code-x",
                "grp-foo-docs",
                "autoindex-catalog",
                "solo",
            ]),
        );
        assert_eq!(got, v(&["grp-foo", "sled"]));
        assert!(node_prefixes(&[], &[]).is_empty());
    }

    #[test]
    fn the_closest_prefix_covers_the_reported_misses() {
        let on_node = v(&["ax", "grp-foo", "a-b", "sled"]);
        // A suffix of the real name (`foo` vs `grp-foo`).
        assert_eq!(closest_prefix("foo", &on_node).as_deref(), Some("grp-foo"));
        assert_eq!(
            closest_prefix("foo-*", &on_node).as_deref(),
            Some("grp-foo")
        );
        // An underscore dropped or added.
        assert_eq!(closest_prefix("ab", &on_node).as_deref(), Some("a-b"));
        assert_eq!(closest_prefix("a_b", &on_node).as_deref(), Some("a-b"));
        // A typo.
        assert_eq!(closest_prefix("slad", &on_node).as_deref(), Some("sled"));
        // Nothing close: no guess.
        assert_eq!(closest_prefix("zzzzzz", &on_node), None);
        // The wanted prefix itself is never its own suggestion.
        assert_eq!(closest_prefix("sled", &v(&["sled"])), None);
    }

    #[test]
    fn the_message_names_prefixes_or_none() {
        let msg = no_index_message("foo-*", Some("foo"), &[], &v(&["ax", "grp-foo"]));
        assert!(msg.starts_with("no index matches foo-*"), "{msg}");
        assert!(msg.contains("prefixes on this node: ax, grp-foo"), "{msg}");
        assert!(msg.contains("closest: grp-foo"), "{msg}");

        let none = no_index_message("nope-*", Some("nope"), &[], &[]);
        assert!(none.contains("prefixes on this node: none"), "{none}");

        let empty = no_index_message("p-*", Some("p"), &v(&["p-docs"]), &v(&["p"]));
        assert!(
            empty.contains("every index matching p-* is empty (p-docs)"),
            "{empty}"
        );
        assert!(!empty.contains("closest"), "{empty}");
    }

    #[test]
    fn a_no_match_suggestion_is_one_line() {
        assert!(no_match_suggestion("where is parse_config defined").contains("parse_config"));
        assert!(no_match_suggestion("how does the retry loop work").contains("two or three"));
        assert!(no_match_suggestion("frobnicate").contains("different spelling"));
    }
}
