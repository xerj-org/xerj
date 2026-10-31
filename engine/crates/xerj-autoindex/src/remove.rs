//! `xerj autoindex remove` — delete one autoindexed folder, after listing it.
//!
//! A prefix can hold more than one folder (an older run left in the catalog,
//! or `--replace-prefix` writing into names the previous folder used). This
//! command reads `autoindex-catalog` first and deletes only the rows and
//! indices that belong to the folder it was asked for. An index another
//! folder's catalog row still names is left in place. `autoindex-catalog`
//! itself is never deleted.
//!
//! A build writes dataset indices before it writes the catalog run (that
//! document is the finalize step). An abort — a disk flood-stage block, a
//! killed process — leaves those indices with no catalog run, so a catalog
//! walk cannot see them (#2391). The resume journal already recorded the
//! root, the prefix, and the plan's index names. This command reads that
//! journal and deletes those indices too. An index a completed catalog run
//! still names (the working copy the abort preserved, or another folder)
//! is not deleted.
//!
//! Without `--yes` on a non-interactive stdin, nothing is deleted and the
//! process exits 4 with the plan on stdout. A terminal prompts. `--yes` is
//! the confirmation, not a root selector: two folders under one prefix still
//! require `--root`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::catalog::{self, CATALOG_INDEX};
use crate::esclient::Es;

/// Same exit as the indexing gate: a decision is required, and a retry
/// without new information will not help.
pub const EXIT_NEEDS_DECISION: i32 = 4;

const PAGE: usize = 500;
/// `from + size` must stay within the default `index.max_result_window`.
const WINDOW: usize = 10_000;

#[derive(Debug, Clone)]
pub struct CatalogDoc {
    pub id: String,
    pub source: Value,
}

#[derive(Debug, Clone)]
pub struct LiveIndex {
    pub name: String,
    pub docs: u64,
    pub store_bytes: u64,
}

#[derive(Debug, Clone)]
struct RootGroup {
    /// The path as the catalog recorded it. This is what `--root` prints.
    display: String,
    key: String,
    run_ids: BTreeSet<String>,
    last_started: String,
    datasets: Vec<Row>,
    /// This folder is known only from an unfinished resume journal. The
    /// build never wrote a catalog run.
    from_journal: bool,
}

#[derive(Debug, Clone)]
struct Row {
    id: String,
    doc_kind: String,
    root: String,
    index_name: String,
    record_count: u64,
    bytes: u64,
    file_count: u64,
}

#[derive(Debug, Clone)]
pub struct RemovePlan {
    pub prefix: String,
    roots: Vec<RootGroup>,
    pub selected_key: Option<String>,
    pub needs_root: bool,
    pub root_not_found: bool,
    pub empty: bool,
    indices: Vec<PlannedIndex>,
    kept_indices: Vec<Kept>,
    rows: Vec<Row>,
    kept_rows: Vec<Row>,
    /// Live `{prefix}-*` indices that no catalog row and no resume journal
    /// names, found only when the plan is otherwise empty (#2405). Listed,
    /// and deleted only with [`RemoveRequest::uncataloged`].
    uncataloged: Vec<LiveIndex>,
    /// This plan deletes `uncataloged` (the caller passed `--uncataloged`).
    uncataloged_selected: bool,
}

#[derive(Debug, Clone)]
struct PlannedIndex {
    name: String,
    live_docs: Option<u64>,
    store_bytes: Option<u64>,
    record_count: u64,
    catalog_bytes: u64,
    /// Named by an unfinished resume journal and by no catalog dataset row
    /// this plan is deleting.
    orphan: bool,
}

/// One index an unfinished autoindex build of `root` / `prefix` planned or
/// created, and never published as a catalog run.
///
/// `remove` and `status` both consume this. The journal is the attribution:
/// a live `{prefix}-*` index with no journal is not assumed to be this folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortedIndex {
    pub name: String,
    pub root: String,
    pub prefix: String,
}

#[derive(Debug, Clone)]
struct Kept {
    name: String,
    reason: String,
}

pub struct RemoveRequest {
    pub url: String,
    pub api_key: Option<String>,
    pub prefix: String,
    pub root: Option<String>,
    pub yes: bool,
    /// Read a `y`/`yes` line from stdin. The CLI sets this only on a terminal.
    /// MCP leaves it false: an agent passes `confirm: true` instead.
    pub prompt: bool,
    /// Explicit `--state-dir`. The default journal (keyed by folder, URL, and
    /// prefix) is always removed after a successful delete; this one is too,
    /// because the default hash cannot find a journal the operator moved.
    pub state_dir: Option<PathBuf>,
    /// `--uncataloged`: when the prefix has no catalog run and no unfinished
    /// resume journal, delete the live `{prefix}-*` indices that no catalog
    /// row names (#2405). A build killed before finalize whose journal is
    /// gone (another machine, another HOME, a discarded state dir) leaves
    /// exactly that. Without this flag those indices are only listed: a
    /// `{prefix}-*` index nothing names is not assumed to be autoindex's.
    pub uncataloged: bool,
}

pub struct RemoveReport {
    pub exit_code: i32,
    pub body: Value,
    pub summary: String,
}

pub fn execute(req: RemoveRequest) -> Result<RemoveReport> {
    let es = Es::new(&req.url, req.api_key.clone())?;
    es.ping().with_context(|| {
        format!(
            "autoindex remove: no node at {} — start one, then rerun the same command",
            req.url
        )
    })?;
    let docs = fetch_catalog(&es, &req.prefix)?;
    let aborted = discover_aborted(&req);
    let mut mentioned = index_names_of(&docs, &req.prefix);
    for item in &aborted {
        if deletable_index(&item.name) {
            mentioned.insert(item.name.clone());
        }
    }
    let live = fetch_live(&es, &mentioned)?;
    let mut plan = plan_remove(&req.prefix, req.root.as_deref(), &docs, &live, &aborted);
    if plan.empty && req.root.is_none() {
        // Best effort: a failed listing keeps the old "nothing to remove".
        let names =
            uncataloged_index_names(&es, &req.prefix, &docs, &mentioned).unwrap_or_default();
        if !names.is_empty() {
            plan.uncataloged = fetch_live(&es, &names)?;
            if req.uncataloged {
                select_uncataloged(&mut plan);
            }
        }
    }
    if plan.needs_root || plan.root_not_found || plan.empty {
        let code = if plan.empty && !plan.uncataloged.is_empty() {
            // A decision is required: `--uncataloged` names these indices.
            EXIT_NEEDS_DECISION
        } else if plan.empty || plan.root_not_found {
            1
        } else {
            EXIT_NEEDS_DECISION
        };
        return Ok(report(&plan, code, false, None));
    }
    let confirmed = if req.yes {
        true
    } else if req.prompt {
        eprint!("{}", prompt_line(&plan));
        let _ = std::io::stderr().flush();
        read_yes()
    } else {
        false
    };
    if !confirmed {
        return Ok(report(&plan, EXIT_NEEDS_DECISION, false, None));
    }
    match apply(&es, &plan) {
        Ok(()) => {
            let root = selected_root_display(&plan);
            let journals = match root.as_deref() {
                Some(root) => {
                    discard_resume_journals(root, &req.url, &req.prefix, req.state_dir.as_deref())
                }
                None => Ok(Vec::new()),
            };
            match journals {
                Ok(removed) => {
                    let mut out = report(&plan, 0, true, None);
                    out.body["resume_journals_removed"] = json!(removed);
                    if !removed.is_empty() {
                        out.summary
                            .push_str(&format!("\nRemoved resume state: {}.", removed.join(", ")));
                    }
                    Ok(out)
                }
                Err(err) => Ok(report(
                    &plan,
                    1,
                    true,
                    Some(format!(
                        "the indices and catalog rows were deleted, but the resume journal was \
                         not: {err:#}. The next autoindex of this folder will abort on that \
                         journal until it is removed"
                    )),
                )),
            }
        }
        Err(err) => {
            let left = match refetch(&es, &req) {
                Ok(left) => Some(left),
                Err(read_err) => {
                    return Ok(report(
                        &plan,
                        1,
                        false,
                        Some(format!(
                            "{err:#}\nThe follow-up read of what is left also failed: {read_err:#}"
                        )),
                    ));
                }
            };
            let mut out = report(&plan, 1, false, Some(format!("{err:#}")));
            if let Some(left) = left {
                out.body["remaining"] = plan_body(&left, 1, false, None);
            }
            out.summary.push_str(
                "\nA delete failed partway. `remaining` in the JSON is what the catalog \
                 and the indices still say is there. Nothing further was attempted.",
            );
            Ok(out)
        }
    }
}

fn refetch(es: &Es, req: &RemoveRequest) -> Result<RemovePlan> {
    let docs = fetch_catalog(es, &req.prefix)?;
    let aborted = discover_aborted(req);
    let mut mentioned = index_names_of(&docs, &req.prefix);
    for item in &aborted {
        if deletable_index(&item.name) {
            mentioned.insert(item.name.clone());
        }
    }
    let live = fetch_live(es, &mentioned)?;
    Ok(plan_remove(
        &req.prefix,
        req.root.as_deref(),
        &docs,
        &live,
        &aborted,
    ))
}

fn prompt_line(plan: &RemovePlan) -> String {
    format!(
        "Remove {} index(es) and {} catalog row(s) for prefix {}? [y/N] ",
        plan.indices.len(),
        plan.rows.len(),
        plan.prefix
    )
}

