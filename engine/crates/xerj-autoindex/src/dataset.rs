//! Dataset clustering: files (or per-table groups within files) merge into
//! datasets by schema fingerprint (Jaccard on field-name sets ≥ 0.7) within
//! the same format family. Path family is a NAMING hint only — no hardcoded
//! directory semantics.
//!
//! Only the DATA-derived field names take part (`Sketch::key_fields`, fed from
//! `extract::FieldOrigin`). A name the extractor invented — `defs`, `symbols`,
//! `title`, `page` — is not evidence about the file, and letting one decide
//! membership made every extractor improvement re-home files: the dataset slug
//! is an ingredient of `ids::doc_id`, so a moved file is re-indexed under a new
//! `_id` in a new index while its old document survives, unreferenced, in the
//! old one (issue #178). Files whose names are all extractor-invented (source
//! code, prose documents) therefore cluster by format family and group alone,
//! which is what their sniffed shape actually says about them.

use crate::infer::FieldAcc;
use crate::sniff::Family;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct Sketch {
    pub file_idx: usize,
    pub group: Option<String>,
    pub family: Family,
    pub fields: HashMap<String, FieldAcc>,
    /// The subset of `fields` whose NAMES were read out of the file. This, not
    /// `fields`, is the clustering key.
    pub key_fields: HashSet<String>,
    pub records: u64,
}

#[derive(Debug)]
pub struct Cluster {
    pub family: Family,
    pub group: Option<String>,
    pub members: Vec<usize>, // file indices
    pub fields: HashMap<String, FieldAcc>,
    /// Union of the members' `key_fields` — what new sketches are compared to.
    pub key_fields: HashSet<String>,
    pub records: u64,
    pub slug: String,
}

fn jaccard(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    inter as f64 / union.max(1) as f64
}

pub fn cluster(sketches: Vec<Sketch>, rels: &[String]) -> Vec<Cluster> {
    let mut clusters: Vec<Cluster> = Vec::new();
    for sk in sketches {
        let names: HashSet<&str> = sk.key_fields.iter().map(|s| s.as_str()).collect();
        let mut best: Option<(usize, f64)> = None;
        for (ci, c) in clusters.iter().enumerate() {
            if c.family != sk.family || c.group != sk.group {
                continue;
            }
            let cnames: HashSet<&str> = c.key_fields.iter().map(|s| s.as_str()).collect();
            let j = jaccard(&names, &cnames);
            let threshold = if sk.group.is_some() { 0.5 } else { 0.7 };
            if j >= threshold && best.map(|(_, bj)| j > bj).unwrap_or(true) {
                best = Some((ci, j));
            }
        }
        match best {
            Some((ci, _)) => {
                let c = &mut clusters[ci];
                c.members.push(sk.file_idx);
                c.records += sk.records;
                c.key_fields.extend(sk.key_fields);
                for (k, acc) in sk.fields {
                    match c.fields.get_mut(&k) {
                        Some(existing) => existing.merge(&acc),
                        None => {
                            c.fields.insert(k, acc);
                        }
                    }
                }
            }
            None => clusters.push(Cluster {
                family: sk.family,
                group: sk.group,
                members: vec![sk.file_idx],
                fields: sk.fields,
                key_fields: sk.key_fields,
                records: sk.records,
                slug: String::new(),
            }),
        }
    }
    assign_slugs(&mut clusters, rels);
    clusters
}

