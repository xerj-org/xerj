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
//! `json`/`yaml`/`xml` files, including a short multi-document stream, is
//! *demoted* to the scope's document dataset and indexed as a document
//! (title/body), because its one-off key set is configuration, not a
//! collection (#173: valkey's per-command JSON files made 382 such
//! "datasets"). Any JSON/YAML/XML/HTML cluster with fewer than
//! [`MIN_DATASET_RECORDS`] rows folds the same way, grouped or not: a page's
//! 5-row table is not an index. A scope that still has
//! more medium structured clusters than [`MAX_STRUCTURED_DATASETS_PER_SCOPE`]
//! folds the smallest into documents. A run keeps at most
//! [`MAX_DATASETS_PER_RUN`] data datasets in all: past it the smallest
//! self-describing clusters fold into documents, then the smallest CSV/JSONL/
//! log clusters merge into one shared dataset per scope and family (their
//! rows stay rows, and every row keeps its `path`). A cluster of
//! [`DATASET_CAP_PROTECT_RECORDS`] or more never moves. A file folds whole or
//! not at all, so no kept dataset loses a row.
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

/// A schema-clustered `json`/`yaml`/`xml` group at most this many files large
/// is eligible to fold into the scope's document dataset. A schema that
/// recurs across more files than this is treated as a real collection.
pub const DOC_DEMOTE_MAX_FILES: usize = 8;

/// Per file, this many records is still a config document: one JSON object,
/// a short array, or a multi-document YAML stream. The old rule required
/// exactly one record, so a workflow file or a small manifest became its own
/// index (one repository → well over a hundred indices).
pub const DOC_DEMOTE_MAX_RECORDS_PER_FILE: u64 = 32;

/// After the small-config fold, one repository scope keeps at most this many
/// `json`/`yaml`/`xml`/`html` data datasets below
/// [`DATASET_CAP_PROTECT_RECORDS`]. The smallest are folded into the document
/// dataset until the scope is at the cap. CSV, JSONL and logs are collections
/// by nature and are not counted here; SQL tables never fold.
pub const MAX_STRUCTURED_DATASETS_PER_SCOPE: usize = 12;

/// A cluster with at least this many records is a real collection. Neither
/// cap folds it.
pub const DATASET_CAP_PROTECT_RECORDS: u64 = 1024;

/// A data cluster of a [`foldable_family`] with fewer rows than this, across
/// all its files, is folded into the scope's document dataset. A separate
/// index for a 5-row page table costs every query over the prefix a shard
/// visit and holds nothing a document search does not find. On a 5 GB
/// benchmark tree, 278 of 357 JSON/HTML/XML/YAML datasets were this small.
pub const MIN_DATASET_RECORDS: u64 = 16;

/// One run aims for at most this many data datasets (document datasets are
/// not counted), over all scopes (#2422: one 5 GB folder made 454 indices,
/// and a search over the prefix visits every one). Past the cap the smallest
/// clusters below [`DATASET_CAP_PROTECT_RECORDS`] rows go first:
///
/// 1. a [`foldable_family`] cluster folds into its scope's documents;
/// 2. a [`mergeable_family`] cluster (CSV, JSONL, logs) merges into one
///    shared dataset per scope and family, so its rows stay rows.
///
/// A protected collection, a workbook sheet, an SQL table and a grouped
/// (multi-array) file never move, so a tree with more of those than the
/// cap keeps them all and ends above it.
pub const MAX_DATASETS_PER_RUN: usize = 32;

/// A small `json`/`yaml`/`xml` cluster (no sub-file group) is configuration,
/// not a dataset: few files, and not many records in each file.
pub(crate) fn small_structured_config(c: &Cluster) -> bool {
    if !demotable_family(c.family) || c.group.is_some() || c.members.is_empty() {
        return false;
    }
    if c.members.len() > DOC_DEMOTE_MAX_FILES {
        return false;
    }
    let ceiling = (c.members.len() as u64).saturating_mul(DOC_DEMOTE_MAX_RECORDS_PER_FILE);
    // `population` is the real row count when the extractor knows it (a JSON
    // array sampled at 500 can still hold 100_000 rows). `records` is only
    // how many rows the sampler kept.
    c.population <= ceiling
}

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
    /// Rows this group actually holds. Equals `records` when the sample saw
    /// every row; larger when the extractor declared a full array length.
    pub population: u64,
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
    /// Sum of the members' [`Sketch::population`]. The per-scope cap and the
    /// small-config fold consult this, not the sample count in `records`.
    pub population: u64,
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
pub(crate) fn demotable_family(f: Family) -> bool {
    matches!(f, Family::Json | Family::Yaml | Family::Xml)
}

/// May a data cluster of this family fold into the document dataset by size
/// or by a cap? Self-describing formats only — the shapes one tree repeats
/// per file: a page's table, a config's keys, one small JSON array. Each
/// renders as useful text through `extract_as_document` (an HTML page
/// through the HTML renderer). CSV, JSONL and logs are collections by
/// nature, and workbooks, SQLite and SQL carry real table schemas: they
/// stay datasets.
pub(crate) fn foldable_family(f: Family) -> bool {
    matches!(f, Family::Json | Family::Yaml | Family::Xml | Family::Html)
}

/// May a data cluster of this family merge, past [`MAX_DATASETS_PER_RUN`],
/// with others of its scope and family into one shared dataset? Line-record
/// formats only: every record is a row of its own and the merged mapping is
/// the union of the members' fields. Workbooks and SQL keep their tables.
pub(crate) fn mergeable_family(f: Family) -> bool {
    matches!(f, Family::Csv | Family::Jsonl | Family::Logs)
}