fn read_yes() -> bool {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

/// What `remove` will delete for `prefix`, given the catalog and live stats.
///
/// Pure: no HTTP. `root_filter` of `None` selects the only root, and refuses
/// when there is more than one. Indices another remaining root still names,
/// and `autoindex-catalog`, are never in the delete set.
///
/// `aborted` is the unfinished resume journal's index list (#2391). Those
/// indices are deleted for the folder the journal names, unless a catalog
/// row this plan is keeping still names them.
pub fn plan_remove(
    prefix: &str,
    root_filter: Option<&str>,
    docs: &[CatalogDoc],
    live: &[LiveIndex],
    aborted: &[AbortedIndex],
) -> RemovePlan {
    let mut by_id: BTreeMap<&str, &CatalogDoc> = BTreeMap::new();
    for doc in docs {
        by_id.entry(doc.id.as_str()).or_insert(doc);
    }
    let docs: Vec<&CatalogDoc> = by_id.into_values().collect();

    let mut groups: BTreeMap<String, RootGroup> = BTreeMap::new();
    for doc in &docs {
        if kind(doc) != "run" || !run_matches_prefix(doc, prefix) {
            continue;
        }
        let recorded = doc.source.get("root").and_then(Value::as_str).unwrap_or("");
        let display = if recorded.is_empty() {
            catalog::UNRECORDED_PREFIX_ROOT.to_string()
        } else {
            recorded.to_string()
        };
        let key = root_key(&display);
        let started = doc
            .source
            .get("started")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let group = groups.entry(key.clone()).or_insert_with(|| RootGroup {
            display: display.clone(),
            key,
            run_ids: BTreeSet::new(),
            last_started: String::new(),
            datasets: Vec::new(),
            from_journal: false,
        });
        if started > group.last_started {
            group.last_started = started;
            group.display = display;
        }
        group.run_ids.insert(run_identity(doc));
        group.run_ids.insert(doc.id.clone());
    }

    // #2391: an aborted build's resume journal is a root even when no catalog
    // run was written. The indices land on that root's dataset list as
    // `orphan` rows; they are not catalog ids and are never bulk-deleted.
    let siblings = sibling_prefixes_of(&docs, prefix);
    let mut orphan_names: BTreeSet<String> = BTreeSet::new();
    for item in aborted {
        if item.prefix != prefix || !index_owned_by_prefix(&item.name, prefix, &siblings) {
            continue;
        }
        if !deletable_index(&item.name) {
            continue;
        }
        orphan_names.insert(item.name.clone());
        let key = root_key(&item.root);
        let group = groups.entry(key.clone()).or_insert_with(|| RootGroup {
            display: item.root.clone(),
            key,
            run_ids: BTreeSet::new(),
            last_started: String::new(),
            datasets: Vec::new(),
            from_journal: true,
        });
        if group.datasets.iter().any(|row| row.index_name == item.name) {
            continue;
        }
        group.datasets.push(Row {
            id: format!("journal:{}:{}", item.prefix, item.name),
            doc_kind: "orphan".into(),
            root: group.display.clone(),
            index_name: item.name.clone(),
            record_count: 0,
            bytes: 0,
            file_count: 0,
        });
    }

    let mut roots: Vec<RootGroup> = groups.into_values().collect();
    // Datasets with no run document still describe a folder. One implicit
    // root keeps `remove --prefix` able to drop them; it does not invent a
    // second folder.
    if roots.is_empty()
        && docs.iter().any(|doc| {
            catalog::dataset_matches_prefix(&doc.source, prefix)
                || catalog::file_matches_prefix(&doc.source, prefix)
                || catalog::correlation_matches_prefix(&doc.source, prefix)
        })
    {
        roots.push(RootGroup {
            display: catalog::UNRECORDED_PREFIX_ROOT.to_string(),
            key: root_key(catalog::UNRECORDED_PREFIX_ROOT),
            run_ids: BTreeSet::new(),
            last_started: String::new(),
            datasets: Vec::new(),
            from_journal: false,
        });
    }
    roots.sort_by(|a, b| a.display.cmp(&b.display));
    attach_root_datasets(&mut roots, &docs, prefix);

    let (selected_key, needs_root, root_not_found) = if let Some(wanted) = root_filter {
        let key = root_key(wanted);
        if roots.iter().any(|g| g.key == key) {
            (Some(key), false, false)
        } else {
            (None, false, true)
        }
    } else if roots.len() > 1 {
        (None, true, false)
    } else {
        (roots.first().map(|g| g.key.clone()), false, false)
    };

    let selected_ids: BTreeSet<&str> = selected_key
        .as_deref()
        .and_then(|key| roots.iter().find(|g| g.key == key))
        .map(|g| g.run_ids.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let single = roots.len() <= 1;
    let deciding = selected_key.is_some() && !needs_root && !root_not_found;

    // Runs of another prefix, by run id, and their prefixes. A `{prefix}-`
    // match on a dataset name is not ownership: `ax-2-docs` starts with `ax-`.
    // A row whose run belongs to another prefix, or a slug-less row whose
    // index sits under a longer sibling prefix, is that prefix's, not ours.
    let mut foreign_runs: BTreeSet<String> = BTreeSet::new();
    let mut sibling_prefixes: BTreeSet<String> = BTreeSet::new();
    for doc in &docs {
        if kind(doc) != "run" || run_matches_prefix(doc, prefix) {
            continue;
        }
        foreign_runs.insert(run_identity(doc));
        foreign_runs.insert(doc.id.clone());
        if let Some(other) = doc
            .source
            .get("prefix")
            .or_else(|| doc.source.get(catalog::CORPUS_SCOPE_FIELD))
            .and_then(Value::as_str)
        {
            if other.len() > prefix.len() && other.starts_with(&format!("{prefix}-")) {
                sibling_prefixes.insert(other.to_string());
            }
        }
    }
    let foreign = |source: &Value| -> bool {
        let run_id = source.get("run_id").and_then(Value::as_str).unwrap_or("");
        if !run_id.is_empty() && foreign_runs.contains(run_id) {
            return true;
        }
        let slugless = source
            .get("slug")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty);
        let names: Vec<&str> = ["index_name", "a_index", "b_index"]
            .iter()
            .filter_map(|k| source.get(*k).and_then(Value::as_str))
            .collect();
        slugless
            && names.iter().any(|name| {
                sibling_prefixes
                    .iter()
                    .any(|other| name.starts_with(&format!("{other}-")))
            })
    };

    let mut rows = Vec::new();
    let mut kept_rows = Vec::new();
    let mut delete_index_names: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut kept_index_reasons: BTreeMap<String, String> = BTreeMap::new();

    for doc in &docs {
        let doc_kind = kind(doc);
        if doc_kind == "run" {
            if !run_matches_prefix(doc, prefix) {
                // Another prefix. Kept so an edges index it shares is not deleted.
                kept_rows.push(row_of(doc, ""));
                continue;
            }
            let key = root_key(
                doc.source
                    .get("root")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(catalog::UNRECORDED_PREFIX_ROOT),
            );
            if deciding && selected_key.as_deref() == Some(key.as_str()) {
                rows.push(row_of(doc, &display_of(&roots, &key)));
            } else if !deciding {
                // Shown under roots; not a delete until a root is chosen.
            } else {
                kept_rows.push(row_of(doc, &display_of(&roots, &key)));
            }
            continue;
        }

        let belongs = match doc_kind {
            "dataset" => catalog::dataset_matches_prefix(&doc.source, prefix),
            "file" => catalog::file_matches_prefix(&doc.source, prefix),
            "correlation" => catalog::correlation_matches_prefix(&doc.source, prefix),
            _ => false,
        };
        if !belongs || foreign(&doc.source) {
            continue;
        }
        let owned = owns_row(&doc.source, &selected_ids, single);
        if deciding && owned {
            let row = row_of(doc, "");
            if doc_kind == "dataset" && !row.index_name.is_empty() {
                let entry = delete_index_names
                    .entry(row.index_name.clone())
                    .or_insert((0, 0));
                entry.0 += row.record_count;
                entry.1 += row.bytes;
            }
            rows.push(row);
        } else if deciding {
            let row = row_of(doc, "");
            if doc_kind == "dataset" && !row.index_name.is_empty() {
                kept_index_reasons
                    .entry(row.index_name.clone())
                    .or_insert_with(|| format!("still named by catalog row {}", row.id));
            }
            kept_rows.push(row);
        }
    }

    // Edges indices are not `{prefix}-*`. Delete one only when every run that
    // names it is being deleted — a sibling prefix's brain is not this folder.
    if deciding {
        let mut edges_kept: BTreeSet<String> = BTreeSet::new();
        let mut edges_drop: BTreeSet<String> = BTreeSet::new();
        for doc in &docs {
            if kind(doc) != "run" {
                continue;
            }
            let Some(edges) = edges_index(&doc.source) else {
                continue;
            };
            let dropping = rows.iter().any(|row| row.id == doc.id);
            if dropping {
                edges_drop.insert(edges);
            } else {
                edges_kept.insert(edges);
            }
        }
        for name in edges_drop {
            if edges_kept.contains(&name) {
                kept_index_reasons
                    .entry(name)
                    .or_insert_with(|| "another catalog run still names this edges index".into());
            } else {
                delete_index_names.entry(name).or_insert((0, 0));
            }
        }
    }

    // Journal indices for the selected folder. A name `kept_index_reasons`
    // already holds (another root's catalog row, a shared edges index) is
    // removed again just below, so the preserved working index stays.
    if deciding {
        if let Some(key) = selected_key.as_deref() {
            if let Some(group) = roots.iter().find(|g| g.key == key) {
                for row in &group.datasets {
                    if row.doc_kind == "orphan" && orphan_names.contains(&row.index_name) {
                        delete_index_names
                            .entry(row.index_name.clone())
                            .or_insert((0, 0));
                    }
                }
            }
        }
    }

    for name in kept_index_reasons.keys() {
        delete_index_names.remove(name);
    }

    let catalog_named_in_rows: BTreeSet<&str> = rows
        .iter()
        .filter(|row| row.doc_kind == "dataset" || row.doc_kind == "correlation")
        .map(|row| row.index_name.as_str())
        .collect();
    let mut indices = Vec::new();
    let mut kept_indices = Vec::new();
    for (name, (record_count, catalog_bytes)) in &delete_index_names {
        if !deletable_index(name) {
            kept_indices.push(Kept {
                name: name.clone(),
                reason: "this command never deletes autoindex-catalog or a wildcard".into(),
            });
            continue;
        }
        let found = live.iter().find(|idx| idx.name == *name);
        indices.push(PlannedIndex {
            name: name.clone(),
            live_docs: found.map(|idx| idx.docs),
            store_bytes: found.map(|idx| idx.store_bytes),
            record_count: *record_count,
            catalog_bytes: *catalog_bytes,
            orphan: orphan_names.contains(name) && !catalog_named_in_rows.contains(name.as_str()),
        });
    }
    for (name, reason) in kept_index_reasons {
        if indices.iter().any(|idx| idx.name == name) {
            continue;
        }
        kept_indices.push(Kept { name, reason });
    }
    indices.sort_by(|a, b| a.name.cmp(&b.name));
    kept_indices.sort_by(|a, b| a.name.cmp(&b.name));
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    kept_rows.sort_by(|a, b| a.id.cmp(&b.id));

    let empty = if root_not_found || needs_root {
        false
    } else if deciding {
        rows.is_empty() && indices.is_empty()
    } else {
        roots.is_empty()
    };

    RemovePlan {
        prefix: prefix.to_string(),
        roots,
        selected_key,
        needs_root,
        root_not_found,
        empty,
        indices,
        kept_indices,
        rows,
        kept_rows,
        uncataloged: Vec::new(),
        uncataloged_selected: false,
    }
}

fn attach_root_datasets(roots: &mut [RootGroup], docs: &[&CatalogDoc], prefix: &str) {
    for doc in docs {
        if kind(doc) != "dataset" || !catalog::dataset_matches_prefix(&doc.source, prefix) {
            continue;
        }
        let run_id = doc
            .source
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let row = row_of(doc, "");
        if roots.len() == 1 {
            roots[0].datasets.push(row);
            continue;
        }
        if let Some(group) = roots
            .iter_mut()
            .find(|g| !run_id.is_empty() && g.run_ids.contains(run_id))
        {
            group.datasets.push(row);
        }
    }
    for group in roots {
        group.datasets.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

fn display_of(roots: &[RootGroup], key: &str) -> String {
    roots
        .iter()
        .find(|g| g.key == key)
        .map(|g| g.display.clone())
        .unwrap_or_default()
}

fn owns_row(source: &Value, selected_ids: &BTreeSet<&str>, single_root: bool) -> bool {
    let run_id = source.get("run_id").and_then(Value::as_str).unwrap_or("");
    if !run_id.is_empty() && selected_ids.contains(run_id) {
        return true;
    }
    // One folder under the prefix: a legacy row with no run_id, and a row
    // whose run_id no longer matches, still belong to that folder. Two
    // folders: a row we cannot attribute is left alone.
    single_root
}

fn deletable_index(name: &str) -> bool {
    !name.is_empty()
        && name != CATALOG_INDEX
        && name != "_all"
        && !name.contains('*')
        && !name.contains('?')
}

/// Prefixes longer than `prefix` that a catalog run records (`ax-2` when this
/// command is removing `ax`). `ax-2-docs` starts with `ax-` and is not ours.
fn sibling_prefixes_of(docs: &[&CatalogDoc], prefix: &str) -> BTreeSet<String> {
    let mut siblings = BTreeSet::new();
    for doc in docs {
        if kind(doc) != "run" || run_matches_prefix(doc, prefix) {
            continue;
        }
        let Some(other) = doc
            .source
            .get("prefix")
            .or_else(|| doc.source.get(catalog::CORPUS_SCOPE_FIELD))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if other.len() > prefix.len() && other.starts_with(&format!("{prefix}-")) {
            siblings.insert(other.to_string());
        }
    }
    siblings
}

/// `{prefix}-{slug}`, and not `{sibling}-{slug}` for a longer prefix.
fn index_owned_by_prefix(name: &str, prefix: &str, siblings: &BTreeSet<String>) -> bool {
    let scoped = format!("{prefix}-");
    if !name.starts_with(&scoped) {
        return false;
    }
    !siblings
        .iter()
        .any(|other| name.starts_with(&format!("{other}-")))
}

fn same_endpoint(recorded: &str, current: &str) -> bool {
    fn norm(s: &str) -> &str {
        s.trim().trim_end_matches('/')
    }
    norm(recorded) == norm(current)
}

fn collect_dataset_indices(plan: &Value, into: &mut BTreeSet<String>) {
    let Some(datasets) = plan.get("datasets").and_then(Value::as_array) else {
        return;
    };
    for dataset in datasets {
        if let Some(name) = dataset.get("index").and_then(Value::as_str) {
            if !name.is_empty() {
                into.insert(name.to_string());
            }
        }
    }
}

/// Index names an unfinished journal attributes to `prefix`.
///
/// A journal that ended in `finish` or `sync_commit` published a catalog run;
/// its indices belong to that run, and this returns nothing. A `resume` or
/// `sync_begin` after that opens the build again. `allow_missing_url` accepts
/// a journal written before `url` was recorded; a home-directory scan passes
/// false so an old journal cannot be claimed by every endpoint.
pub fn aborted_indices_from_journal(
    text: &str,
    prefix: &str,
    url: &str,
    allow_missing_url: bool,
) -> Vec<AbortedIndex> {
    unfinished_journal_indices(text, url, allow_missing_url)
        .into_iter()
        .filter(|item| item.prefix == prefix)
        .collect()
}

/// Every index an unfinished journal names, under the prefix the journal recorded.
///
/// `status` uses this so a leftover of a prefix other than the one `status`
/// was asked about is still flagged. `remove` filters to its own prefix.
pub fn unfinished_journal_indices(
    text: &str,
    url: &str,
    allow_missing_url: bool,
) -> Vec<AbortedIndex> {
    let mut open = false;
    let mut saw_run = false;
    let mut root = String::new();
    let mut recorded_url = String::new();
    let mut recorded_prefix = String::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("kind").and_then(Value::as_str) {
            Some("run") => {
                saw_run = true;
                open = true;
                root = value
                    .get("root")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                recorded_url = value
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                recorded_prefix = value
                    .get("prefix")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
            Some("resume") | Some("sync_abort") => open = true,
            Some("plan") if open => {
                if let Some(plan) = value.get("plan") {
                    collect_dataset_indices(plan, &mut names);
                }
            }
            Some("index_created") if open => {
                if let Some(name) = value.get("index").and_then(Value::as_str) {
                    if !name.is_empty() {
                        names.insert(name.to_string());
                    }
                }
            }
            Some("sync_begin") => {
                open = true;
                if let Some(plan) = value.pointer("/desired/plan") {
                    collect_dataset_indices(plan, &mut names);
                }
            }
            Some("finish") | Some("sync_commit") => open = false,
            _ => {}
        }
    }
    if !saw_run || !open || recorded_prefix.is_empty() || root.is_empty() {
        return Vec::new();
    }
    if recorded_url.is_empty() {
        if !allow_missing_url {
            return Vec::new();
        }
    } else if !same_endpoint(&recorded_url, url) {
        return Vec::new();
    }
    names
        .into_iter()
        .map(|name| AbortedIndex {
            name,
            root: root.clone(),
            prefix: recorded_prefix.clone(),
        })
        .collect()
}

fn autoindex_state_root() -> PathBuf {
    xerj_common::home::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".xerj")
        .join("autoindex")
}

/// Unfinished journals for this prefix and endpoint.
///
/// Explicit `--state-dir` and the default directory for `--root` are read
/// even when the journal predates the `url` field. Every other state
/// directory under `~/.xerj/autoindex` counts only when its `run` record
/// names this endpoint, so a scan cannot adopt another node's build.
fn discover_aborted(req: &RemoveRequest) -> Vec<AbortedIndex> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    let mut consider = |path: PathBuf, allow_missing_url: bool| {
        if !seen.insert(path.clone()) {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        found.extend(aborted_indices_from_journal(
            &text,
            &req.prefix,
            &req.url,
            allow_missing_url,
        ));
    };
    if let Some(dir) = &req.state_dir {
        consider(dir.join("journal.ndjson"), true);
    }
    if let Some(root) = &req.root {
        for dir in resume_state_dirs(root, &req.url, &req.prefix, None) {
            consider(dir.join("journal.ndjson"), true);
        }
    }
    if let Ok(entries) = std::fs::read_dir(autoindex_state_root()) {
        for entry in entries.flatten() {
            consider(entry.path().join("journal.ndjson"), false);
        }
    }
    let mut dedup: BTreeMap<(String, String), AbortedIndex> = BTreeMap::new();
    for item in found {
        dedup
            .entry((root_key(&item.root), item.name.clone()))
            .or_insert(item);
    }
    dedup.into_values().collect()
}

/// Live indices an aborted build left, and that no catalog dataset names.
///
/// `cataloged` is the set of index names a completed catalog run of this
/// prefix still claims. The working index is in that set and is not flagged.
pub fn orphan_flags(
    live: &[String],
    cataloged: &BTreeSet<String>,
    aborted: &[AbortedIndex],
) -> Vec<(String, String)> {
    let mut by_name: BTreeMap<&str, &str> = BTreeMap::new();
    for item in aborted {
        by_name
            .entry(item.name.as_str())
            .or_insert(item.root.as_str());
    }
    let mut flags = Vec::new();
    for name in live {
        if cataloged.contains(name) {
            continue;
        }
        let Some(root) = by_name.get(name.as_str()) else {
            continue;
        };
        flags.push((name.clone(), (*root).to_string()));
    }
    flags.sort();
    flags
}

/// One `status` line per orphan root: the indices and the remove command.
pub fn orphan_status_lines(prefix: &str, flags: &[(String, String)]) -> Vec<String> {
    let mut by_root: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, root) in flags {
        by_root
            .entry(root.as_str())
            .or_default()
            .push(name.as_str());
    }
    let mut lines = Vec::new();
    for (root, names) in by_root {
        lines.push(format!(
            "orphans (aborted build, no catalog run) under {root}: {}",
            names.join(", ")
        ));
        lines.push(format!(
            "  xerj autoindex remove --prefix {} --root {} --yes",
            shell_quote(prefix),
            shell_quote(root)
        ));
    }
    lines
}

fn shell_quote(s: &str) -> String {
    if s.contains([' ', '\t', '\'', '"', '\\', '$']) {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

fn kind(doc: &CatalogDoc) -> &str {
    doc.source
        .get("doc_kind")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn run_matches_prefix(doc: &CatalogDoc, prefix: &str) -> bool {
    doc.source
        .get("prefix")
        .or_else(|| doc.source.get(catalog::CORPUS_SCOPE_FIELD))
        .and_then(Value::as_str)
        == Some(prefix)
}

fn run_identity(doc: &CatalogDoc) -> String {
    doc.source
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| doc.id.clone())
}

fn edges_index(source: &Value) -> Option<String> {
    source
        .pointer("/graph/edges_index")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn row_of(doc: &CatalogDoc, root: &str) -> Row {
    let index_name = doc
        .source
        .get("index_name")
        .or_else(|| doc.source.get("a_index"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let root = if root.is_empty() {
        doc.source
            .get("root")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    } else {
        root.to_string()
    };
    Row {
        id: doc.id.clone(),
        doc_kind: kind(doc).to_string(),
        root,
        index_name,
        record_count: u64_of(&doc.source, "record_count")
            .or_else(|| u64_of(&doc.source, "records"))
            .unwrap_or(0),
        bytes: u64_of(&doc.source, "bytes").unwrap_or(0),
        file_count: u64_of(&doc.source, "file_count").unwrap_or(0),
    }
}

fn u64_of(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

/// Compare a catalog root and a `--root` argument. A path that exists is
/// canonicalized; a folder that has already been deleted still matches the
/// string the catalog stored.
pub fn root_key(root: &str) -> String {
    let trimmed = root.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed == catalog::UNRECORDED_PREFIX_ROOT {
        return catalog::UNRECORDED_PREFIX_ROOT.to_string();
    }
    let path = std::path::Path::new(trimmed);
    if path.exists() {
        if let Ok(canon) = path.canonicalize() {
            let text = canon.to_string_lossy();
            return text.trim_end_matches(['/', '\\']).to_string();
        }
    }
    trimmed.to_string()
}

fn index_names_of(docs: &[CatalogDoc], prefix: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for doc in docs {
        if catalog::dataset_matches_prefix(&doc.source, prefix) {
            if let Some(name) = doc.source.get("index_name").and_then(Value::as_str) {
                if deletable_index(name) {
                    names.insert(name.to_string());
                }
            }
        }
        if kind(doc) == "run" && run_matches_prefix(doc, prefix) {
            if let Some(name) = edges_index(&doc.source) {
                if deletable_index(&name) {
                    names.insert(name);
                }
            }
        }
    }
    names
}

/// Live `{prefix}-*` indices that no catalog row read for this command names
/// (dataset rows of every prefix, run edges, correlation sides), that are not
/// a longer sibling prefix's, and that `remove` may delete (#2405).
fn uncataloged_index_names(
    es: &Es,
    prefix: &str,
    docs: &[CatalogDoc],
    mentioned: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let live = es.cat_indices_json(&format!("{prefix}-*"))?;
    let refs: Vec<&CatalogDoc> = docs.iter().collect();
    Ok(uncataloged_among(prefix, &live, &refs, mentioned))
}

fn uncataloged_among(
    prefix: &str,
    live: &[String],
    docs: &[&CatalogDoc],
    mentioned: &BTreeSet<String>,
) -> BTreeSet<String> {
    let siblings = sibling_prefixes_of(docs, prefix);
    let mut claimed: BTreeSet<String> = mentioned.clone();
    for doc in docs {
        for key in ["index_name", "a_index", "b_index"] {
            if let Some(name) = doc.source.get(key).and_then(Value::as_str) {
                claimed.insert(name.to_string());
            }
        }
        if let Some(name) = edges_index(&doc.source) {
            claimed.insert(name);
        }
        if let Some(plan) = doc.source.get("plan") {
            collect_dataset_indices(plan, &mut claimed);
        }
    }
    live.iter()
        .filter(|name| deletable_index(name))
        .filter(|name| !name.starts_with('.'))
        .filter(|name| index_owned_by_prefix(name, prefix, &siblings))
        .filter(|name| !claimed.contains(*name))
        .cloned()
        .collect()
}

/// Turn an empty plan into one that deletes its uncataloged indices.
fn select_uncataloged(plan: &mut RemovePlan) {
    plan.indices = plan
        .uncataloged
        .iter()
        .map(|live| PlannedIndex {
            name: live.name.clone(),
            live_docs: Some(live.docs),
            store_bytes: Some(live.store_bytes),
            record_count: 0,
            catalog_bytes: 0,
            orphan: true,
        })
        .collect();
    plan.uncataloged_selected = true;
    plan.empty = plan.indices.is_empty();
}

fn fetch_catalog(es: &Es, prefix: &str) -> Result<Vec<CatalogDoc>> {
    let mut docs = Vec::new();
    docs.extend(search_kind(es, json!({"term": {"doc_kind": "run"}}))?);
    let scoped = format!("{prefix}-");
    docs.extend(search_kind(
        es,
        json!({"bool": {"filter": [
            {"term": {"doc_kind": "dataset"}},
            {"prefix": {"index_name": scoped}}
        ]}}),
    )?);
    docs.extend(search_kind(
        es,
        json!({"bool": {"filter": [
            {"term": {"doc_kind": "file"}},
            {"bool": {"should": [
                {"term": {"prefix": prefix}},
                {"term": {catalog::CORPUS_SCOPE_FIELD: prefix}}
            ], "minimum_should_match": 1}}
        ]}}),
    )?);
    docs.extend(search_kind(
        es,
        json!({"bool": {"filter": [
            {"term": {"doc_kind": "correlation"}},
            {"prefix": {"a_index": scoped}}
        ]}}),
    )?);
    Ok(docs)
}

/// Index names a catalog dataset row of `prefix` still claims.
///
/// A missing catalog is an empty set (the aborted build). Any other search
/// failure is returned so a caller can refuse to call the working index an
/// orphan just because the catalog could not be read.
pub(crate) fn cataloged_index_names(es: &Es, prefix: &str) -> Result<BTreeSet<String>> {
    let docs = search_kind(
        es,
        json!({"bool": {"filter": [
            {"term": {"doc_kind": "dataset"}},
            {"prefix": {"index_name": format!("{prefix}-")}}
        ]}}),
    )?;
    let mut names = BTreeSet::new();
    for doc in docs {
        if !catalog::dataset_matches_prefix(&doc.source, prefix) {
            continue;
        }
        if let Some(name) = doc.source.get("index_name").and_then(Value::as_str) {
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names)
}

fn search_kind(es: &Es, query: Value) -> Result<Vec<CatalogDoc>> {
    let mut from = 0usize;
    let mut out = Vec::new();
    loop {
        if from >= WINDOW {
            anyhow::bail!(
                "autoindex remove: the catalog page is larger than {WINDOW} documents, so this \
                 command cannot see every row. Nothing was deleted."
            );
        }
        let body = json!({
            "query": query,
            "size": PAGE,
            "from": from,
            "sort": [{"_id": "asc"}]
        });
        let Some(page) = es.search_page_present(CATALOG_INDEX, &body)? else {
            return Ok(Vec::new());
        };
        let hits = page
            .pointer("/hits/hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if hits.is_empty() {
            break;
        }
        for hit in &hits {
            let id = hit
                .get("_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let source = hit.get("_source").cloned().unwrap_or(Value::Null);
            out.push(CatalogDoc { id, source });
        }
        let total = hit_total(&page);
        from += hits.len();
        if hits.len() < PAGE || total.is_some_and(|n| from as u64 >= n) {
            break;
        }
        if from >= WINDOW {
            anyhow::bail!(
                "autoindex remove: read {WINDOW} catalog documents and the page was still full. \
                 Nothing was deleted."
            );
        }
    }
    Ok(out)
}

fn hit_total(page: &Value) -> Option<u64> {
    let total = page.pointer("/hits/total")?;
    total
        .as_u64()
        .or_else(|| total.get("value").and_then(Value::as_u64))
}

fn fetch_live(es: &Es, names: &BTreeSet<String>) -> Result<Vec<LiveIndex>> {
    let mut out = Vec::new();
    for name in names {
        let path = format!("/{}/_stats", path_escape(name));
        let (status, body) = es.request_json("GET", &path, None)?;
        if status == 404 {
            continue;
        }
        if !(200..300).contains(&status) {
            // A stats miss is not a reason to guess a size. The plan still
            // lists the index from the catalog; live numbers stay absent.
            continue;
        }
        let stats = body
            .pointer(&format!("/indices/{name}/primaries"))
            .or_else(|| body.pointer(&format!("/indices/{name}/total")))
            .or_else(|| body.pointer("/_all/primaries"));
        let docs = stats
            .and_then(|s| s.pointer("/docs/count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let store_bytes = stats
            .and_then(|s| s.pointer("/store/size_in_bytes"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        out.push(LiveIndex {
            name: name.clone(),
            docs,
            store_bytes,
        });
    }
    Ok(out)
}

fn apply(es: &Es, plan: &RemovePlan) -> Result<()> {
    for index in &plan.indices {
        if !deletable_index(&index.name) {
            anyhow::bail!("refusing to delete {}", index.name);
        }
        let path = format!("/{}", path_escape(&index.name));
        let (status, body) = es
            .request_json("DELETE", &path, None)
            .with_context(|| format!("DELETE {path}"))?;
        if status == 404 || (200..300).contains(&status) {
            continue;
        }
        anyhow::bail!("DELETE {path} returned HTTP {status}: {body}");
    }
    if plan.rows.is_empty() {
        return Ok(());
    }
    let mut body = Vec::new();
    for row in &plan.rows {
        if row.id == CATALOG_INDEX {
            anyhow::bail!("refusing to delete the {CATALOG_INDEX} index");
        }
        let action = json!({"delete": {"_index": CATALOG_INDEX, "_id": row.id}});
        body.extend_from_slice(action.to_string().as_bytes());
        body.push(b'\n');
    }
    let outcome = es.bulk(body).context("delete catalog rows")?;
    if outcome.item_errors > 0 || outcome.server_errors > 0 {
        // A missing row is already the end state we want. Retry each id and
        // treat 404 as gone. Anything else is the partway failure.
        for row in &plan.rows {
            let path = format!("/{CATALOG_INDEX}/_doc/{}", path_escape(&row.id));
            let (status, resp) = es.request_json("DELETE", &path, None)?;
            if status == 404 || (200..300).contains(&status) {
                continue;
            }
            anyhow::bail!(
                "catalog row {} was not deleted (HTTP {status}: {resp}). bulk said: {}",
                row.id,
                outcome
                    .first_error
                    .as_deref()
                    .or(outcome.first_server_error.as_deref())
                    .unwrap_or("unknown")
            );
        }
    }
    es.refresh(CATALOG_INDEX).ok();
    Ok(())
}

fn selected_root_display(plan: &RemovePlan) -> Option<String> {
    let key = plan.selected_key.as_ref()?;
    plan.roots
        .iter()
        .find(|group| group.key == *key)
        .map(|group| group.display.clone())
}

/// State directories whose `journal.ndjson` would make the next autoindex of
/// `root_display` resume a corpus this command just deleted (#2314).
fn resume_state_dirs(
    root_display: &str,
    url: &str,
    prefix: &str,
    explicit: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs = vec![crate::state::default_state_dir(root_display, url, prefix)];
    if let Ok(canonical) = std::fs::canonicalize(root_display) {
        let text = canonical.to_string_lossy();
        if text != root_display {
            dirs.push(crate::state::default_state_dir(&text, url, prefix));
        }
    }
    if let Some(explicit) = explicit {
        dirs.push(explicit.to_path_buf());
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Delete `journal.ndjson` and `sync-snapshots` for one removed folder.
///
/// The directory itself stays: it may hold notes the operator put there. A
/// missing journal is success. A journal for a different root is a different
/// directory, because the default path hashes the root.
pub fn discard_resume_journals(
    root_display: &str,
    url: &str,
    prefix: &str,
    explicit: Option<&Path>,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for dir in resume_state_dirs(root_display, url, prefix, explicit) {
        let journal = dir.join("journal.ndjson");
        match std::fs::remove_file(&journal) {
            Ok(()) => removed.push(journal.display().to_string()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                anyhow::bail!("remove {}: {err}", journal.display());
            }
        }
        let snapshots = dir.join("sync-snapshots");
        if snapshots.is_dir() {
            std::fs::remove_dir_all(&snapshots)
                .with_context(|| format!("remove {}", snapshots.display()))?;
            removed.push(snapshots.display().to_string());
        }
    }
    Ok(removed)
}

fn path_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn report(plan: &RemovePlan, exit_code: i32, deleted: bool, error: Option<String>) -> RemoveReport {
    let summary = summary_text(plan, exit_code, deleted, error.as_deref());
    RemoveReport {
        exit_code,
        body: plan_body(plan, exit_code, deleted, error),
        summary,
    }
}

fn plan_body(plan: &RemovePlan, exit_code: i32, deleted: bool, error: Option<String>) -> Value {
    let selected = plan.selected_key.as_ref().and_then(|key| {
        plan.roots
            .iter()
            .find(|g| g.key == *key)
            .map(|g| g.display.clone())
    });
    json!({
        "action": "autoindex-remove",
        "prefix": plan.prefix,
        "exit": exit_code,
        "deleted": deleted,
        "needs_root": plan.needs_root,
        "root_not_found": plan.root_not_found,
        "empty": plan.empty,
        "selected_root": selected,
        "roots": plan.roots.iter().map(|g| json!({
            "root": g.display,
            "last_started": if g.last_started.is_empty() { Value::Null } else { json!(g.last_started) },
            "from_journal": g.from_journal,
            "datasets": g.datasets.iter().map(row_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "remove": {
            "indices": plan.indices.iter().map(|idx| json!({
                "name": idx.name,
                "docs": idx.live_docs,
                "store_bytes": idx.store_bytes,
                "record_count": idx.record_count,
                "bytes": idx.catalog_bytes,
                "orphan": idx.orphan,
            })).collect::<Vec<_>>(),
            "catalog_rows": plan.rows.iter().map(row_json).collect::<Vec<_>>(),
        },
        "keep": {
            "indices": plan.kept_indices.iter().map(|idx| json!({
                "name": idx.name,
                "reason": idx.reason,
            })).collect::<Vec<_>>(),
            "catalog_rows": plan.kept_rows.iter().filter(|row| {
                // Runs from other prefixes are evidence for edges sharing, not
                // rows of this prefix the caller asked to see as "kept".
                row.doc_kind != "run" || plan.roots.iter().any(|g| g.display == row.root || g.key == root_key(&row.root))
            }).map(row_json).collect::<Vec<_>>(),
        },
        "uncataloged": plan.uncataloged.iter().map(|idx| json!({
            "name": idx.name,
            "docs": idx.docs,
            "store_bytes": idx.store_bytes,
        })).collect::<Vec<_>>(),
        "error": error,
    })
}

fn row_json(row: &Row) -> Value {
    json!({
        "id": row.id,
        "doc_kind": row.doc_kind,
        "root": if row.root.is_empty() { Value::Null } else { json!(row.root) },
        "index_name": if row.index_name.is_empty() { Value::Null } else { json!(row.index_name) },
        "record_count": row.record_count,
        "bytes": row.bytes,
        "file_count": row.file_count,
    })
}

fn summary_text(plan: &RemovePlan, exit_code: i32, deleted: bool, error: Option<&str>) -> String {
    let mut lines = Vec::new();
    lines.push(format!("autoindex remove: prefix {}", plan.prefix));
    if plan.roots.is_empty() {
        lines.push("no catalog run under this prefix.".into());
    } else {
        lines.push("roots:".into());
        for root in &plan.roots {
            let when = if root.last_started.is_empty() {
                String::new()
            } else {
                format!("  last {}", root.last_started)
            };
            let journal = if root.from_journal {
                "  (aborted build, no catalog run)"
            } else {
                ""
            };
            lines.push(format!("  {}{when}{journal}", root.display));
        }
    }
    if plan.needs_root {
        lines.push(
            "this prefix holds more than one folder. Nothing was deleted. Pass --root with one \
             path listed above. --yes does not choose a folder."
                .into(),
        );
    } else if plan.root_not_found {
        lines.push(
            "error: --root does not match a folder under this prefix. Nothing was deleted.".into(),
        );
    } else if plan.empty && !plan.uncataloged.is_empty() {
        lines.push(format!(
            "no resume journal under this prefix either. These live {}-* indices are named by \
             no catalog row (a build that never reached finalize, or indices made by hand):",
            plan.prefix
        ));
        for idx in &plan.uncataloged {
            lines.push(format!(
                "  index {}  live docs {}  store {}b",
                idx.name, idx.docs, idx.store_bytes
            ));
        }
        lines.push(format!(
            "Nothing was deleted. To delete exactly these, rerun:\n  xerj autoindex remove \
             --prefix {} --uncataloged --yes",
            shell_quote(&plan.prefix)
        ));
    } else if plan.empty {
        lines.push("error: nothing under this prefix to remove. Nothing was deleted.".into());
    } else {
        lines.push("will remove:".into());
        if plan.indices.is_empty() && plan.rows.is_empty() {
            lines.push("  (nothing)".into());
        }
        for idx in &plan.indices {
            let docs = idx
                .live_docs
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".into());
            let size = idx
                .store_bytes
                .map(|n| format!("{n}b"))
                .unwrap_or_else(|| "unknown".into());
            let orphan = if plan.uncataloged_selected {
                "  no catalog row, no resume journal"
            } else if idx.orphan {
                "  aborted build, no catalog run"
            } else {
                ""
            };
            lines.push(format!(
                "  index {}  live docs {docs}  store {size}  catalog records {}  catalog bytes {}{orphan}",
                idx.name, idx.record_count, idx.catalog_bytes
            ));
        }
        for row in &plan.rows {
            lines.push(format!(
                "  catalog {}  {}  records {}  bytes {}",
                row.id, row.doc_kind, row.record_count, row.bytes
            ));
        }
        if !plan.kept_indices.is_empty() {
            lines.push(
                "will keep (another folder under this prefix, or a shared edges index):".into(),
            );
            for idx in &plan.kept_indices {
                lines.push(format!("  index {}  {}", idx.name, idx.reason));
            }
        }
        if deleted {
            lines.push(format!(
                "removed {} index(es) and {} catalog row(s).",
                plan.indices.len(),
                plan.rows.len()
            ));
        } else if exit_code == EXIT_NEEDS_DECISION {
            lines.push(
                "Nothing was deleted. On a terminal, answer the prompt. Otherwise rerun with --yes."
                    .into(),
            );
        }
    }
    if let Some(error) = error {
        lines.push(format!("error: {error}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, source: Value) -> CatalogDoc {
        CatalogDoc {
            id: id.to_string(),
            source,
        }
    }

    fn run(id: &str, prefix: &str, root: &str, started: &str) -> CatalogDoc {
        doc(
            id,
            json!({
                "doc_kind": "run",
                "prefix": prefix,
                "root": root,
                "run_id": id.trim_start_matches("run:"),
                "started": started
            }),
        )
    }

    fn dataset(
        id: &str,
        index: &str,
        slug: &str,
        run_id: &str,
        records: u64,
        bytes: u64,
    ) -> CatalogDoc {
        doc(
            id,
            json!({
                "doc_kind": "dataset",
                "index_name": index,
                "slug": slug,
                "run_id": run_id,
                "record_count": records,
                "bytes": bytes,
                "file_count": 1
            }),
        )
    }

    fn two_roots() -> Vec<CatalogDoc> {
        vec![
            run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z"),
            run("run:b", "ax", "/data/b", "2026-02-01T00:00:00Z"),
            dataset("ds:ax:only-a", "ax-only-a", "only-a", "a", 2, 10),
            dataset("ds:ax:docs", "ax-docs", "docs", "b", 5, 50),
            doc(
                "file:ax:a",
                json!({"doc_kind":"file","prefix":"ax","run_id":"a","records":2,"bytes":10}),
            ),
            doc(
                "file:ax:b",
                json!({"doc_kind":"file","prefix":"ax","run_id":"b","records":5,"bytes":50}),
            ),
            dataset("ds:ax-2:docs", "ax-2-docs", "docs", "other", 9, 90),
            doc(
                "run:other",
                json!({
                    "doc_kind":"run","prefix":"ax-2","root":"/data/other",
                    "run_id":"other","started":"2026-03-01T00:00:00Z",
                    "graph": {"edges_index": ".xerj-memory-shared-edges"}
                }),
            ),
        ]
    }

    fn names(plan: &RemovePlan) -> Vec<String> {
        plan.indices.iter().map(|i| i.name.clone()).collect()
    }

    fn row_ids(plan: &RemovePlan) -> Vec<String> {
        plan.rows.iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn a_sibling_prefix_dataset_is_never_ours() {
        // A legacy row with no slug: `ax-2-docs` starts with `ax-`, but the
        // catalog names a run of prefix `ax-2` for it. Removing `ax` keeps it,
        // and keeps it even when the row carries no run id at all.
        let mut docs = vec![
            run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "a", 5, 50),
            doc(
                "run:other",
                json!({"doc_kind":"run","prefix":"ax-2","root":"/data/other",
                       "run_id":"other","started":"2026-03-01T00:00:00Z"}),
            ),
            doc(
                "ds:ax-2:docs",
                json!({"doc_kind":"dataset","index_name":"ax-2-docs","run_id":"other",
                       "record_count":9,"bytes":90}),
            ),
            doc(
                "ds:ax-2:code",
                json!({"doc_kind":"dataset","index_name":"ax-2-code",
                       "record_count":3,"bytes":30}),
            ),
        ];
        docs.push(dataset("ds:ax:code", "ax-code", "code", "a", 1, 10));
        let plan = plan_remove("ax", None, &docs, &[], &[]);
        assert!(!plan.needs_root, "{plan:?}");
        let n = names(&plan);
        assert_eq!(
            n,
            vec!["ax-code".to_string(), "ax-docs".to_string()],
            "{n:?}"
        );
        let ids = row_ids(&plan);
        assert!(!ids.iter().any(|id| id.starts_with("ds:ax-2")), "{ids:?}");
        assert!(!ids.contains(&"run:other".to_string()), "{ids:?}");
    }

    #[test]
    fn two_roots_without_a_root_delete_nothing() {
        let plan = plan_remove("ax", None, &two_roots(), &[], &[]);
        assert!(plan.needs_root, "{plan:?}");
        assert!(plan.indices.is_empty());
        assert!(plan.rows.is_empty());
        assert_eq!(plan.roots.len(), 2);
        let body = plan_body(&plan, 4, false, None);
        let roots: Vec<&str> = body["roots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["root"].as_str().unwrap())
            .collect();
        assert_eq!(roots, vec!["/data/a", "/data/b"]);
        let a = body["roots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["root"] == "/data/a")
            .unwrap();
        let indexes: Vec<&str> = a["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["index_name"].as_str().unwrap())
            .collect();
        assert_eq!(indexes, vec!["ax-only-a"]);
    }

    #[test]
    fn removing_one_root_leaves_the_other_folders_indices_and_rows() {
        let live = vec![
            LiveIndex {
                name: "ax-only-a".into(),
                docs: 2,
                store_bytes: 100,
            },
            LiveIndex {
                name: "ax-docs".into(),
                docs: 5,
                store_bytes: 500,
            },
        ];
        let plan = plan_remove("ax", Some("/data/a"), &two_roots(), &live, &[]);
        assert!(!plan.needs_root && !plan.root_not_found, "{plan:?}");
        assert_eq!(names(&plan), vec!["ax-only-a".to_string()]);
        assert!(plan.indices[0].live_docs == Some(2));
        assert!(plan.indices[0].store_bytes == Some(100));
        assert_eq!(plan.indices[0].record_count, 2);
        let ids = row_ids(&plan);
        assert!(ids.contains(&"ds:ax:only-a".into()), "{ids:?}");
        assert!(ids.contains(&"run:a".into()), "{ids:?}");
        assert!(ids.contains(&"file:ax:a".into()), "{ids:?}");
        assert!(!ids.iter().any(|id| id.contains("ds:ax:docs")), "{ids:?}");
        assert!(
            !ids.iter().any(|id| id == "run:b" || id == "file:ax:b"),
            "{ids:?}"
        );
        assert!(!names(&plan)
            .iter()
            .any(|n| n == "ax-docs" || n == "ax-2-docs" || n == CATALOG_INDEX));
        assert!(
            plan.kept_indices.iter().any(|k| k.name == "ax-docs"),
            "{:?}",
            plan.kept_indices
        );
        let kept_ids: Vec<_> = plan.kept_rows.iter().map(|r| r.id.as_str()).collect();
        assert!(kept_ids.contains(&"ds:ax:docs"), "{kept_ids:?}");
        assert!(kept_ids.contains(&"run:b"), "{kept_ids:?}");
        assert!(kept_ids.contains(&"file:ax:b"), "{kept_ids:?}");
    }

    /// The index that made #2114 expensive: both folders' catalog rows name
    /// `ax-docs`. Removing one folder deletes its rows and leaves the index,
    /// because the other folder still names it.
    #[test]
    fn an_index_another_root_still_names_is_not_deleted() {
        let docs = vec![
            run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z"),
            run("run:b", "ax", "/data/b", "2026-02-01T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "a", 2, 10),
            dataset("ds:ax:docs-b", "ax-docs", "docs", "b", 5, 50),
        ];
        let plan = plan_remove("ax", Some("/data/a"), &docs, &[], &[]);
        assert!(
            !names(&plan).iter().any(|n| n == "ax-docs"),
            "{:?}",
            names(&plan)
        );
        assert!(plan.kept_indices.iter().any(|k| k.name == "ax-docs"));
        let ids = row_ids(&plan);
        assert!(ids.contains(&"ds:ax:docs".into()), "{ids:?}");
        assert!(ids.contains(&"run:a".into()), "{ids:?}");
        assert!(!ids.contains(&"ds:ax:docs-b".into()), "{ids:?}");
        assert!(!ids.contains(&"run:b".into()), "{ids:?}");
    }

    #[test]
    fn a_shared_edges_index_is_not_deleted() {
        let mut docs = two_roots();
        docs[0].source["graph"] = json!({"edges_index": ".xerj-memory-shared-edges"});
        let plan = plan_remove("ax", Some("/data/a"), &docs, &[], &[]);
        assert!(
            !names(&plan).iter().any(|n| n.contains("edges")),
            "{:?}",
            names(&plan)
        );
        assert!(plan
            .kept_indices
            .iter()
            .any(|k| k.name == ".xerj-memory-shared-edges"));
    }

    #[test]
    fn an_edges_index_only_this_folder_names_is_removed() {
        let mut docs = vec![
            run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "a", 1, 1),
        ];
        docs[0].source["graph"] = json!({"edges_index": ".xerj-memory-a-edges"});
        let plan = plan_remove("ax", None, &docs, &[], &[]);
        assert!(
            names(&plan).contains(&".xerj-memory-a-edges".into()),
            "{:?}",
            names(&plan)
        );
        assert!(names(&plan).contains(&"ax-docs".into()));
    }

    #[test]
    fn the_catalog_index_is_never_a_delete_target() {
        let docs = vec![
            run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z"),
            dataset("ds:ax:catalog", CATALOG_INDEX, "catalog", "a", 1, 1),
        ];
        let plan = plan_remove("ax", None, &docs, &[], &[]);
        assert!(
            !names(&plan).iter().any(|n| n == CATALOG_INDEX),
            "{:?}",
            names(&plan)
        );
    }

    #[test]
    fn an_unknown_root_deletes_nothing() {
        let plan = plan_remove("ax", Some("/data/missing"), &two_roots(), &[], &[]);
        assert!(plan.root_not_found);
        assert!(plan.indices.is_empty());
        assert!(plan.rows.is_empty());
    }

    #[test]
    fn one_root_is_selected_without_passing_root() {
        let docs = vec![
            run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "a", 4, 40),
        ];
        let plan = plan_remove("ax", None, &docs, &[], &[]);
        assert!(!plan.needs_root, "{plan:?}");
        assert_eq!(names(&plan), vec!["ax-docs".to_string()]);
        assert_eq!(plan.indices[0].record_count, 4);
        assert_eq!(plan.indices[0].catalog_bytes, 40);
    }

    #[test]
    fn trailing_slash_on_root_matches_the_catalog_path() {
        let docs = vec![run("run:a", "ax", "/data/a", "2026-01-01T00:00:00Z")];
        let plan = plan_remove("ax", Some("/data/a/"), &docs, &[], &[]);
        assert!(!plan.root_not_found, "{plan:?}");
        assert!(row_ids(&plan).contains(&"run:a".into()));
    }

    /// The acceptance check that is not a planner property: without `--yes`
    /// and without a terminal, the HTTP client must not send DELETE.
    #[test]
    fn without_yes_the_node_receives_no_delete() {
        let (url, log) = spawn_catalog_server(&two_roots());
        let report = execute(RemoveRequest {
            url,
            api_key: None,
            prefix: "ax".into(),
            root: None,
            yes: false,
            prompt: false,
            state_dir: None,
            uncataloged: false,
        })
        .expect("plan");
        assert_eq!(report.exit_code, EXIT_NEEDS_DECISION, "{}", report.summary);
        assert_eq!(report.body["deleted"], false);
        assert_eq!(report.body["needs_root"], true);
        let roots = report.body["roots"].as_array().unwrap();
        assert_eq!(roots.len(), 2);
        let calls = log.lock().unwrap().clone();
        assert!(
            calls.iter().all(|c| !c.starts_with("DELETE ")),
            "a plan must not delete: {calls:?}"
        );
        assert!(report.summary.contains("/data/a"), "{}", report.summary);
        assert!(
            report.summary.contains("Nothing was deleted"),
            "{}",
            report.summary
        );
    }

    /// `--yes` confirms a plan. It does not pick a folder when the prefix
    /// holds more than one.
    #[test]
    fn yes_without_a_root_still_deletes_nothing_when_two_folders_share_the_prefix() {
        let (url, log) = spawn_catalog_server(&two_roots());
        let report = execute(RemoveRequest {
            url,
            api_key: None,
            prefix: "ax".into(),
            root: None,
            yes: true,
            prompt: false,
            state_dir: None,
            uncataloged: false,
        })
        .expect("plan");
        assert_eq!(report.exit_code, EXIT_NEEDS_DECISION, "{}", report.summary);
        assert_eq!(report.body["deleted"], false);
        assert_eq!(report.body["needs_root"], true);
        assert!(
            report.summary.contains("--yes does not choose a folder"),
            "{}",
            report.summary
        );
        let calls = log.lock().unwrap().clone();
        assert!(
            calls.iter().all(|c| !c.starts_with("DELETE ")),
            "choosing a folder is not optional: {calls:?}"
        );
    }

    #[test]
    fn yes_with_a_root_deletes_only_that_folders_index_and_rows() {
        let (url, log) = spawn_catalog_server(&two_roots());
        let report = execute(RemoveRequest {
            url,
            api_key: None,
            prefix: "ax".into(),
            root: Some("/data/a".into()),
            yes: true,
            prompt: false,
            state_dir: None,
            uncataloged: false,
        })
        .expect("remove");
        assert_eq!(report.exit_code, 0, "{}\n{}", report.summary, report.body);
        assert_eq!(report.body["deleted"], true);
        let calls = log.lock().unwrap().clone();
        let deletes: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("DELETE "))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(deletes, vec!["DELETE /ax-only-a".to_string()], "{calls:?}");
        let bulk = calls
            .iter()
            .find(|c| c.starts_with("BULK "))
            .cloned()
            .unwrap_or_default();
        assert!(bulk.contains("ds:ax:only-a"), "{bulk}");
        assert!(bulk.contains("run:a"), "{bulk}");
        assert!(!bulk.contains("ds:ax:docs"), "{bulk}");
        assert!(!bulk.contains("run:b"), "{bulk}");
        assert!(
            report.body["remove"]["indices"][0]["docs"] == 2,
            "{}",
            report.body
        );
        assert!(
            report.body["remove"]["indices"][0]["store_bytes"] == 100,
            "{}",
            report.body
        );
    }

    /// #2405: a build killed before finalize, with its journal gone, leaves
    /// live `{prefix}-*` indices and no catalog row. `remove --prefix` used to
    /// say "nothing under this prefix" and delete nothing.
    #[test]
    fn uncataloged_indices_are_listed_and_deleted_only_with_the_opt_in() {
        let other = vec![
            run("run:o", "zq-2", "/data/o", "2026-01-01T00:00:00Z"),
            dataset("ds:zq-2:docs", "zq-2-docs", "docs", "o", 1, 1),
        ];
        let live = ["zq-docs", "zq-code", "zq-2-docs"];
        let request = |url: String, uncataloged: bool| RemoveRequest {
            url,
            api_key: None,
            prefix: "zq".into(),
            root: None,
            yes: true,
            prompt: false,
            state_dir: None,
            uncataloged,
        };

        let (url, log) = spawn_catalog_server_with(&other, &live);
        let report = execute(request(url, false)).expect("plan");
        assert_eq!(report.exit_code, EXIT_NEEDS_DECISION, "{}", report.summary);
        assert_eq!(report.body["deleted"], false);
        let listed: Vec<&str> = report.body["uncataloged"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert_eq!(listed, vec!["zq-code", "zq-docs"], "{}", report.summary);
        assert!(
            report
                .summary
                .contains("xerj autoindex remove --prefix zq --uncataloged --yes"),
            "{}",
            report.summary
        );
        let calls = log.lock().unwrap().clone();
        assert!(
            calls.iter().all(|c| !c.starts_with("DELETE ")),
            "listing is not deleting: {calls:?}"
        );

        let (url, log) = spawn_catalog_server_with(&other, &live);
        let report = execute(request(url, true)).expect("remove");
        assert_eq!(report.exit_code, 0, "{}", report.summary);
        assert_eq!(report.body["deleted"], true);
        let calls = log.lock().unwrap().clone();
        let deletes: Vec<&String> = calls.iter().filter(|c| c.starts_with("DELETE ")).collect();
        assert_eq!(
            deletes,
            vec!["DELETE /zq-code", "DELETE /zq-docs"],
            "the sibling prefix's cataloged index stays: {calls:?}"
        );
    }

    #[test]
    fn uncataloged_skips_claimed_sibling_dotted_and_catalog_indices() {
        let docs = [
            run("run:o", "zq-2", "/data/o", "2026-01-01T00:00:00Z"),
            dataset("ds:other:x", "zq-claimed", "x", "o", 1, 1),
        ];
        let refs: Vec<&CatalogDoc> = docs.iter().collect();
        let live: Vec<String> = [
            "zq-docs",
            "zq-claimed",
            "zq-2-docs",
            CATALOG_INDEX,
            ".zq-hidden",
            "zqx-docs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = uncataloged_among("zq", &live, &refs, &BTreeSet::new());
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            vec!["zq-docs".to_string()]
        );
    }

    fn spawn_catalog_server(
        docs: &[CatalogDoc],
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        spawn_catalog_server_with(docs, &[])
    }

    /// Like [`spawn_catalog_server`], and `GET /_cat/indices/...` lists `live`.
    fn spawn_catalog_server_with(
        docs: &[CatalogDoc],
        live: &[&str],
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let cat = Value::Array(live.iter().map(|n| json!({"index": n})).collect()).to_string();
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let hits: Vec<Value> = docs
            .iter()
            .map(|d| json!({"_id": d.id, "_source": d.source}))
            .collect();
        let catalog = json!({"hits": {"total": {"value": hits.len()}, "hits": hits}}).to_string();
        let log = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log_thread = Arc::clone(&log);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = ready_tx.send(());
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .ok();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    match stream.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            while let Some((head, body)) = take_http(&mut buf) {
                                let resp = answer(&head, &body, &catalog, &cat, &log_thread);
                                if stream.write_all(resp.as_bytes()).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("catalog server thread");
        (format!("http://127.0.0.1:{port}"), log)
    }

    fn take_http(buf: &mut Vec<u8>) -> Option<(String, Vec<u8>)> {
        let split = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&buf[..split]).to_string();
        let rest = buf.split_off(split + 4);
        *buf = rest;
        let len = head
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if buf.len() < len {
            // Put the bytes back and wait for the rest of the body.
            let mut combined = head.into_bytes();
            combined.extend_from_slice(b"\r\n\r\n");
            combined.extend_from_slice(buf);
            *buf = combined;
            return None;
        }
        let body = buf.drain(..len).collect();
        Some((head, body))
    }

    fn answer(
        head: &str,
        body: &[u8],
        catalog: &str,
        cat: &str,
        log: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> String {
        let req = head.lines().next().unwrap_or("");
        let mut parts = req.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
        let mut line = format!("{method} {path}");
        if method == "POST" && path == "/_bulk" {
            line = format!("BULK {}", String::from_utf8_lossy(body));
        }
        log.lock().unwrap().push(line);
        let json_body = if method == "GET" && path == "/" {
            "{}".into()
        } else if path.contains("_search") {
            catalog.to_string()
        } else if path.starts_with("/_cat/indices") {
            cat.to_string()
        } else if path.ends_with("/_stats") {
            let name = path.trim_start_matches('/').trim_end_matches("/_stats");
            let (docs, bytes) = if name == "ax-only-a" {
                (2u64, 100u64)
            } else {
                (5, 500)
            };
            json!({
                "indices": {
                    name: {
                        "primaries": {
                            "docs": {"count": docs},
                            "store": {"size_in_bytes": bytes}
                        }
                    }
                }
            })
            .to_string()
        } else if method == "POST" && path == "/_bulk" {
            "{\"errors\":false,\"items\":[]}".into()
        } else {
            "{\"acknowledged\":true}".into()
        };
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json_body}",
            json_body.len()
        )
    }

    /// #2314: `remove` used to leave `journal.ndjson`, and the next autoindex
    /// of that folder aborted on it. Only the selected root's default journal
    /// and an explicit `--state-dir` are deleted. A sibling folder's journal
    /// and an unrelated file in the explicit directory stay.
    #[test]
    fn discard_resume_journals_removes_the_selected_roots_journal_only() {
        let root = tempfile::tempdir().unwrap();
        let root_s = root.path().to_string_lossy().to_string();
        let url = "http://127.0.0.1:9200";
        let prefix = "proj";
        let selected = crate::state::default_state_dir(&root_s, url, prefix);
        let sibling = crate::state::default_state_dir("/other/folder", url, prefix);
        let explicit = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::create_dir_all(selected.join("sync-snapshots").join("gen")).unwrap();
        std::fs::write(selected.join("journal.ndjson"), "{}\n").unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("journal.ndjson"), "{}\n").unwrap();
        std::fs::write(explicit.path().join("journal.ndjson"), "{}\n").unwrap();
        std::fs::create_dir_all(explicit.path().join("sync-snapshots")).unwrap();
        std::fs::write(explicit.path().join("notes.txt"), "keep\n").unwrap();

        let removed = discard_resume_journals(&root_s, url, prefix, Some(explicit.path())).unwrap();
        assert!(
            removed.iter().any(|path| path.ends_with("journal.ndjson")),
            "{removed:?}"
        );
        assert!(!selected.join("journal.ndjson").exists());
        assert!(!selected.join("sync-snapshots").exists());
        assert!(
            sibling.join("journal.ndjson").exists(),
            "a sibling journal stays"
        );
        assert!(!explicit.path().join("journal.ndjson").exists());
        assert!(!explicit.path().join("sync-snapshots").exists());
        assert_eq!(
            std::fs::read_to_string(explicit.path().join("notes.txt")).unwrap(),
            "keep\n"
        );

        let again = discard_resume_journals(&root_s, url, prefix, Some(explicit.path())).unwrap();
        assert!(
            again.is_empty(),
            "a missing journal is not an error: {again:?}"
        );
        let _ = std::fs::remove_dir_all(&selected);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    fn aborted(name: &str, root: &str, prefix: &str) -> AbortedIndex {
        AbortedIndex {
            name: name.to_string(),
            root: root.to_string(),
            prefix: prefix.to_string(),
        }
    }

    /// #2391: a rebuild aborted before finalize. The partial prefix has indices
    /// and a resume journal, and no catalog run. The working index is a
    /// different prefix that did finish. Removing the partial prefix by root
    /// deletes only the partial indices.
    #[test]
    fn an_aborted_build_with_no_catalog_run_is_removed_by_root() {
        let working = vec![
            run("run:wp", "wp", "/site", "2026-10-01T00:00:00Z"),
            dataset("ds:wp:docs", "wp-docs", "docs", "wp", 40, 400),
            run("run:other", "ax", "/elsewhere", "2026-10-02T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "other", 3, 30),
        ];
        let live = vec![
            LiveIndex {
                name: "wp-docs".into(),
                docs: 40,
                store_bytes: 4000,
            },
            LiveIndex {
                name: "wp-rebuild-posts".into(),
                docs: 12,
                store_bytes: 1200,
            },
            LiveIndex {
                name: "wp-rebuild-pages".into(),
                docs: 4,
                store_bytes: 400,
            },
            LiveIndex {
                name: "ax-docs".into(),
                docs: 3,
                store_bytes: 300,
            },
        ];
        let aborted = vec![
            aborted("wp-rebuild-posts", "/site", "wp-rebuild"),
            aborted("wp-rebuild-pages", "/site", "wp-rebuild"),
            // A journal must not be able to name the preserved working index
            // or another root's index just by listing it.
            aborted("wp-docs", "/site", "wp-rebuild"),
            aborted("ax-docs", "/site", "wp-rebuild"),
        ];
        let plan = plan_remove("wp-rebuild", Some("/site"), &working, &live, &aborted);
        assert!(
            !plan.needs_root && !plan.root_not_found && !plan.empty,
            "{plan:?}"
        );
        assert_eq!(
            names(&plan),
            vec![
                "wp-rebuild-pages".to_string(),
                "wp-rebuild-posts".to_string()
            ],
            "{plan:?}"
        );
        assert!(plan.indices.iter().all(|idx| idx.orphan), "{plan:?}");
        assert_eq!(plan.indices[0].live_docs, Some(4));
        assert!(
            !names(&plan)
                .iter()
                .any(|n| n == "wp-docs" || n == "ax-docs"),
            "working index and the other root stay: {:?}",
            names(&plan)
        );
        assert!(
            row_ids(&plan).is_empty(),
            "an aborted build has no catalog row to delete: {:?}",
            row_ids(&plan)
        );
        let body = plan_body(&plan, 0, false, None);
        assert_eq!(body["remove"]["indices"][1]["orphan"], true);
        assert!(
            summary_text(&plan, 0, false, None).contains("aborted build, no catalog run"),
            "{}",
            summary_text(&plan, 0, false, None)
        );
    }

    /// The same prefix, two folders. The aborted folder's journal names the
    /// index the other folder's catalog still owns. That index stays.
    #[test]
    fn an_aborted_journal_cannot_delete_another_roots_cataloged_index() {
        let docs = vec![
            run("run:keep", "ax", "/data/keep", "2026-10-01T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "keep", 9, 90),
        ];
        let aborted = vec![
            aborted("ax-docs", "/data/abort", "ax"),
            aborted("ax-partial", "/data/abort", "ax"),
        ];
        let plan = plan_remove("ax", Some("/data/abort"), &docs, &[], &aborted);
        assert_eq!(names(&plan), vec!["ax-partial".to_string()], "{plan:?}");
        assert!(plan.indices[0].orphan);
        assert!(
            plan.kept_indices.iter().any(|k| k.name == "ax-docs"),
            "the cataloged working index is kept: {:?}",
            plan.kept_indices
        );
    }

    #[test]
    fn two_aborted_roots_need_a_root_and_delete_nothing() {
        let aborted = vec![
            aborted("ax-a", "/data/a", "ax"),
            aborted("ax-b", "/data/b", "ax"),
        ];
        let plan = plan_remove("ax", None, &[], &[], &aborted);
        assert!(plan.needs_root, "{plan:?}");
        assert!(plan.indices.is_empty());
        assert!(plan.rows.is_empty());
    }

    #[test]
    fn one_aborted_root_is_selected_without_passing_root() {
        let plan = plan_remove(
            "ax",
            None,
            &[],
            &[],
            &[aborted("ax-posts", "/data/a", "ax")],
        );
        assert!(!plan.needs_root && !plan.empty, "{plan:?}");
        assert_eq!(names(&plan), vec!["ax-posts".to_string()]);
    }

    #[test]
    fn a_sibling_prefix_index_in_the_journal_is_not_deleted() {
        let docs = vec![run("run:sib", "ax-2", "/data/sib", "2026-10-01T00:00:00Z")];
        let aborted = vec![
            aborted("ax-docs", "/data/a", "ax"),
            aborted("ax-2-docs", "/data/a", "ax"),
        ];
        let plan = plan_remove("ax", Some("/data/a"), &docs, &[], &aborted);
        assert_eq!(names(&plan), vec!["ax-docs".to_string()], "{plan:?}");
    }

    #[test]
    fn a_finished_journal_is_not_an_orphan_source() {
        let text = "\
{\"kind\":\"run\",\"root\":\"/site\",\"url\":\"http://127.0.0.1:9200\",\"prefix\":\"wp-rebuild\"}\n\
{\"kind\":\"plan\",\"plan\":{\"datasets\":[{\"index\":\"wp-rebuild-posts\"}]}}\n\
{\"kind\":\"finish\",\"summary\":{}}\n";
        assert!(
            aborted_indices_from_journal(text, "wp-rebuild", "http://127.0.0.1:9200", true)
                .is_empty()
        );
    }

    #[test]
    fn an_unfinished_journal_names_the_partial_prefix() {
        let text = "\
{\"kind\":\"run\",\"root\":\"/site\",\"url\":\"http://127.0.0.1:9200/\",\"prefix\":\"wp-rebuild\"}\n\
{\"kind\":\"plan\",\"plan\":{\"datasets\":[{\"index\":\"wp-rebuild-pages\"},{\"index\":\"wp-rebuild-posts\"}],\"files\":{}}}\n";
        let got = aborted_indices_from_journal(text, "wp-rebuild", "http://127.0.0.1:9200", true);
        let names: Vec<_> = got.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, vec!["wp-rebuild-pages", "wp-rebuild-posts"]);
        assert!(got.iter().all(|item| item.root == "/site"));
        let any = unfinished_journal_indices(text, "http://127.0.0.1:9200", true);
        assert!(
            any.iter().all(|item| item.prefix == "wp-rebuild"),
            "status uses the prefix the journal recorded, not the one it was asked about: {any:?}"
        );
        assert!(
            aborted_indices_from_journal(text, "wp", "http://127.0.0.1:9200", true).is_empty(),
            "a different prefix does not adopt this journal"
        );
        assert!(
            aborted_indices_from_journal(text, "wp-rebuild", "http://10.0.0.2:9200", true)
                .is_empty(),
            "a different endpoint does not adopt this journal"
        );
    }

    #[test]
    fn a_pending_generation_is_an_orphan_source_and_a_commit_is_not() {
        let open = "\
{\"kind\":\"run\",\"root\":\"/site\",\"url\":\"http://127.0.0.1:9200\",\"prefix\":\"wp-rebuild\"}\n\
{\"kind\":\"sync_begin\",\"desired\":{\"plan\":{\"datasets\":[{\"index\":\"wp-rebuild-posts\"}]}}}\n";
        assert_eq!(
            aborted_indices_from_journal(open, "wp-rebuild", "http://127.0.0.1:9200", true).len(),
            1
        );
        let committed = format!("{open}{{\"kind\":\"sync_commit\"}}\n");
        assert!(aborted_indices_from_journal(
            &committed,
            "wp-rebuild",
            "http://127.0.0.1:9200",
            true
        )
        .is_empty());
        let resumed = format!(
            "{committed}{{\"kind\":\"resume\"}}\n{{\"kind\":\"index_created\",\"index\":\"wp-rebuild-extra\"}}\n"
        );
        let got =
            aborted_indices_from_journal(&resumed, "wp-rebuild", "http://127.0.0.1:9200", true);
        let names: Vec<_> = got.iter().map(|item| item.name.as_str()).collect();
        assert!(names.contains(&"wp-rebuild-posts"), "{names:?}");
        assert!(names.contains(&"wp-rebuild-extra"), "{names:?}");
    }

    #[test]
    fn status_flags_the_partial_prefix_and_not_the_working_index() {
        let live = vec![
            "wp-docs".into(),
            "wp-rebuild-posts".into(),
            "ax-docs".into(),
        ];
        let mut cataloged = BTreeSet::new();
        cataloged.insert("wp-docs".to_string());
        let aborted = vec![aborted("wp-rebuild-posts", "/site", "wp-rebuild")];
        let flags = orphan_flags(&live, &cataloged, &aborted);
        assert_eq!(flags, vec![("wp-rebuild-posts".into(), "/site".into())]);
        let lines = orphan_status_lines("wp-rebuild", &flags);
        assert!(
            lines.iter().any(|line| line.contains("wp-rebuild-posts")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| {
                line.contains("xerj autoindex remove --prefix wp-rebuild --root /site --yes")
            }),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("wp-docs")),
            "{lines:?}"
        );
    }

    /// The acceptance path: the node holds the preserved working index and the
    /// partial prefix. `remove --root` of the partial prefix deletes only the
    /// partial indices. The journal is the only record of them.
    #[test]
    fn remove_by_root_deletes_the_aborted_prefix_and_leaves_the_working_index() {
        let docs = vec![
            run("run:wp", "wp", "/site", "2026-10-01T00:00:00Z"),
            dataset("ds:wp:docs", "wp-docs", "docs", "wp", 40, 400),
            run("run:else", "ax", "/elsewhere", "2026-10-02T00:00:00Z"),
            dataset("ds:ax:docs", "ax-docs", "docs", "else", 2, 20),
        ];
        let (url, log) = spawn_catalog_server(&docs);
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            state.path().join("journal.ndjson"),
            format!(
                "{{\"kind\":\"run\",\"root\":\"/site\",\"url\":\"{url}\",\"prefix\":\"wp-rebuild\"}}\n\
                 {{\"kind\":\"plan\",\"plan\":{{\"datasets\":[{{\"index\":\"wp-rebuild-posts\"}},{{\"index\":\"wp-rebuild-pages\"}}]}}}}\n"
            ),
        )
        .unwrap();
        let report = execute(RemoveRequest {
            url,
            api_key: None,
            prefix: "wp-rebuild".into(),
            root: Some("/site".into()),
            yes: true,
            prompt: false,
            state_dir: Some(state.path().to_path_buf()),
            uncataloged: false,
        })
        .expect("remove");
        assert_eq!(report.exit_code, 0, "{}\n{}", report.summary, report.body);
        assert_eq!(report.body["deleted"], true);
        let removed: Vec<String> = report.body["remove"]["indices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|idx| idx["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            removed,
            vec![
                "wp-rebuild-pages".to_string(),
                "wp-rebuild-posts".to_string()
            ]
        );
        assert!(report.body["remove"]["indices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|idx| idx["orphan"] == true));
        let calls = log.lock().unwrap().clone();
        let deletes: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("DELETE "))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            deletes,
            vec![
                "DELETE /wp-rebuild-pages".to_string(),
                "DELETE /wp-rebuild-posts".to_string()
            ],
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("wp-docs") && c.starts_with("DELETE")),
            "the preserved working index was deleted: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("ax-docs") && c.starts_with("DELETE")),
            "another root's index was deleted: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("BULK") && c.contains("ds:wp:docs")),
            "the working catalog row was deleted: {calls:?}"
        );
    }
}
