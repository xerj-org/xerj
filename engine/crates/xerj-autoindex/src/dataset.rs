//! Dataset clustering: what a "dataset" means for a folder of files.
//!
//! Two kinds of dataset come out of a tree (#173/#196):
//!
//! **Documents.** Files whose field names are all extractor-invented (source
//! code, prose, PDFs, line-oriented text, HTML pages) carry no schema of their
//! own, so schema inference has nothing legitimate to split them on. They merge
//! into ONE dataset per *scope* — the nearest enclosing repository root
//! (a directory with a `.git` entry), or the autoindex root when the file is
//! under no repository. Splitting them per format family produced indices no
//! user recognised as datasets (#196: one Rust workspace → four indices), and
//! per-schema clustering of their incidental shapes shattered a two-repo tree
//! into hundreds (#173). One folder → one searchable corpus per repository.
//!
//! **Data.** Files with data-derived field names (`Sketch::key_fields`, fed
//! from `extract::FieldOrigin`: CSV headers, JSON keys, SQL columns) still
//! cluster by schema fingerprint (Jaccard on field-name sets ≥ 0.7) within the
//! same format family AND scope — a real schema that recurs across files is a
//! dataset. But a self-describing config blob is not: a small cluster of
//! `json`/`yaml`/`xml` files that each produced exactly one record is *demoted*
//! to the scope's document dataset and indexed as a document (title/body),
//! because its one-off key set is configuration, not a collection
//! (#173: valkey's per-command JSON files made 382 such "datasets").
//!
//! Only DATA-derived names take part in schema clustering. A name the
//! extractor invented — `defs`, `symbols`, `title`, `page` — is not evidence
//! about the file, and letting one decide membership made every extractor
//! improvement re-home files: the dataset slug is an ingredient of
//! `ids::doc_id`, so a moved file is re-indexed under a new `_id` in a new
//! index while its old document survives, unreferenced, in the old one
//! (issue #178). Scope-based document grouping strengthens that stability:
//! a document's dataset now depends only on its own path and the repository
//! layout, never on which other files exist.

use crate::infer::FieldAcc;
use crate::sniff::Family;
use std::collections::{BTreeMap, HashMap, HashSet};

/// A schema-clustered `json`/`yaml`/`xml` group at most this many files large,
/// in which every file produced exactly one record, is configuration — demote
/// it to the scope's document dataset. A schema that recurs across more files
/// than this is treated as a real single-record-per-file collection.
pub const DOC_DEMOTE_MAX_FILES: usize = 8;

#[derive(Debug)]
pub struct Sketch {
    pub file_idx: usize,
    pub group: Option<String>,
    pub family: Family,
    pub fields: HashMap<String, FieldAcc>,
    /// The subset of `fields` whose NAMES were read out of the file. This, not
    /// `fields`, is the schema-clustering key; empty means "document".
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
    /// Repository scope this cluster lives in ("" = the autoindex root).
    pub scope: String,
    /// A per-scope document dataset (code/prose/config), not a schema dataset.
    pub is_docs: bool,
    /// Members demoted from one-off config clusters. Their sampled fields were
    /// discarded (config keys are not document fields); the caller re-samples
    /// them through the document renderer and must index them as documents.
    pub demoted: Vec<usize>,
}

fn jaccard(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    inter as f64 / union.max(1) as f64
}

/// May this data cluster be demoted to documents if it stays a small fleet of
/// single-record files? Self-describing structured formats only: CSV is
/// tabular by nature, logs/jsonl are collections by nature, and sql groups
/// carry a real table schema.
fn demotable_family(f: Family) -> bool {
    matches!(f, Family::Json | Family::Yaml | Family::Xml)
}