pub fn sanitize_slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn numericish_segment(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

fn path_candidate(rel: &str) -> String {
    let parts: Vec<&str> = rel.split('/').collect();
    let dirs = &parts[..parts.len().saturating_sub(1)];
    let meaningful: Vec<String> = dirs
        .iter()
        .filter(|d| !numericish_segment(d))
        .take(2)
        .map(|d| sanitize_slug(d))
        .filter(|d| !d.is_empty())
        .collect();
    meaningful.join("-")
}

fn assign_slugs(clusters: &mut [Cluster], rels: &[String]) {
    // deterministic cluster order: by first member rel
    let mut order: Vec<usize> = (0..clusters.len()).collect();
    order.sort_by_key(|&i| {
        clusters[i]
            .members
            .iter()
            .map(|&m| rels[m].clone())
            .min()
            .unwrap_or_default()
    });

    // base candidate per cluster: segment-wise longest common prefix of the
    // members' path candidates; heterogeneous members fall back to the most
    // common candidate.
    let mut bases: Vec<String> = Vec::with_capacity(clusters.len());
    for c in clusters.iter() {
        let cands: Vec<Vec<String>> = c
            .members
            .iter()
            .map(|&m| {
                path_candidate(&rels[m])
                    .split('-')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .collect();
        let mut lcp: Vec<String> = cands.first().cloned().unwrap_or_default();
        for cand in &cands[1..] {
            let n = lcp
                .iter()
                .zip(cand.iter())
                .take_while(|(a, b)| a == b)
                .count();
            lcp.truncate(n);
        }
        let mut base = if !lcp.is_empty() {
            lcp.join("-")
        } else {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for cand in &cands {
                *counts.entry(cand.join("-")).or_default() += 1;
            }
            counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map(|(k, _)| k)
                .unwrap_or_default()
        };
        if let Some(g) = &c.group {
            let gs = sanitize_slug(g);
            if !gs.is_empty() {
                if base.is_empty() {
                    base = gs;
                } else {
                    base = format!("{base}-{gs}");
                }
            }
        }
        if base.is_empty() {
            base = c.family.as_str().replace('-', "");
        }
        bases.push(base);
    }

    // collision resolution: single-file clusters get their file stem appended
    let mut by_base: HashMap<String, Vec<usize>> = HashMap::new();
    for &i in &order {
        by_base.entry(bases[i].clone()).or_default().push(i);
    }
    for idxs in by_base.values() {
        if idxs.len() < 2 {
            continue;
        }
        for &i in idxs {
            if clusters[i].members.len() == 1 {
                let rel = &rels[clusters[i].members[0]];
                let stem = rel
                    .rsplit('/')
                    .next()
                    .unwrap_or(rel)
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(rel);
                let stem = sanitize_slug(stem);
                if !stem.is_empty() && !bases[i].ends_with(&stem) {
                    let short: String = stem.chars().take(24).collect();
                    bases[i] = format!("{}-{}", bases[i], short.trim_matches('-'));
                }
            }
        }
    }

    // final dedup with -2/-3 …
    let mut used: HashSet<String> = HashSet::new();
    for &i in &order {
        let mut slug = bases[i].clone();
        let mut k = 2;
        while !used.insert(slug.clone()) {
            slug = format!("{}-{}", bases[i], k);
            k += 1;
        }
        clusters[i].slug = slug;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids;

    fn acc(value: &str) -> FieldAcc {
        let mut a = FieldAcc::default();
        a.add(&serde_json::Value::String(value.to_string()));
        a
    }

    /// A source file: every name comes from the extractor, so the clustering
    /// key is empty whether or not the parser found symbols.
    fn code_sketch(file_idx: usize, symbols: bool) -> Sketch {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), acc("f.rs"));
        fields.insert("language".to_string(), acc("rust"));
        fields.insert("body".to_string(), acc("fn main() {}"));
        if symbols {
            fields.insert("defs".to_string(), acc("function main"));
            fields.insert("symbols".to_string(), acc("main"));
            fields.insert("symbol_count".to_string(), acc("1"));
        }
        Sketch {
            file_idx,
            group: None,
            family: Family::Code,
            fields,
            key_fields: HashSet::new(),
            records: 1,
        }
    }

    /// A data file: the names are the file's own, so they are the key.
    fn data_sketch(file_idx: usize, family: Family, names: &[&str]) -> Sketch {
        let mut fields = HashMap::new();
        let mut key_fields = HashSet::new();
        for n in names {
            fields.insert((*n).to_string(), acc("v"));
            key_fields.insert((*n).to_string());
        }
        Sketch {
            file_idx,
            group: None,
            family,
            fields,
            key_fields,
            records: 1,
        }
    }

    fn doc_ids(clusters: &[Cluster], rels: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for c in clusters {
            for &m in &c.members {
                // `rel` stands in for the content key: this test changes the
                // extractor, never the files.
                out.push(ids::doc_id(&c.slug, &rels[m], "code"));
            }
        }
        out.sort();
        out
    }

    /// Regression for #178. Re-index the same corpus with an extractor that
    /// now finds symbols in files that produced none before: the documents
    /// must land on exactly the same `_id`s, so the re-index overwrites in
    /// place instead of writing a second copy beside the first.
    #[test]
    fn a_better_extractor_does_not_re_home_a_single_file() {
        let rels: Vec<String> = (0..6).map(|i| format!("src/mod{i}/f.rs")).collect();

        // Before: files 4 and 5 parsed to zero symbols.
        let before = cluster(
            (0..6).map(|i| code_sketch(i, i < 4)).collect::<Vec<_>>(),
            &rels,
        );
        // The whole point: one dataset, not one per symbol-presence variant.
        assert_eq!(before.len(), 1, "{before:#?}");
        assert_eq!(before[0].members.len(), 6);

        // After: the improved grammar finds symbols in all six.
        let after = cluster(
            (0..6).map(|i| code_sketch(i, true)).collect::<Vec<_>>(),
            &rels,
        );

        let (b, a) = (doc_ids(&before, &rels), doc_ids(&after, &rels));
        assert_eq!(a.len(), b.len(), "document count grew: {b:?} -> {a:?}");
        assert_eq!(a, b, "documents moved to new _ids: {b:?} -> {a:?}");
    }

    /// The same must hold when only SOME files change and the corpus also
    /// holds data files — the data datasets must not be disturbed either.
    #[test]
    fn a_partial_extractor_change_leaves_the_data_datasets_alone() {
        let rels: Vec<String> = vec![
            "src/a.rs".into(),
            "src/b.rs".into(),
            "data/events.csv".into(),
            "data/users.csv".into(),
        ];
        let mixed = |symbols_in_b: bool| {
            vec![
                code_sketch(0, true),
                code_sketch(1, symbols_in_b),
                data_sketch(2, Family::Csv, &["ts", "level", "msg"]),
                data_sketch(3, Family::Csv, &["id", "email", "name"]),
            ]
        };
        let before = cluster(mixed(false), &rels);
        let after = cluster(mixed(true), &rels);
        let slugs = |cs: &[Cluster]| {
            let mut v: Vec<(usize, String)> = cs
                .iter()
                .flat_map(|c| c.members.iter().map(|&m| (m, c.slug.clone())))
                .collect();
            v.sort();
            v
        };
        assert_eq!(slugs(&before), slugs(&after));
        // 1 code dataset + 2 unrelated CSV schemas.
        assert_eq!(before.len(), 3, "{before:#?}");
    }

    /// The fix must not blunt clustering: unrelated schemas stay apart, near
    /// -identical ones still merge, and formats never mix.
    #[test]
    fn genuinely_different_content_still_separates() {
        let rels: Vec<String> = (0..5).map(|i| format!("d/f{i}")).collect();
        let clusters = cluster(
            vec![
                data_sketch(0, Family::Csv, &["ts", "level", "msg"]),
                // one extra column out of four — still the same schema
                data_sketch(1, Family::Csv, &["ts", "level", "msg", "host"]),
                data_sketch(2, Family::Csv, &["id", "email", "name"]),
                // same names, different format family
                data_sketch(3, Family::Jsonl, &["ts", "level", "msg"]),
                code_sketch(4, true),
            ],
            &rels,
        );
        assert_eq!(clusters.len(), 4, "{clusters:#?}");
        let members: Vec<Vec<usize>> = clusters.iter().map(|c| c.members.clone()).collect();
        assert!(members.contains(&vec![0, 1]), "{members:?}");
        assert!(members.contains(&vec![2]), "{members:?}");
        assert!(members.contains(&vec![3]), "{members:?}");
        assert!(members.contains(&vec![4]), "{members:?}");
    }
}
