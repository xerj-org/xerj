//! Dataset clustering: files (or per-table groups within files) merge into
//! datasets by schema fingerprint (Jaccard on field-name sets ≥ 0.7) within
//! the same format family. Path family is a NAMING hint only — no hardcoded
//! directory semantics.

use crate::infer::FieldAcc;
use crate::sniff::Family;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct Sketch {
    pub file_idx: usize,
    pub group: Option<String>,
    pub family: Family,
    pub fields: HashMap<String, FieldAcc>,
    pub records: u64,
}

#[derive(Debug)]
pub struct Cluster {
    pub family: Family,
    pub group: Option<String>,
    pub members: Vec<usize>, // file indices
    pub fields: HashMap<String, FieldAcc>,
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
        let names: HashSet<&str> = sk.fields.keys().map(|s| s.as_str()).collect();
        let mut best: Option<(usize, f64)> = None;
        for (ci, c) in clusters.iter().enumerate() {
            if c.family != sk.family || c.group != sk.group {
                continue;
            }
            let cnames: HashSet<&str> = c.fields.keys().map(|s| s.as_str()).collect();
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