/// A foldable data cluster too small to be a dataset of its own. A schema
/// that recurs across more than [`DOC_DEMOTE_MAX_FILES`] files is a real
/// collection however few rows each file holds, and stays.
pub(crate) fn below_min_dataset(c: &Cluster) -> bool {
    foldable_family(c.family)
        && !c.members.is_empty()
        && c.members.len() <= DOC_DEMOTE_MAX_FILES
        && c.population < MIN_DATASET_RECORDS
}

/// Cluster sketches into datasets. `rels` and `scopes` are indexed by
/// `Sketch::file_idx`: `rels` are root-relative paths (slug naming), `scopes`
/// the repository scope of each file ("" = root) — see `module` docs.
///
/// Equivalent to feeding every sketch, in order, to a [`Clusterer`] and
/// calling [`Clusterer::finish`].
pub fn cluster(sketches: Vec<Sketch>, rels: &[String], scopes: &[String]) -> Vec<Cluster> {
    let mut clusterer = Clusterer::default();
    for sk in sketches {
        let scope = scopes[sk.file_idx].as_str();
        clusterer.push(sk, scope);
    }
    clusterer.finish(rels)
}

fn docs_cluster<'a>(docs: &'a mut BTreeMap<String, Cluster>, scope: &str) -> &'a mut Cluster {
    docs.entry(scope.to_string()).or_insert_with(|| Cluster {
        family: Family::Code,
        group: None,
        members: Vec::new(),
        fields: HashMap::new(),
        key_fields: HashSet::new(),
        records: 0,
        population: 0,
        slug: String::new(),
        scope: scope.to_string(),
        is_docs: true,
        demoted: Vec::new(),
    })
}

/// Incremental form of [`cluster`]: sketches are folded into their dataset as
/// they arrive, so a caller never has to hold every file's sketch at once.
///
/// A sketch carries one [`FieldAcc`] per sampled field (a 1 KiB byte histogram,
/// up to 512 length samples and up to [`crate::infer::RAW_CAP`] raw values
/// each). Phase A used to collect every file's sketches before clustering:
/// tens of KiB per file, and 4.4 GB of client RSS on a 146k-file folder before
/// a single document was sent. Folding them as they are produced bounds that
/// to the clusters themselves. Sketches must be pushed in file order for the
/// result to match [`cluster`] exactly (clustering is greedy and
/// order-dependent).
#[derive(Default)]
pub struct Clusterer {
    // scope → docs cluster (BTreeMap: deterministic scope order).
    docs: BTreeMap<String, Cluster>,
    data: Vec<Cluster>,
}