/// Cluster sketches into datasets. `rels` and `scopes` are indexed by
/// `Sketch::file_idx`: `rels` are root-relative paths (slug naming), `scopes`
/// the repository scope of each file ("" = root) — see `module` docs.
pub fn cluster(sketches: Vec<Sketch>, rels: &[String], scopes: &[String]) -> Vec<Cluster> {
    // scope → docs cluster (BTreeMap: deterministic scope order).
    let mut docs: BTreeMap<String, Cluster> = BTreeMap::new();
    let mut data: Vec<Cluster> = Vec::new();
    fn docs_cluster<'a>(docs: &'a mut BTreeMap<String, Cluster>, scope: &str) -> &'a mut Cluster {
        docs.entry(scope.to_string()).or_insert_with(|| Cluster {
            family: Family::Code,
            group: None,
            members: Vec::new(),
            fields: HashMap::new(),
            key_fields: HashSet::new(),
            records: 0,
            slug: String::new(),
            scope: scope.to_string(),
            is_docs: true,
            demoted: Vec::new(),
        })
    }

    for sk in sketches {
        let scope = scopes[sk.file_idx].as_str();
        // Documents: no data-derived names, no sub-file group.
        if sk.key_fields.is_empty() && sk.group.is_none() {
            let c = docs_cluster(&mut docs, scope);
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
            continue;
        }
        // Data: schema clustering within (scope, family, group).
        let names: HashSet<&str> = sk.key_fields.iter().map(|s| s.as_str()).collect();
        let mut best: Option<(usize, f64)> = None;
        for (ci, c) in data.iter().enumerate() {
            if c.family != sk.family || c.group != sk.group || c.scope != scope {
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
                let c = &mut data[ci];
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
            None => data.push(Cluster {
                family: sk.family,
                group: sk.group,
                members: vec![sk.file_idx],
                fields: sk.fields,
                key_fields: sk.key_fields,
                records: sk.records,
                slug: String::new(),
                scope: scope.to_string(),
                is_docs: false,
                demoted: Vec::new(),
            }),
        }
    }

    // Demote one-off config clusters to their scope's document dataset. Their
    // sampled fields are dropped — the caller re-samples the demoted files
    // through the document renderer, which is also what indexes them.
    let mut kept: Vec<Cluster> = Vec::with_capacity(data.len());
    for c in data {
        let one_record_each = c.records == c.members.len() as u64;
        if demotable_family(c.family)
            && c.group.is_none()
            && c.members.len() <= DOC_DEMOTE_MAX_FILES
            && one_record_each
        {
            let d = docs_cluster(&mut docs, &c.scope);
            d.members.extend(c.members.iter().copied());
            d.demoted.extend(c.members);
            continue;
        }
        kept.push(c);
    }

    // Docs slugs are scope-derived — independent of which other files exist,
    // so adding a file can never rename (and re-home, #178) a docs dataset.
    let mut clusters: Vec<Cluster> = Vec::with_capacity(docs.len() + kept.len());
    let mut used: HashSet<String> = HashSet::new();
    for (scope, mut c) in docs {
        let base = if scope.is_empty() {
            "docs".to_string()
        } else {
            let s = sanitize_slug(&scope);
            if s.is_empty() {
                "docs".to_string()
            } else {
                format!("{s}-docs")
            }
        };
        let mut slug = base.clone();
        let mut k = 2;
        while !used.insert(slug.clone()) {
            slug = format!("{base}-{k}");
            k += 1;
        }
        c.slug = slug;
        clusters.push(c);
    }
    assign_slugs(&mut kept, rels, &mut used);
    clusters.extend(kept);
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

fn assign_slugs(clusters: &mut [Cluster], rels: &[String], used: &mut HashSet<String>) {
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

    /// A prose file: same extractor-owned vocabulary situation as code.
    fn prose_sketch(file_idx: usize) -> Sketch {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), acc("README"));
        fields.insert("body".to_string(), acc("This project does things."));
        Sketch {
            file_idx,
            group: None,
            family: Family::TxtProse,
            fields,
            key_fields: HashSet::new(),
            records: 1,
        }
    }

    /// A data file: the names are the file's own, so they are the key.
    fn data_sketch(file_idx: usize, family: Family, names: &[&str]) -> Sketch {
        data_sketch_n(file_idx, family, names, 1)
    }

    fn data_sketch_n(file_idx: usize, family: Family, names: &[&str], records: u64) -> Sketch {
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
            records,
        }
    }

    fn root_scopes(n: usize) -> Vec<String> {
        vec![String::new(); n]
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
        let scopes = root_scopes(6);

        // Before: files 4 and 5 parsed to zero symbols.
        let before = cluster(
            (0..6).map(|i| code_sketch(i, i < 4)).collect::<Vec<_>>(),
            &rels,
            &scopes,
        );
        // The whole point: one dataset, not one per symbol-presence variant.
        assert_eq!(before.len(), 1, "{before:#?}");
        assert_eq!(before[0].members.len(), 6);

        // After: the improved grammar finds symbols in all six.
        let after = cluster(
            (0..6).map(|i| code_sketch(i, true)).collect::<Vec<_>>(),
            &rels,
            &scopes,
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
        let scopes = root_scopes(4);
        let mixed = |symbols_in_b: bool| {
            vec![
                code_sketch(0, true),
                code_sketch(1, symbols_in_b),
                data_sketch_n(2, Family::Csv, &["ts", "level", "msg"], 40),
                data_sketch_n(3, Family::Csv, &["id", "email", "name"], 40),
            ]
        };
        let before = cluster(mixed(false), &rels, &scopes);
        let after = cluster(mixed(true), &rels, &scopes);
        let slugs = |cs: &[Cluster]| {
            let mut v: Vec<(usize, String)> = cs
                .iter()
                .flat_map(|c| c.members.iter().map(|&m| (m, c.slug.clone())))
                .collect();
            v.sort();
            v
        };
        assert_eq!(slugs(&before), slugs(&after));
        // 1 document dataset + 2 unrelated CSV schemas.
        assert_eq!(before.len(), 3, "{before:#?}");
    }

    /// The fix must not blunt clustering: unrelated data schemas stay apart,
    /// near-identical ones still merge, and data formats never mix.
    #[test]
    fn genuinely_different_data_still_separates() {
        let rels: Vec<String> = (0..5).map(|i| format!("d/f{i}")).collect();
        let scopes = root_scopes(5);
        let clusters = cluster(
            vec![
                data_sketch_n(0, Family::Csv, &["ts", "level", "msg"], 40),
                // one extra column out of four — still the same schema
                data_sketch_n(1, Family::Csv, &["ts", "level", "msg", "host"], 40),
                data_sketch_n(2, Family::Csv, &["id", "email", "name"], 40),
                // same names, different format family
                data_sketch_n(3, Family::Jsonl, &["ts", "level", "msg"], 40),
                code_sketch(4, true),
            ],
            &rels,
            &scopes,
        );
        assert_eq!(clusters.len(), 4, "{clusters:#?}");
        let members: Vec<Vec<usize>> = clusters.iter().map(|c| c.members.clone()).collect();
        assert!(members.contains(&vec![0, 1]), "{members:?}");
        assert!(members.contains(&vec![2]), "{members:?}");
        assert!(members.contains(&vec![3]), "{members:?}");
        assert!(members.contains(&vec![4]), "{members:?}");
    }

    /// #196: one workspace, several format families — still ONE dataset.
    /// Code and prose carry no schema, so format family is not a boundary.
    #[test]
    fn one_workspace_of_code_and_prose_is_one_dataset() {
        let rels: Vec<String> = vec![
            "src/a.rs".into(),
            "src/b.rs".into(),
            "README.md".into(),
            "docs/guide.md".into(),
        ];
        let scopes = root_scopes(4);
        let clusters = cluster(
            vec![
                code_sketch(0, true),
                code_sketch(1, false),
                prose_sketch(2),
                prose_sketch(3),
            ],
            &rels,
            &scopes,
        );
        assert_eq!(clusters.len(), 1, "{clusters:#?}");
        assert_eq!(clusters[0].members.len(), 4);
        assert!(clusters[0].is_docs);
        assert_eq!(clusters[0].slug, "docs");
        // the docs mapping holds both vocabularies
        assert!(clusters[0].fields.contains_key("language"));
        assert!(clusters[0].fields.contains_key("body"));
    }

    /// #173: two repositories in one tree — one document dataset per repo,
    /// named after the repo, regardless of how many incidental shapes the
    /// source files take.
    #[test]
    fn each_repository_is_its_own_document_dataset() {
        let rels: Vec<String> = vec![
            "valkey/src/a.c".into(),
            "valkey/README.md".into(),
            "memcached/src/b.c".into(),
            "memcached/doc/notes.md".into(),
        ];
        let scopes: Vec<String> = vec![
            "valkey".into(),
            "valkey".into(),
            "memcached".into(),
            "memcached".into(),
        ];
        let clusters = cluster(
            vec![
                code_sketch(0, true),
                prose_sketch(1),
                code_sketch(2, true),
                prose_sketch(3),
            ],
            &rels,
            &scopes,
        );
        assert_eq!(clusters.len(), 2, "{clusters:#?}");
        let mut slugs: Vec<&str> = clusters.iter().map(|c| c.slug.as_str()).collect();
        slugs.sort();
        assert_eq!(slugs, ["memcached-docs", "valkey-docs"]);
        for c in &clusters {
            assert!(c.is_docs);
            assert_eq!(c.members.len(), 2);
        }
    }

    /// #173's 382-cluster mechanism: single-record structured files with
    /// one-off key sets are configuration, not datasets — they demote into
    /// the scope's document dataset and their config keys never reach its
    /// mapping. A recurring schema (many files) stays a data dataset, and a
    /// multi-record structured file is a collection whatever its size.
    #[test]
    fn one_off_config_files_demote_to_documents() {
        let mut sketches = Vec::new();
        let mut rels = Vec::new();
        // one code file so the scope has a docs dataset already
        rels.push("src/main.c".to_string());
        sketches.push(code_sketch(0, true));
        // 30 per-command config JSONs, each its own key set, one record each
        for i in 1..=30 {
            rels.push(format!("commands/cmd{i}.json"));
            let summary = format!("CMD{i}_summary");
            let arity = format!("CMD{i}_arity");
            sketches.push(data_sketch(
                i,
                Family::Json,
                &[summary.as_str(), arity.as_str()],
            ));
        }
        // a recurring schema: 12 files sharing the same keys, one record each
        for i in 31..=42 {
            rels.push(format!("profiles/p{i}.json"));
            sketches.push(data_sketch(i, Family::Json, &["name", "email", "age"]));
        }
        // a multi-record JSON collection (array file) with a one-off schema
        rels.push("data/events.json".to_string());
        sketches.push(data_sketch_n(43, Family::Json, &["evt", "ts_ms"], 500));
        let scopes = root_scopes(rels.len());
        let clusters = cluster(sketches, &rels, &scopes);

        let docs: Vec<&Cluster> = clusters.iter().filter(|c| c.is_docs).collect();
        assert_eq!(docs.len(), 1, "{clusters:#?}");
        let d = docs[0];
        assert_eq!(d.members.len(), 31, "code + 30 demoted configs");
        assert_eq!(d.demoted.len(), 30);
        assert!(
            !d.fields.keys().any(|k| k.starts_with("CMD")),
            "config keys leaked into the document mapping: {:?}",
            d.fields.keys()
        );

        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(data.len(), 2, "{clusters:#?}");
        assert!(
            data.iter()
                .any(|c| c.members.len() == 12 && c.key_fields.contains("email")),
            "the recurring profile schema must stay a data dataset"
        );
        assert!(
            data.iter()
                .any(|c| c.records == 500 && c.key_fields.contains("evt")),
            "a multi-record collection must stay a data dataset"
        );
    }

    /// Scope is a hard boundary for data schemas too: the same CSV schema in
    /// two repositories is two datasets (per-repo corpora stay self-contained).
    #[test]
    fn the_same_schema_in_two_repositories_stays_apart() {
        let rels: Vec<String> = vec!["a/x.csv".into(), "b/y.csv".into()];
        let scopes: Vec<String> = vec!["a".into(), "b".into()];
        let clusters = cluster(
            vec![
                data_sketch_n(0, Family::Csv, &["ts", "level", "msg"], 40),
                data_sketch_n(1, Family::Csv, &["ts", "level", "msg"], 40),
            ],
            &rels,
            &scopes,
        );
        assert_eq!(clusters.len(), 2, "{clusters:#?}");
    }

    /// Adding a file to the tree must never rename the docs dataset (its slug
    /// is scope-derived, not content-derived) — the #178 property extended to
    /// the new grouping.
    #[test]
    fn adding_a_file_never_renames_the_docs_dataset() {
        let rels3: Vec<String> = vec!["src/a.rs".into(), "src/b.rs".into(), "lib/c.rs".into()];
        let before = cluster(
            vec![code_sketch(0, true), code_sketch(1, true)],
            &rels3,
            &root_scopes(3),
        );
        let after = cluster(
            vec![
                code_sketch(0, true),
                code_sketch(1, true),
                code_sketch(2, true),
            ],
            &rels3,
            &root_scopes(3),
        );
        assert_eq!(before[0].slug, after[0].slug);
    }
}