impl Clusterer {
    /// Fold one sketch into its dataset. `scope` is the repository scope of
    /// `sk.file_idx`.
    pub fn push(&mut self, sk: Sketch, scope: &str) {
        let docs = &mut self.docs;
        let data = &mut self.data;
        // Documents: no data-derived names, no sub-file group.
        if sk.key_fields.is_empty() && sk.group.is_none() {
            let c = docs_cluster(docs, scope);
            c.members.push(sk.file_idx);
            c.records += sk.records;
            c.population += sk.population;
            for (k, acc) in sk.fields {
                match c.fields.get_mut(&k) {
                    Some(existing) => existing.merge(&acc),
                    None => {
                        c.fields.insert(k, acc);
                    }
                }
            }
            return;
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
                c.population += sk.population;
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
                population: sk.population,
                slug: String::new(),
                scope: scope.to_string(),
                is_docs: false,
                demoted: Vec::new(),
            }),
        }
    }

    /// Demote, cap and name the clusters. `rels` is indexed by file index.
    pub fn finish(self, rels: &[String]) -> Vec<Cluster> {
        let Clusterer { mut docs, mut data } = self;
        // Fold what is not a dataset into the scope's document dataset. Three
        // rules choose the clusters; a fourth keeps files whole:
        //
        // 1. A small config cluster — one record each, or a short multi-doc
        //    YAML/JSON/XML file — is configuration (#173).
        // 2. A JSON/YAML/XML/HTML cluster below `MIN_DATASET_RECORDS` rows is
        //    not a collection anyone queries as one: a page's 5-row table, one
        //    tiny grouped JSON array. Each used to become its own index (one
        //    5 GB tree → 454 indices, two thirds of them under 16 rows).
        // 3. Caps on the same families below `DATASET_CAP_PROTECT_RECORDS`
        //    rows: at most `MAX_STRUCTURED_DATASETS_PER_SCOPE` per scope, and
        //    the run as a whole at `MAX_DATASETS_PER_RUN`. The smallest fold
        //    first; past the run cap, small CSV/JSONL/log clusters then merge
        //    per scope and family (below).
        // 4. Folding marks the whole file `as_document`, so phase B indexes the
        //    file as one document and none of its rows. A file folds only when
        //    EVERY cluster it is a member of folds; otherwise all of its
        //    clusters stay, and no row of a kept dataset is dropped.
        //
        // Folded members' sampled fields are dropped — the caller re-samples
        // them through the document renderer, which is also what indexes them.
        let n = data.len();
        let mut fold = vec![false; n];
        for (i, c) in data.iter().enumerate() {
            fold[i] = small_structured_config(c) || below_min_dataset(c);
        }
        // Clusters of each file. A file that also belongs to an unfoldable
        // cluster (a large collection, an SQL table, a workbook sheet) never
        // folds: its unit below is not a cap candidate.
        let mut file_clusters: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, c) in data.iter().enumerate() {
            for &m in &c.members {
                file_clusters.entry(m).or_default().push(i);
            }
        }
        let foldable_at_all =
            |c: &Cluster| foldable_family(c.family) && c.population < DATASET_CAP_PROTECT_RECORDS;
        // The caps move FILES, so they work on units of clusters joined by a
        // shared file: a multi-array JSON file (grammar.json, an OpenAPI spec)
        // and every grouped cluster of it. On main the caps skipped grouped
        // clusters outright (`c.group.is_none()`), and those were most of the
        // 454 indices in #2422. A unit folds whole, and only when every cluster
        // in it may fold and their rows together are below the protect size.
        let mut parent: Vec<usize> = (0..n).collect();
        fn root(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for cs in file_clusters.values() {
            for w in cs.windows(2) {
                let (x, y) = (root(&mut parent, w[0]), root(&mut parent, w[1]));
                if x != y {
                    parent[x.max(y)] = x.min(y);
                }
            }
        }
        let mut units: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..n {
            let r = root(&mut parent, i);
            units.entry(r).or_default().push(i);
        }
        // A unit with any cluster left unfolded keeps ALL its clusters (rule 4
        // unfolds the rest of its files), so a kept unit counts u.len().
        let mut total_live = units
            .values()
            .filter(|u| u.iter().any(|&j| !fold[j]))
            .map(Vec::len)
            .sum::<usize>();
        // (clusters, rows, first file, members) per foldable unit.
        let mut cand: Vec<(usize, u64, usize, Vec<usize>)> = units
            .into_values()
            .filter_map(|u| {
                if u.iter().all(|&j| fold[j]) || !u.iter().all(|&j| foldable_at_all(&data[j])) {
                    return None;
                }
                let rows: u64 = u.iter().map(|&j| data[j].population).sum();
                if rows >= DATASET_CAP_PROTECT_RECORDS {
                    return None;
                }
                let first = u
                    .iter()
                    .flat_map(|&j| data[j].members.iter().copied())
                    .min()
                    .unwrap_or(usize::MAX);
                Some((u.len(), rows, first, u))
            })
            .collect();
        // Largest collections stay. Ties break toward the earlier file so the
        // fold does not depend on hash order. Size is the real row count, so a
        // file sampled at 500 does not tie with a 100-row file that happened to
        // fit in the sample.
        cand.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)).then(a.2.cmp(&b.2)));
        let order = |a: &usize, b: &usize| {
            let (a, b) = (&data[*a], &data[*b]);
            b.population
                .cmp(&a.population)
                .then(b.members.len().cmp(&a.members.len()))
                .then_with(|| {
                    let am = a.members.iter().copied().min().unwrap_or(usize::MAX);
                    let bm = b.members.iter().copied().min().unwrap_or(usize::MAX);
                    am.cmp(&bm)
                })
        };
        // Per-scope cap on small self-describing datasets.
        let mut kept_in_scope: HashMap<String, usize> = HashMap::new();
        let mut folded_unit = vec![false; cand.len()];
        for (k, (live, _, _, u)) in cand.iter().enumerate() {
            let scope = data[u[0]].scope.clone();
            let kept = kept_in_scope.entry(scope).or_default();
            if *kept + live > MAX_STRUCTURED_DATASETS_PER_SCOPE {
                for &j in u {
                    fold[j] = true;
                }
                folded_unit[k] = true;
                total_live -= live;
            } else {
                *kept += live;
            }
        }
        // Per-run cap, all scopes together (#2422). The fold half: the smallest
        // self-describing units go to documents until the run is at the cap.
        // The merge half runs after rule 4, on what is left.
        let mut live = total_live;
        for (k, (n_live, _, _, u)) in cand.iter().enumerate().rev() {
            if live <= MAX_DATASETS_PER_RUN {
                break;
            }
            if folded_unit[k] {
                continue;
            }
            for &j in u {
                fold[j] = true;
            }
            live -= n_live;
        }
        // Rule 4, to a fixed point: unfolding only ever clears flags.
        loop {
            let mut changed = false;
            for i in 0..n {
                if fold[i]
                    && data[i]
                        .members
                        .iter()
                        .any(|m| file_clusters[m].iter().any(|&j| !fold[j]))
                {
                    fold[i] = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // The merge half of the per-run cap. Still over it, the smallest
        // CSV/JSONL/log clusters (no sub-file group, below the protect size,
        // whose files hold no other cluster) merge into ONE dataset per scope
        // and family. The first candidate of a bucket becomes the shared
        // dataset; each later one joins it and saves an index.
        let mut merge_into: Vec<Option<usize>> = vec![None; n];
        let mut live = fold.iter().filter(|f| !**f).count();
        if live > MAX_DATASETS_PER_RUN {
            let mut cands: Vec<usize> = (0..n)
                .filter(|&i| {
                    let c = &data[i];
                    !fold[i]
                        && mergeable_family(c.family)
                        && c.group.is_none()
                        && c.population < DATASET_CAP_PROTECT_RECORDS
                        && c.members.iter().all(|m| file_clusters[m].len() == 1)
                })
                .collect();
            cands.sort_by(order);
            let mut shared: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
            for &i in cands.iter().rev() {
                if live <= MAX_DATASETS_PER_RUN {
                    break;
                }
                let key = (data[i].scope.clone(), data[i].family.as_str());
                match shared.get(&key) {
                    Some(&t) => {
                        merge_into[i] = Some(t);
                        live -= 1;
                    }
                    None => {
                        shared.insert(key, i);
                    }
                }
            }
        }
        for i in 0..n {
            let Some(t) = merge_into[i] else { continue };
            let src = std::mem::replace(
                &mut data[i],
                Cluster {
                    family: Family::Code,
                    group: None,
                    members: Vec::new(),
                    fields: HashMap::new(),
                    key_fields: HashSet::new(),
                    records: 0,
                    population: 0,
                    slug: String::new(),
                    scope: String::new(),
                    is_docs: false,
                    demoted: Vec::new(),
                },
            );
            let dst = &mut data[t];
            dst.members.extend(src.members);
            dst.records += src.records;
            dst.population += src.population;
            dst.key_fields.extend(src.key_fields);
            for (k, acc) in src.fields {
                match dst.fields.get_mut(&k) {
                    Some(existing) => existing.merge(&acc),
                    None => {
                        dst.fields.insert(k, acc);
                    }
                }
            }
        }
        let mut kept: Vec<Cluster> = Vec::with_capacity(n);
        let mut folded_files: HashSet<usize> = HashSet::new();
        for (i, c) in data.into_iter().enumerate() {
            if merge_into[i].is_some() {
                continue;
            }
            if !fold[i] {
                kept.push(c);
                continue;
            }
            let d = docs_cluster(&mut docs, &c.scope);
            for m in c.members {
                // A file with several folded clusters is one document.
                if folded_files.insert(m) {
                    if !d.members.contains(&m) {
                        d.members.push(m);
                    }
                    d.demoted.push(m);
                }
            }
        }

        // Docs slugs are scope-derived — independent of which other files exist,
        // so adding a file can never rename (and re-home, #178) a docs dataset.
        let mut clusters: Vec<Cluster> = Vec::with_capacity(docs.len() + kept.len());
        let mut used: HashSet<String> = HashSet::new();
        for (scope, mut c) in docs {
            let base = if scope.is_empty() {
                "docs".to_string()
            } else if let Some(repo_scope) = crate::sdd::parse_specs_scope(&scope) {
                let s = sanitize_slug(repo_scope);
                if s.is_empty() {
                    "specs".to_string()
                } else {
                    format!("{s}-specs")
                }
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

/// [`sanitize_slug`] for an index prefix (`--prefix`, and the `xc-<corpus>-b<n>`
/// prefix `xerj corpus index` passes). Same rules, with one difference: a run
/// made only of literal `-` is kept as written. `sanitize_slug` collapsed it,
/// so a corpus named `owner--repo` (every author-approved hub candidate) was
/// indexed under `xc-owner-repo-…` while `corpus index` verified, and `xerj code`
/// searched, `xc-owner--repo-…`: 0 records, every build (issue #1889). Any other
/// separator run (`_`, `.`, spaces, a mix with `-`) still becomes one `-`.
pub fn sanitize_prefix(s: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let flush = |out: &mut String, run: &mut String| {
        if !run.is_empty() {
            if !out.is_empty() {
                if run.chars().all(|c| c == '-') {
                    out.push_str(run);
                } else {
                    out.push('-');
                }
            }
            run.clear();
        }
    };
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            flush(&mut out, &mut run);
            out.push(c.to_ascii_lowercase());
        } else {
            run.push(c);
        }
    }
    // a trailing separator run is dropped, as in `sanitize_slug`
    out
}

/// Why `{prefix}-docs` cannot be an index name, if it cannot.
///
/// #2057: the server used to be the first thing that noticed a digit-leading
/// prefix, and it noticed after the hash. `None` means the prefix can name
/// an index.
pub fn index_prefix_error(prefix: &str) -> Option<String> {
    let index = format!("{prefix}-docs");
    match xerj_common::IndexName::validate(&index) {
        Ok(()) => None,
        Err(err) => {
            let hint = if prefix.starts_with(|c: char| c.is_ascii_digit()) {
                format!(" A prefix that starts with a letter works, for example p{prefix}.")
            } else {
                String::new()
            };
            Some(format!(
                "--prefix {prefix} is not a valid index prefix: an index is named \
                 {prefix}-<dataset>, and `{index}` is rejected ({err}). An index name must \
                 start with a lowercase letter or '.'.{hint} Nothing was walked."
            ))
        }
    }
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

/// Drop a leading `{prefix}-` from each slug.
///
/// The index name is `{prefix}-{slug}`. A folder of root-level logs indexed
/// with `--prefix logs` otherwise becomes `logs-logs-syslog`: the slug's
/// family fallback is `logs` and a single-file collision appends the stem.
/// Stripping that one repeated segment makes the index `logs-syslog`. A slug
/// that does not start with the prefix is left alone, so the default prefix
/// `ax` does not rename `logs-syslog`. An empty remainder is not produced.
/// Collisions created by the strip are numbered `-2`, `-3`, in slug order.
pub fn drop_repeated_prefix(prefix: &str, clusters: &mut [Cluster]) {
    let prefix = sanitize_prefix(prefix);
    if prefix.is_empty() {
        return;
    }
    let needle = format!("{prefix}-");
    let mut order: Vec<usize> = (0..clusters.len()).collect();
    order.sort_by_key(|&i| clusters[i].slug.clone());
    let mut used: HashSet<String> = HashSet::new();
    let mut next = vec![String::new(); clusters.len()];
    for &i in &order {
        let base = match clusters[i].slug.strip_prefix(&needle) {
            Some(rest) if !rest.is_empty() => rest.to_string(),
            _ => clusters[i].slug.clone(),
        };
        let mut slug = base.clone();
        let mut k = 2u32;
        while !used.insert(slug.clone()) {
            slug = format!("{base}-{k}");
            k += 1;
        }
        next[i] = slug;
    }
    for (i, slug) in next.into_iter().enumerate() {
        clusters[i].slug = slug;
    }
}

/// Longest dataset slug autoindex derives. Slugs come from file paths and
/// group names with no natural bound — a Go module path produced a 329-char
/// index name that the server refused (index names are capped at 255 bytes),
/// failing the run (part of #1122). 128 leaves room for the `--prefix` and for
/// the suffixes rebuilds add to it (`-reindex`, `-generation-v1`).
pub const MAX_SLUG_LEN: usize = 128;

/// Shorten slugs longer than [`MAX_SLUG_LEN`]: keep the leading part and
/// append `-` plus 8 hex digits of a hash of the full slug, so the name stays
/// readable, is the same on every run, and two long slugs sharing a long
/// beginning still get different names. Slugs within the limit are untouched.
pub fn cap_slug_len(clusters: &mut [Cluster]) {
    if clusters.iter().all(|c| c.slug.len() <= MAX_SLUG_LEN) {
        return;
    }
    let mut used: HashSet<String> = clusters
        .iter()
        .filter(|c| c.slug.len() <= MAX_SLUG_LEN)
        .map(|c| c.slug.clone())
        .collect();
    let mut order: Vec<usize> = (0..clusters.len())
        .filter(|&i| clusters[i].slug.len() > MAX_SLUG_LEN)
        .collect();
    order.sort_by_key(|&i| clusters[i].slug.clone());
    for i in order {
        let full = clusters[i].slug.clone();
        let mut salt = 0u64;
        loop {
            let slug = capped_slug(&full, salt);
            if used.insert(slug.clone()) {
                clusters[i].slug = slug;
                break;
            }
            salt += 1;
        }
    }
}

fn capped_slug(full: &str, salt: u64) -> String {
    let hash = xxhash_rust::xxh3::xxh3_64_with_seed(full.as_bytes(), salt) as u32;
    let suffix = format!("-{hash:08x}");
    // Slugs are ASCII (`sanitize_slug`); the boundary check keeps this safe
    // for any other caller.
    let mut cut = MAX_SLUG_LEN - suffix.len();
    while !full.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = full[..cut].trim_end_matches('-');
    format!("{head}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(slug: &str) -> Cluster {
        Cluster {
            family: Family::Json,
            group: None,
            members: Vec::new(),
            fields: HashMap::new(),
            key_fields: HashSet::new(),
            records: 0,
            population: 0,
            slug: slug.to_string(),
            scope: String::new(),
            is_docs: false,
            demoted: Vec::new(),
        }
    }

    #[test]
    fn long_slugs_are_capped_with_a_stable_hash_suffix() {
        // The real 329-char index name from a Go module tree (part of #1122).
        let long = format!(
            "gomod-github-com-paths-v1-example-a-bit-of-everything-floatvalue-{}",
            "nested-message-repeated-string-value-".repeat(7)
        );
        let sibling = format!("{long}other");
        let mut clusters = vec![named("short"), named(&long), named(&sibling)];
        cap_slug_len(&mut clusters);
        assert_eq!(clusters[0].slug, "short");
        for c in &clusters[1..] {
            assert!(c.slug.len() <= MAX_SLUG_LEN, "{} chars", c.slug.len());
            assert!(c.slug.starts_with("gomod-github-com-paths-v1-example"));
            assert_eq!(c.slug, sanitize_slug(&c.slug), "still a valid slug");
            assert!(xerj_common::IndexName::validate(&format!("b5-{}", c.slug)).is_ok());
        }
        assert_ne!(clusters[1].slug, clusters[2].slug);
        // Deterministic: the same input names the same index on every run.
        let mut again = vec![named("short"), named(&long), named(&sibling)];
        cap_slug_len(&mut again);
        let a: Vec<_> = clusters.iter().map(|c| c.slug.clone()).collect();
        let b: Vec<_> = again.iter().map(|c| c.slug.clone()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn capped_slug_never_collides_with_an_existing_one() {
        let long = "a".repeat(MAX_SLUG_LEN + 50);
        let taken = capped_slug(&long, 0);
        let mut clusters = vec![named(&taken), named(&long)];
        cap_slug_len(&mut clusters);
        assert_eq!(clusters[0].slug, taken);
        assert_ne!(clusters[1].slug, taken);
        assert!(clusters[1].slug.len() <= MAX_SLUG_LEN);
    }

    #[test]
    fn a_digit_leading_prefix_is_rejected_before_any_walk() {
        let err = index_prefix_error("2042").expect("2042-docs is not an index name");
        assert!(err.contains("lowercase letter"), "{err}");
        assert!(err.contains("p2042"), "{err}");
        assert!(err.contains("Nothing was walked"), "{err}");
        assert!(index_prefix_error("proj").is_none());
    }

    #[test]
    fn sanitize_prefix_keeps_a_literal_double_hyphen() {
        // issue #1889: `owner--repo` corpora must index under the name they verify under
        assert_eq!(
            sanitize_prefix("xc-0whitedev--java-process-inspector-b1792446398"),
            "xc-0whitedev--java-process-inspector-b1792446398"
        );
        assert_eq!(sanitize_prefix("My_Proj"), "my-proj");
        assert_eq!(sanitize_prefix("a_-b"), "a-b");
        assert_eq!(sanitize_prefix("a. b"), "a-b");
        assert_eq!(sanitize_prefix("--ref--"), "ref");
        assert_eq!(sanitize_prefix("ref-"), "ref");
        assert_eq!(sanitize_prefix("___"), "");
        // without a double hyphen it agrees with sanitize_slug
        for s in ["ax", "xc-lz4-b17", "Logs 2026", "a__b", "x.y.z"] {
            assert_eq!(sanitize_prefix(s), sanitize_slug(s), "{s}");
        }
    }
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
            population: 1,
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
            population: 1,
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
            population: records,
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
    /// large multi-record file stays a collection when the scope is under
    /// the dataset cap.
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
            "a multi-record collection must stay a data dataset while the scope is under the cap"
        );
    }

    /// A YAML/JSON file that yields a handful of records is still config.
    /// The one-record rule left every multi-doc manifest as its own index.
    #[test]
    fn small_multi_doc_yaml_and_json_fold_into_documents() {
        let mut sketches = Vec::new();
        let mut rels = Vec::new();
        rels.push("src/main.rs".into());
        sketches.push(code_sketch(0, true));
        for i in 1..=20 {
            rels.push(format!(".github/workflows/w{i}.yml"));
            sketches.push(data_sketch_n(
                i,
                Family::Yaml,
                &[&format!("job{i}"), "runs_on"],
                4,
            ));
        }
        // one short JSON array, its own schema
        rels.push("config/matrix.json".into());
        sketches.push(data_sketch_n(21, Family::Json, &["os", "rust"], 6));
        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let docs: Vec<&Cluster> = clusters.iter().filter(|c| c.is_docs).collect();
        assert_eq!(docs.len(), 1, "{clusters:#?}");
        assert_eq!(docs[0].demoted.len(), 21, "20 workflows + the matrix");
        assert!(
            clusters.iter().filter(|c| !c.is_docs).count() == 0,
            "small multi-doc configs must not each become an index: {clusters:#?}"
        );
    }

    /// Twenty medium one-off JSON files would be twenty indices. The cap
    /// keeps the largest [`MAX_STRUCTURED_DATASETS_PER_SCOPE`] and folds the
    /// rest. A collection at the protect threshold is never folded.
    #[test]
    fn a_scope_caps_medium_structured_datasets_and_keeps_large_ones() {
        let mut sketches = Vec::new();
        let mut rels = Vec::new();
        rels.push("README.md".into());
        sketches.push(prose_sketch(0));
        let n_medium = MAX_STRUCTURED_DATASETS_PER_SCOPE + 8;
        for i in 0..n_medium {
            let idx = i + 1;
            rels.push(format!("fixtures/f{idx}.json"));
            // Larger index → more records, so the cap keeps the tail.
            sketches.push(data_sketch_n(
                idx,
                Family::Json,
                &[&format!("k{idx}")],
                40 + i as u64,
            ));
        }
        let big = n_medium + 1;
        rels.push("data/events.json".into());
        sketches.push(data_sketch_n(
            big,
            Family::Json,
            &["evt", "ts"],
            DATASET_CAP_PROTECT_RECORDS,
        ));
        // A CSV schema is not subject to the structured cap.
        let csv = big + 1;
        rels.push("data/rows.csv".into());
        sketches.push(data_sketch_n(csv, Family::Csv, &["a", "b"], 10));

        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let docs = clusters.iter().find(|c| c.is_docs).expect("docs");
        assert_eq!(
            docs.demoted.len(),
            8,
            "the eight smallest medium files fold"
        );
        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(
            data.len(),
            MAX_STRUCTURED_DATASETS_PER_SCOPE + 2,
            "cap + the protected JSON + the CSV: {data:#?}"
        );
        assert!(
            data.iter()
                .any(|c| c.records == DATASET_CAP_PROTECT_RECORDS),
            "a large collection is not folded by the cap"
        );
        assert!(data.iter().any(|c| c.family == Family::Csv));
        let kept_medium: Vec<u64> = data
            .iter()
            .filter(|c| c.family == Family::Json && c.records < DATASET_CAP_PROTECT_RECORDS)
            .map(|c| c.records)
            .collect();
        assert_eq!(kept_medium.len(), MAX_STRUCTURED_DATASETS_PER_SCOPE);
        assert!(kept_medium.iter().all(|r| *r >= 40 + 8), "{kept_medium:?}");
    }

    /// Phase A samples 500 rows. A JSON array of 1024+ rows must still be
    /// protected when the sampler's count is below the threshold.
    #[test]
    fn the_cap_protects_a_sampled_array_by_its_real_length() {
        let mut sketches = vec![prose_sketch(0)];
        let mut rels = vec!["README.md".into()];
        for i in 0..MAX_STRUCTURED_DATASETS_PER_SCOPE + 1 {
            let idx = i + 1;
            rels.push(format!("f{idx}.json"));
            sketches.push(data_sketch_n(idx, Family::Json, &[&format!("k{idx}")], 40));
        }
        let big = sketches.len();
        rels.push("huge.json".into());
        let mut huge = data_sketch_n(big, Family::Json, &["id"], 500);
        huge.population = DATASET_CAP_PROTECT_RECORDS;
        sketches.push(huge);
        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let docs = clusters.iter().find(|c| c.is_docs).expect("docs");
        assert!(
            !docs.demoted.contains(&big),
            "real length {}, sample 500: {docs:#?}",
            DATASET_CAP_PROTECT_RECORDS
        );
        assert!(clusters
            .iter()
            .any(|c| !c.is_docs && c.population == DATASET_CAP_PROTECT_RECORDS));
    }

    /// Folding the primary array of a multi-array file would mark the whole
    /// file as a document and drop the sibling array's rows. Since #2422 the
    /// file's two arrays count toward the scope cap as one unit; this unit is
    /// the largest, so it stays whole and the single-array files fold.
    #[test]
    fn a_grouped_sibling_keeps_its_file_out_of_the_structured_cap() {
        let mut sketches = vec![prose_sketch(0)];
        let mut rels = vec!["README.md".into()];
        let n_medium = MAX_STRUCTURED_DATASETS_PER_SCOPE + 1;
        for i in 0..n_medium {
            let idx = i + 1;
            rels.push(format!("fixtures/f{idx}.json"));
            sketches.push(data_sketch_n(idx, Family::Json, &[&format!("k{idx}")], 40));
        }
        let primary_idx = n_medium + 1;
        rels.push("config/subsystem_map.json".into());
        sketches.push(data_sketch_n(
            primary_idx,
            Family::Json,
            &["name", "addr"],
            40,
        ));
        let mut sibling = data_sketch_n(primary_idx, Family::Json, &["label", "start"], 30);
        sibling.group = Some("runs".into());
        sketches.push(sibling);

        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let docs = clusters.iter().find(|c| c.is_docs).expect("docs");
        assert!(
            !docs.demoted.contains(&primary_idx),
            "the multi-array file must stay extracted as records"
        );
        assert!(
            clusters
                .iter()
                .any(|c| { c.group.as_deref() == Some("runs") && c.members == vec![primary_idx] }),
            "the sibling array stays its own dataset: {clusters:#?}"
        );
        assert!(clusters
            .iter()
            .any(|c| { c.group.is_none() && !c.is_docs && c.members.contains(&primary_idx) }));
    }

    /// #2422 root cause: the per-scope cap skipped every grouped cluster, so
    /// a tree of multi-array JSON files (tree-sitter `grammar.json`, OpenAPI
    /// specs) kept one index per array. Grouped clusters now count toward
    /// the cap; a file's arrays fold together, so a kept file keeps all of
    /// its arrays and a folded one is one document.
    #[test]
    fn grouped_arrays_count_toward_the_scope_cap_and_fold_per_file() {
        let mut sketches = vec![prose_sketch(0)];
        let mut rels = vec!["README.md".to_string()];
        let files = 20;
        for f in 1..=files {
            rels.push(format!("grammars/g{f}/grammar.json"));
            sketches.push(data_sketch_n(
                f,
                Family::Json,
                &[&format!("name{f}"), "word"],
                20,
            ));
            let mut g = data_sketch_n(
                f,
                Family::Json,
                &[&format!("rule{f}"), "kind"],
                20 + f as u64,
            );
            g.group = Some(format!("rules{f}"));
            sketches.push(g);
        }
        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(data.len(), MAX_STRUCTURED_DATASETS_PER_SCOPE, "{data:#?}");
        let docs = clusters.iter().find(|c| c.is_docs).expect("docs");
        let kept_files = MAX_STRUCTURED_DATASETS_PER_SCOPE / 2;
        assert_eq!(
            docs.demoted.len(),
            files - kept_files,
            "one document per folded file"
        );
        for c in &data {
            let f = c.members[0];
            assert!(!docs.demoted.contains(&f));
            assert_eq!(
                data.iter().filter(|d| d.members == vec![f]).count(),
                2,
                "both arrays kept"
            );
        }
        // The largest files (by rows) are the ones kept.
        assert!(data.iter().all(|c| c.members[0] > files - kept_files));
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

    /// `--prefix logs` on a folder of root-level log files. The slug's family
    /// fallback is `logs` and a one-file collision appends the stem, so the
    /// index `{prefix}-{slug}` was `logs-logs-syslog`. The default prefix
    /// `ax` does not contain that segment and must not rename the slug.
    #[test]
    fn a_logs_prefix_does_not_repeat_inside_a_root_syslog_slug() {
        let rels = vec!["syslog".into(), "xferlog".into()];
        let sketches = || {
            vec![
                data_sketch_n(0, Family::Logs, &["ts", "host", "program", "message"], 40),
                data_sketch_n(
                    1,
                    Family::Logs,
                    &["date", "host", "bytes", "path", "user"],
                    40,
                ),
            ]
        };
        let mut clusters = cluster(sketches(), &rels, &root_scopes(2));
        let mut raw: Vec<String> = clusters.iter().map(|c| c.slug.clone()).collect();
        raw.sort();
        assert_eq!(
            raw,
            ["logs-syslog", "logs-xferlog"],
            "the family fallback plus the file stem is what stuttered"
        );
        drop_repeated_prefix("logs", &mut clusters);
        let mut slugs: Vec<String> = clusters.iter().map(|c| c.slug.clone()).collect();
        slugs.sort();
        assert_eq!(slugs, ["syslog", "xferlog"]);

        let mut untouched = cluster(sketches(), &rels, &root_scopes(2));
        let before: Vec<String> = untouched.iter().map(|c| c.slug.clone()).collect();
        drop_repeated_prefix("ax", &mut untouched);
        let after: Vec<String> = untouched.iter().map(|c| c.slug.clone()).collect();
        assert_eq!(before, after, "the default prefix must not rename slugs");
    }

    /// One page per table used to be one index per page: a tree of 200 HTML
    /// reports with 5-row tables became 200 indices of a few documents each.
    /// A page table below `MIN_DATASET_RECORDS` rows folds into the scope's
    /// documents; a real table stays a dataset.
    #[test]
    fn small_html_tables_fold_into_documents_and_a_real_table_stays() {
        let mut sketches = vec![prose_sketch(0)];
        let mut rels = vec!["README.md".to_string()];
        for i in 1..=40 {
            rels.push(format!("reports/r{i}.html"));
            sketches.push(data_sketch_n(
                i,
                Family::Html,
                &[&format!("measure{i}"), "value"],
                5,
            ));
        }
        rels.push("reports/table.html".into());
        sketches.push(data_sketch_n(
            41,
            Family::Html,
            &["crate", "downloads"],
            400,
        ));
        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let docs = clusters.iter().find(|c| c.is_docs).expect("docs");
        assert_eq!(docs.demoted.len(), 40, "{clusters:#?}");
        assert_eq!(docs.members.len(), 41, "README + 40 folded pages");
        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(data.len(), 1, "{data:#?}");
        assert_eq!(data[0].members, vec![41]);
    }

    /// A multi-array JSON file folds only whole. When every array is tiny the
    /// file is one document (listed once, not once per array); when one
    /// array is a real collection, every array of the file stays a dataset,
    /// so no kept dataset loses rows.
    #[test]
    fn a_multi_array_file_folds_whole_or_not_at_all() {
        let mut sketches = vec![prose_sketch(0)];
        let rels: Vec<String> = vec!["README.md".into(), "tiny.json".into(), "mixed.json".into()];
        sketches.push(data_sketch_n(1, Family::Json, &["a", "b"], 3));
        let mut g = data_sketch_n(1, Family::Json, &["c", "d"], 2);
        g.group = Some("rules".into());
        sketches.push(g);
        sketches.push(data_sketch_n(2, Family::Json, &["x", "y"], 600));
        let mut g = data_sketch_n(2, Family::Json, &["p", "q"], 2);
        g.group = Some("notes".into());
        sketches.push(g);
        let clusters = cluster(sketches, &rels, &root_scopes(rels.len()));
        let docs = clusters.iter().find(|c| c.is_docs).expect("docs");
        assert_eq!(docs.demoted, vec![1], "{clusters:#?}");
        assert_eq!(docs.members.iter().filter(|&&m| m == 1).count(), 1);
        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(data.len(), 2, "both arrays of mixed.json stay: {data:#?}");
        assert!(data.iter().all(|c| c.members == vec![2]));
    }

    /// The per-scope cap alone let a tree of many repositories keep 12 small
    /// datasets in each. One run keeps at most `MAX_DATASETS_PER_RUN` data
    /// datasets: the protected collection and the CSV count toward it but
    /// never fold, and the largest small JSON datasets fill the rest.
    #[test]
    fn a_run_keeps_at_most_the_run_cap_of_small_structured_datasets() {
        let mut sketches = Vec::new();
        let mut rels = Vec::new();
        let mut scopes = Vec::new();
        let repos = 10;
        let per_repo = MAX_STRUCTURED_DATASETS_PER_SCOPE;
        for r in 0..repos {
            for k in 0..per_repo {
                let idx = rels.len();
                rels.push(format!("repo{r}/f{k}.json"));
                scopes.push(format!("repo{r}"));
                sketches.push(data_sketch_n(
                    idx,
                    Family::Json,
                    &[&format!("k{r}_{k}")],
                    40 + idx as u64,
                ));
            }
        }
        let big = rels.len();
        rels.push("repo0/events.json".into());
        scopes.push("repo0".into());
        sketches.push(data_sketch_n(
            big,
            Family::Json,
            &["evt"],
            DATASET_CAP_PROTECT_RECORDS,
        ));
        let csv = rels.len();
        rels.push("repo0/rows.csv".into());
        scopes.push("repo0".into());
        sketches.push(data_sketch_n(csv, Family::Csv, &["a", "b"], 40));
        let clusters = cluster(sketches, &rels, &scopes);
        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(data.len(), MAX_DATASETS_PER_RUN);
        let small: Vec<&&Cluster> = data
            .iter()
            .filter(|c| c.family == Family::Json && c.population < DATASET_CAP_PROTECT_RECORDS)
            .collect();
        let keep_small = MAX_DATASETS_PER_RUN - 2;
        assert_eq!(small.len(), keep_small);
        let smallest_kept = small.iter().map(|c| c.population).min().unwrap();
        assert_eq!(
            smallest_kept,
            40 + (repos * per_repo - keep_small) as u64,
            "the largest are the ones kept"
        );
        assert!(clusters.iter().any(|c| c.members == vec![big]));
        assert!(clusters
            .iter()
            .any(|c| c.members == vec![csv] && !c.is_docs));
        let demoted: usize = clusters.iter().map(|c| c.demoted.len()).sum();
        assert_eq!(demoted, repos * per_repo - keep_small);
    }

    /// Past the run cap, small CSV/JSONL datasets are not turned into text:
    /// they merge into one dataset per scope and family, every row still a
    /// row. A protected CSV stays as it is.
    #[test]
    fn past_the_run_cap_small_record_datasets_merge_per_scope_and_family() {
        let mut sketches = Vec::new();
        let mut rels = Vec::new();
        let mut scopes = Vec::new();
        let n_csv = MAX_DATASETS_PER_RUN + 10;
        for k in 0..n_csv {
            let idx = rels.len();
            rels.push(format!("bench/run{k}.csv"));
            scopes.push(String::new());
            sketches.push(data_sketch_n(
                idx,
                Family::Csv,
                &[&format!("col{k}"), &format!("val{k}")],
                100 + idx as u64,
            ));
        }
        let big = rels.len();
        rels.push("bench/all.csv".into());
        scopes.push(String::new());
        sketches.push(data_sketch_n(
            big,
            Family::Csv,
            &["q", "ndcg"],
            DATASET_CAP_PROTECT_RECORDS * 4,
        ));
        let clusters = cluster(sketches, &rels, &scopes);
        let data: Vec<&Cluster> = clusters.iter().filter(|c| !c.is_docs).collect();
        assert_eq!(data.len(), MAX_DATASETS_PER_RUN, "{data:#?}");
        assert!(
            clusters.iter().all(|c| c.demoted.is_empty()),
            "no CSV became text"
        );
        let total: usize = data.iter().map(|c| c.members.len()).sum();
        assert_eq!(total, n_csv + 1, "every file still lands in one dataset");
        assert!(data.iter().any(|c| c.members == vec![big]));
        let shared = data.iter().max_by_key(|c| c.members.len()).unwrap();
        assert_eq!(shared.members.len(), n_csv + 1 - (MAX_DATASETS_PER_RUN - 1));
        assert!(
            shared.fields.contains_key("col0"),
            "the union of the members' columns"
        );
        // The merge takes the smallest: file 0 (100 rows) is in the shared one.
        assert!(shared.members.contains(&0));
    }
}
