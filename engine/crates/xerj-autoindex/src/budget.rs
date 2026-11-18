//! Disk budget for `xerj autoindex`, decided before hashing.
//!
//! The walk has already counted files. Hashing and embedding have not
//! started. This module estimates how large the index will be, compares that
//! with the room left under the node's flood-stage watermark, and — when the
//! default plan does not fit — names a smaller plan and waits for an explicit
//! yes. It never invents a ratio: a kind with no measured calibration is
//! reported as unmeasured, and an unmeasured disk estimate does not refuse a
//! run.
//!
//! JSON record arrays are a separate check. A `.json` file between 256 KiB
//! and 64 MiB is parsed for its record count, and that count is priced with
//! a structural memory ceiling (vector width, not a measured RSS). The
//! ceiling can refuse a run. It cannot make an unmeasured disk plan fit.
//!
//! Free space and the watermark come from [`crate::flood`], the one
//! `GET /_cat/allocation?format=json` already made before the walk. A second
//! probe is not issued. A node that omits `disk.flood_stage` does not get a
//! guessed 95% margin.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::estimate::{count_phrase, human_bytes};
use crate::extract::code::is_code_ext;
use crate::flood::{self, Volume};
use crate::gate::{self, Approval};
use crate::order;
use crate::walk::{self, FileEntry};

/// Plans a caller can name with `--plan`. The order is the tie-break: an
/// earlier plan wins when two that fit keep the same number of source bytes,
/// so `default` wins whenever it fits.
pub const PLAN_NAMES: &[&str] = &[
    "default",
    "skip-vendored",
    "skip-data",
    "code-and-docs",
    "no-vectors",
    "cap-1mb",
    "code-and-docs-no-vectors",
];

const CAP_1MB: u64 = 1 << 20;

/// What the calibration measured, and where. An empty table is not a zero
/// estimate. The numbers in [`MEASURED`] are dataset-index store sizes from
/// one release binary on one host; they are not a promise about this machine.
const CALIBRATION_NOTE: &str = "per-kind ratios are dataset-index store sizes measured 2026-10-21 with xerj v1.0.0-rc.97 on one 4-core Linux x86_64 host. Code and prose were measured on real repository sources and docs; data, pdf and unindexed kinds on synthetic files. Embeddings were the default lexical feature-hash (the neural embedder was not enabled). Catalog store size is not included. time_s is a rough projection from that host, not this machine. A kind with no row is unmeasured and does not refuse the run. JSON record counts use a separate structural memory ceiling (memory_basis); that ceiling is not a measured store ratio";

/// Bytes charged per counted JSON record when the plan embeds.
///
/// 384-d f32 is 1,536 bytes. 2,048 leaves about 512 bytes for the source
/// document beside that one vector. Not a measured RSS. Not multiplied by
/// chunk count: the semantic cap already refuses a data column whose text
/// would chunk into tens of thousands of vectors.
const SEMANTIC_RECORD_BYTES: u64 = 2048;

/// Bytes charged per counted JSON record when the plan does not embed.
const PLAIN_RECORD_BYTES: u64 = 512;

/// `.json` files smaller than this are not parsed during the pre-flight.
const JSON_PROBE_MIN: u64 = 256 << 10;

/// `.json` files this size or larger are not parsed during the pre-flight.
/// The same bound is the whole-file JSON read cap.
const JSON_PROBE_MAX: u64 = 64 << 20;

const MEMORY_BASIS: &str = "structural ceiling from the vector width, not a measured RSS and not a store-size ratio: 2048 bytes per counted JSON record when semantic vectors are on (one 384-d f32 vector plus about 512 bytes of source) or 512 bytes per record when they are off, plus the .json file size. Only .json files from 256 KiB up to 64 MiB are counted. A file that does not parse contributes nothing and does not refuse the run";

/// One measured index of one kind.
#[derive(Debug, Clone, Copy)]
struct Point {
    files: u64,
    source_bytes: u64,
    index_bytes: u64,
    wall_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Model {
    Code,
    Prose,
    Pdf,
    Data,
    /// Binary and scientific containers the extractor does not read. Their
    /// index cost is a catalog row, not a copy of the file.
    Unindexed,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct Calib {
    model: Model,
    semantic: bool,
    small: Point,
    large: Point,
}

/// Release-binary calibration, 2026-10-21.
///
/// Binary: `xerj v1.0.0-rc.97`, started `--insecure` (lexical feature-hash
/// embeddings; `--embed-mode neural` was not passed). Host: Linux 6.12.94+
/// x86_64, 4 cores, 16387280 KiB RAM.
///
/// Code and prose points are real repository content, not synthetic files:
/// copies of this repository's `.rs` sources (5 files / 291,007 B and
/// 97 files / 5,096,601 B) and `.md` documents (24 files / 520,410 B and
/// 169 files / 2,969,566 B). Synthetic distinct-token files under-predicted
/// those by about 3.5x, so they were replaced. Data, PDF and unindexed points
/// are still the synthetic runs described below.
///
/// `index_bytes` is the sum of `store.size` for that prefix's indices from
/// `GET /_cat/indices?bytes=b&h=index,store.size` right after the run, before
/// segment merges settle: that peak is what the disk has to hold. A force-merge
/// later shrank the real-content stores by roughly 20-40%, so this errs high.
/// `autoindex-catalog` is omitted: its store size moved by tens of KB in both
/// directions between runs. For code and prose the per-file intercept comes
/// out negative, so the fitter uses the steeper of the two measured ratios.
/// `wall_ms` for code and prose is the wall time of the whole `xerj autoindex`
/// command, so it still contains the fixed per-run cost.
///
/// Synthetic points (data, PDF, unindexed): each is one `xerj autoindex` of
/// files with distinct tokens. `.npy` files were recorded as junk
/// (`files_junk` equal to the file count) and their dataset-index store delta
/// was 0. The two PDF points are about 1 KB and 10 KB; their per-byte slope is
/// negative, so the fitter uses the steeper of the two measured ratios.
static MEASURED: &[Calib] = &[
    Calib {
        model: Model::Code,
        semantic: true,
        small: Point {
            files: 5,
            source_bytes: 291_007,
            index_bytes: 1_337_452,
            wall_ms: 2_279,
        },
        large: Point {
            files: 97,
            source_bytes: 5_096_601,
            index_bytes: 15_252_875,
            wall_ms: 5_886,
        },
    },
    Calib {
        model: Model::Code,
        semantic: false,
        small: Point {
            files: 5,
            source_bytes: 291_007,
            index_bytes: 574_967,
            wall_ms: 1_740,
        },
        large: Point {
            files: 97,
            source_bytes: 5_096_601,
            index_bytes: 7_367_909,
            wall_ms: 5_139,
        },
    },
    Calib {
        model: Model::Prose,
        semantic: true,
        small: Point {
            files: 24,
            source_bytes: 520_410,
            index_bytes: 4_656_359,
            wall_ms: 1_749,
        },
        large: Point {
            files: 169,
            source_bytes: 2_969_566,
            index_bytes: 15_859_387,
            wall_ms: 5_308,
        },
    },
    Calib {
        model: Model::Prose,
        semantic: false,
        small: Point {
            files: 24,
            source_bytes: 520_410,
            index_bytes: 748_083,
            wall_ms: 3_796,
        },
        large: Point {
            files: 169,
            source_bytes: 2_969_566,
            index_bytes: 2_805_069,
            wall_ms: 3_646,
        },
    },
    Calib {
        model: Model::Data,
        semantic: true,
        small: Point {
            files: 1,
            source_bytes: 20_039,
            index_bytes: 63_682,
            wall_ms: 600,
        },
        large: Point {
            files: 4,
            source_bytes: 320_156,
            index_bytes: 681_267,
            wall_ms: 900,
        },
    },
    Calib {
        model: Model::Data,
        semantic: false,
        small: Point {
            files: 1,
            source_bytes: 20_039,
            index_bytes: 63_682,
            wall_ms: 600,
        },
        large: Point {
            files: 4,
            source_bytes: 320_156,
            index_bytes: 669_741,
            wall_ms: 900,
        },
    },
    Calib {
        model: Model::Other,
        semantic: true,
        small: Point {
            files: 1,
            source_bytes: 4_000,
            index_bytes: 19_611,
            wall_ms: 600,
        },
        large: Point {
            files: 8,
            source_bytes: 160_000,
            index_bytes: 169_502,
            wall_ms: 600,
        },
    },
    Calib {
        model: Model::Other,
        semantic: false,
        small: Point {
            files: 1,
            source_bytes: 4_000,
            index_bytes: 19_611,
            wall_ms: 600,
        },
        large: Point {
            files: 8,
            source_bytes: 160_000,
            index_bytes: 169_134,
            wall_ms: 600,
        },
    },
    Calib {
        model: Model::Pdf,
        semantic: true,
        small: Point {
            files: 1,
            source_bytes: 1_082,
            index_bytes: 14_542,
            wall_ms: 0,
        },
        large: Point {
            files: 4,
            source_bytes: 10_336,
            index_bytes: 29_218,
            wall_ms: 100,
        },
    },
    Calib {
        model: Model::Pdf,
        semantic: false,
        small: Point {
            files: 1,
            source_bytes: 1_082,
            index_bytes: 14_542,
            wall_ms: 0,
        },
        large: Point {
            files: 4,
            source_bytes: 10_336,
            index_bytes: 29_180,
            wall_ms: 100,
        },
    },
    Calib {
        model: Model::Unindexed,
        semantic: true,
        small: Point {
            files: 1,
            source_bytes: 4_000,
            index_bytes: 0,
            wall_ms: 0,
        },
        large: Point {
            files: 8,
            source_bytes: 320_000,
            index_bytes: 0,
            wall_ms: 0,
        },
    },
    Calib {
        model: Model::Unindexed,
        semantic: false,
        small: Point {
            files: 1,
            source_bytes: 4_000,
            index_bytes: 0,
            wall_ms: 0,
        },
        large: Point {
            files: 8,
            source_bytes: 320_000,
            index_bytes: 0,
            wall_ms: 0,
        },
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Code,
    Prose,
    Pdf,
    Data,
    Binary,
    Other,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Code => "code",
            Kind::Prose => "prose",
            Kind::Pdf => "pdf",
            Kind::Data => "data",
            Kind::Binary => "binary",
            Kind::Other => "other",
        }
    }
}

struct Fact {
    rel: String,
    size: u64,
    kind: Kind,
    model: Model,
    vendored: bool,
    /// Rows in a probed `.json` file. `None` when the file was not probed.
    records: Option<u64>,
}

/// What the caller already decided, plus the walked files and the volume from
/// the flood-stage probe.
pub(crate) struct Request<'a> {
    pub root: &'a str,
    pub files: &'a [FileEntry],
    pub volume: Option<Volume>,
    pub max_index_bytes: Option<u64>,
    pub no_semantic: bool,
    pub plan: Option<&'a str>,
    pub approve: Option<Approval>,
    pub duplicate_prefixes: &'a [String],
    /// Resident-memory ceiling for counted JSON records. `None` skips the
    /// memory comparison. The live path passes
    /// [`xerj_common::resource::memory_safe_zone_bytes`].
    pub memory_budget: Option<u64>,
}

/// A line the operator typed at the budget prompt.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Nobody was asked, or the reply was empty.
    Absent,
    Plan(String),
    Cancel,
}

enum Preselect {
    None,
    Plan(String),
    Cancel,
    Refuse(&'static str),
}

struct PlanEst {
    name: &'static str,
    files: u64,
    source_bytes: u64,
    index_bytes: Option<u64>,
    time_s: Option<f64>,
    drops: Vec<String>,
    no_semantic: bool,
    keep: Option<Vec<String>>,
    fits: Option<bool>,
    /// Structural memory ceiling for the JSON records this plan keeps.
    /// `None` when the plan kept no probed `.json` file.
    memory_bytes: Option<u64>,
}

pub struct Assessment {
    pub body: Value,
    /// Lines to show before a prompt, or with a refusal.
    pub lines: Vec<String>,
    /// One line when the run continues. Empty when there is nothing new to say.
    pub note: String,
    pub needs_choice: bool,
    pub plan_names: Vec<String>,
    preselect: Preselect,
    plans: Vec<PlanEst>,
}

pub enum Outcome {
    Continue {
        /// `None` keeps every walked file.
        keep: Option<Vec<String>>,
        no_semantic: bool,
        note: String,
        /// Set when a named plan (or `--yes`) produced `keep`. `None` on the
        /// default plan, which does not filter the walk.
        plan_name: Option<String>,
    },
    /// `body` is `Null` for a cancel, which prints no JSON.
    Stop { code: i32, body: Value },
}

/// Room the index may occupy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiskBudget {
    /// Bytes until `disk.avail` would cross the flood-stage reserve.
    UntilFlood {
        bytes: u64,
        flood_percent: u8,
        avail: u64,
        total: u64,
    },
    /// Watermark off (`disk.flood_stage` 0, or the threshold disabled).
    WatermarkOff { bytes: u64, avail: u64 },
}

/// Index budget from the allocation row `#2077` already parsed.
///
/// `None` when the node did not report a flood threshold, or reported one
/// but not the total the reserve is a fraction of. The missing number is
/// not filled in from the default 95.
pub(crate) fn disk_budget(volume: &Volume) -> Option<DiskBudget> {
    let avail = volume.avail?;
    let flood = volume.flood_stage?;
    if !volume.threshold_enabled || flood == 0 {
        return Some(DiskBudget::WatermarkOff {
            bytes: avail,
            avail,
        });
    }
    let total = volume.total?;
    let headroom = u64::from(100u8.saturating_sub(flood));
    let reserve = ((total as u128) * u128::from(headroom) / 100) as u64;
    Some(DiskBudget::UntilFlood {
        bytes: avail.saturating_sub(reserve),
        flood_percent: flood,
        avail,
        total,
    })
}

/// Parse `--max-index-size`. A bare integer is bytes. `KB`/`MB`/`GB` (and the
/// one-letter forms) are powers of 1024. Zero is rejected.
pub fn parse_byte_size(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("--max-index-size needs a size, for example 2GB".into());
    }
    let split = raw.find(|c: char| !c.is_ascii_digit());
    let (number, suffix) = match split {
        Some(i) => (&raw[..i], raw[i..].trim()),
        None => (raw, ""),
    };
    if number.is_empty() {
        return Err(format!(
            "--max-index-size needs a size in bytes or with a KB/MB/GB suffix, got {raw}"
        ));
    }
    let n: u64 = number.parse().map_err(|_| {
        format!("--max-index-size needs a size in bytes or with a KB/MB/GB suffix, got {raw}")
    })?;
    let mult: u64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => {
            return Err(format!(
                "--max-index-size suffix must be B, KB, MB, or GB, got {suffix}"
            ))
        }
    };
    let bytes = n
        .checked_mul(mult)
        .ok_or_else(|| format!("--max-index-size {raw} overflows"))?;
    if bytes == 0 {
        return Err("--max-index-size must be greater than 0".into());
    }
    Ok(bytes)
}

pub(crate) fn assess(req: &Request<'_>) -> Assessment {
    let facts: Vec<Fact> = req.files.iter().map(fact_of).collect();
    let semantic_default = !req.no_semantic;
    let (budget_bytes, budget_basis, disk_json) = budget_view(req.volume, req.max_index_bytes);
    let plans = PLAN_NAMES
        .iter()
        .filter_map(|name| {
            estimate_plan(
                name,
                &facts,
                semantic_default,
                budget_bytes,
                req.memory_budget,
            )
        })
        .collect::<Vec<_>>();
    let plan_names: Vec<String> = plans.iter().map(|p| p.name.to_string()).collect();
    let recommended = recommended_plan(&plans).map(|p| p.name);
    let default_fits = plans
        .iter()
        .find(|p| p.name == "default")
        .and_then(|p| p.fits);

    let mut preselect = Preselect::None;
    let mut needs_choice = false;
    if req.approve == Some(Approval::Cancel) {
        preselect = Preselect::Cancel;
    } else if let Some(name) = req.plan {
        match plans.iter().find(|p| p.name == name) {
            // A plan named on the command line is the override when the
            // estimate is wrong: it runs even if the estimate says it does
            // not fit, with a warning. The flood-stage guard still applies.
            Some(_) => preselect = Preselect::Plan(name.to_string()),
            // `no-vectors` is omitted when the run is already `--no-semantic`:
            // it is the same plan as `default`.
            None if PLAN_NAMES.contains(&name) => {
                preselect = Preselect::Plan("default".to_string())
            }
            None => preselect = Preselect::Refuse("unknown"),
        }
    } else if default_fits == Some(false) {
        let accepted = matches!(req.approve, Some(Approval::Proceed | Approval::Fast));
        if accepted {
            match recommended {
                Some(name) => preselect = Preselect::Plan(name.to_string()),
                None => preselect = Preselect::Refuse("none"),
            }
        } else {
            needs_choice = true;
        }
    }

    let note = if needs_choice || matches!(preselect, Preselect::Refuse(_) | Preselect::Cancel) {
        String::new()
    } else if let Preselect::Plan(name) = &preselect {
        plans
            .iter()
            .find(|p| p.name == name)
            .map(|p| continue_note(p, budget_bytes, &budget_basis))
            .unwrap_or_default()
    } else if default_fits == Some(true) {
        plans
            .iter()
            .find(|p| p.name == "default")
            .map(|p| {
                format!(
                    "{} under the {} budget ({budget_basis})",
                    plan_size_phrase(p),
                    budget_bytes
                        .map(human_bytes)
                        .unwrap_or_else(|| "unset".into())
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    let lines = summary_lines(&facts, &plans, budget_bytes, &budget_basis, recommended);
    let body = json!({
        "xerj": "autoindex-budget",
        "root": req.root,
        "classification": "by extension, before content sniff",
        "files": facts.len(),
        "bytes": facts.iter().map(|f| f.size).sum::<u64>(),
        "by_kind": counts_by(&facts, |f| f.kind.name()),
        "by_folder": counts_by(&facts, |f| top_folder(&f.rel)),
        "disk": disk_json,
        "budget_bytes": budget_bytes,
        "budget_basis": budget_basis,
        "max_index_bytes": req.max_index_bytes,
        "decision_required": needs_choice || matches!(preselect, Preselect::Refuse(_)),
        "plans": plans.iter().map(plan_json).collect::<Vec<_>>(),
        "recommended": recommended,
        "json_records": json_record_total(&facts),
        "memory_safe_zone_bytes": req.memory_budget,
        "memory_basis": MEMORY_BASIS,
        "duplicate_prefixes": req.duplicate_prefixes,
        "accept": "re-run with --plan <name>, or --yes to take the recommended plan that fits. A named plan runs even when the estimate says it does not fit (the estimate errs high). --approve cancel indexes nothing",
        "calibration": CALIBRATION_NOTE,
    });

    Assessment {
        body,
        lines,
        note,
        needs_choice,
        plan_names,
        preselect,
        plans,
    }
}

pub fn resolve(assessment: &Assessment, choice: Choice) -> Outcome {
    if matches!(assessment.preselect, Preselect::Cancel) || matches!(choice, Choice::Cancel) {
        return Outcome::Stop {
            code: 0,
            body: Value::Null,
        };
    }
    if let Preselect::Refuse(why) = assessment.preselect {
        let mut body = assessment.body.clone();
        body["exit_code"] = json!(gate::EXIT_NEEDS_DECISION);
        body["refused"] = json!(match why {
            "none" => "no plan fits the budget",
            _ => "unknown plan",
        });
        return Outcome::Stop {
            code: gate::EXIT_NEEDS_DECISION,
            body,
        };
    }
    let name = match &assessment.preselect {
        Preselect::Plan(name) => Some(name.as_str()),
        Preselect::None if assessment.needs_choice => match choice {
            Choice::Plan(ref name) => Some(name.as_str()),
            Choice::Absent => {
                let mut body = assessment.body.clone();
                body["exit_code"] = json!(gate::EXIT_NEEDS_DECISION);
                return Outcome::Stop {
                    code: gate::EXIT_NEEDS_DECISION,
                    body,
                };
            }
            Choice::Cancel => {
                return Outcome::Stop {
                    code: 0,
                    body: Value::Null,
                };
            }
        },
        _ => None,
    };
    let Some(name) = name else {
        return Outcome::Continue {
            keep: None,
            no_semantic: false,
            note: assessment.note.clone(),
            plan_name: None,
        };
    };
    match assessment.plans.iter().find(|p| p.name == name) {
        Some(plan) if plan.keep.as_ref().is_some_and(|kept| kept.is_empty()) => {
            let mut body = assessment.body.clone();
            body["exit_code"] = json!(gate::EXIT_NEEDS_DECISION);
            body["refused"] = json!(format!(
                "--plan {} keeps no files, so nothing was hashed. Pick another plan, or cancel",
                plan.name
            ));
            Outcome::Stop {
                code: gate::EXIT_NEEDS_DECISION,
                body,
            }
        }
        Some(plan) => Outcome::Continue {
            keep: plan.keep.clone(),
            no_semantic: plan.no_semantic,
            note: if assessment.note.is_empty() {
                continue_note(plan, budget_bytes_of(&assessment.body), "")
            } else {
                assessment.note.clone()
            },
            plan_name: Some(plan.name.to_string()),
        },
        None => {
            let mut body = assessment.body.clone();
            body["exit_code"] = json!(gate::EXIT_NEEDS_DECISION);
            body["refused"] = json!(format!(
                "unknown plan {name}; choices: {}",
                assessment.plan_names.join(", ")
            ));
            Outcome::Stop {
                code: gate::EXIT_NEEDS_DECISION,
                body,
            }
        }
    }
}

/// How many plan-excluded paths the decision JSON lists before it counts the rest.
const REMOVAL_LIST_CAP: usize = 20;

/// Files a reduced plan would drop that are already published and still in the walk.
///
/// The budget keep-set is applied to the walk *before* reconcile. On the
/// `--no-graph` path those files are absent from the inventory, so
/// `plan_operations` emits `Delete` for their groups. On the graph path they
/// are still on disk, so they are classified as exclusions and
/// `sweep_excluded_groups` deletes their documents. Either way a smaller plan
/// silently unindexes data the folder still has. This is the list that refusal
/// prints, and that `--drop-indexed` agrees to remove.
///
/// A file that is in a prior plan but no longer in the walk is a real deletion
/// or an ignore-rule exclusion, not this decision, and is left out of the list.
pub(crate) fn removal_decision(
    plan_name: &str,
    plans: &[&crate::state::Plan],
    discovered: &[FileEntry],
    keep: &[String],
) -> Option<Value> {
    let keep: std::collections::HashSet<&str> = keep.iter().map(String::as_str).collect();
    let walked: std::collections::HashSet<&str> =
        discovered.iter().map(|file| file.rel.as_str()).collect();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut drops: Vec<Removal> = Vec::new();
    for prior in plans {
        for (key, assignment) in &prior.files {
            if !walked.contains(assignment.rel.as_str()) || keep.contains(assignment.rel.as_str()) {
                continue;
            }
            if !seen.insert((key.clone(), assignment.rel.clone())) {
                continue;
            }
            let mut datasets: Vec<String> = assignment
                .assignments
                .iter()
                .map(|(_, slug)| slug.clone())
                .collect();
            datasets.sort();
            datasets.dedup();
            drops.push(Removal {
                path: assignment.rel.clone(),
                file_key: key.clone(),
                datasets,
            });
        }
    }
    if drops.is_empty() {
        return None;
    }
    drops.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.file_key.cmp(&right.file_key))
    });
    let total = drops.len();
    let listed = drops.len().min(REMOVAL_LIST_CAP);
    let dropped_keys: std::collections::HashSet<&str> = drops
        .iter()
        .map(|removed| removed.file_key.as_str())
        .collect();
    let datasets = emptied_datasets(plans, &walked, &keep, &dropped_keys);
    let listed_paths = drops
        .iter()
        .take(listed)
        .map(|removed| removed.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let more = if total > listed {
        format!(" (and {} more)", total - listed)
    } else {
        String::new()
    };
    let dataset_phrase = if datasets.is_empty() {
        String::new()
    } else {
        format!(" Dataset(s) left with no files: {}.", datasets.join(", "))
    };
    let would_remove: Vec<Value> = drops
        .iter()
        .take(listed)
        .map(|removed| {
            json!({
                "path": removed.path,
                "file_key": removed.file_key,
                "datasets": removed.datasets,
            })
        })
        .collect();
    Some(json!({
        "xerj": "autoindex-plan-removal",
        "decision_required": true,
        "exit_code": crate::gate::EXIT_NEEDS_DECISION,
        "plan": plan_name,
        "would_remove": would_remove,
        "would_remove_total": total,
        "would_remove_truncated": total > listed,
        "datasets": datasets,
        "accept": format!(
            "plan {plan_name} leaves out {total} already-indexed file(s) still in the folder: \
             {listed_paths}{more}.{dataset_phrase} A reconcile would delete them. Nothing was \
             changed. Re-run the same command with --drop-indexed to delete them, or drop \
             --plan to keep the full index. --yes and --plan do not agree to this deletion"
        ),
    }))
}

struct Removal {
    path: String,
    file_key: String,
    datasets: Vec<String>,
}

/// A dataset the plan would empty: at least one of its published files is in
/// the drop list, and none of its published files are both still walked and kept.
fn emptied_datasets(
    plans: &[&crate::state::Plan],
    walked: &std::collections::HashSet<&str>,
    keep: &std::collections::HashSet<&str>,
    dropped_keys: &std::collections::HashSet<&str>,
) -> Vec<String> {
    let mut survivor: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut touched: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for prior in plans {
        for (key, assignment) in &prior.files {
            let survives =
                walked.contains(assignment.rel.as_str()) && keep.contains(assignment.rel.as_str());
            for (_, slug) in &assignment.assignments {
                if survives {
                    survivor.insert(slug.as_str());
                }
                if dropped_keys.contains(key.as_str()) {
                    touched.insert(slug.as_str());
                }
            }
        }
    }
    touched
        .into_iter()
        .filter(|slug| !survivor.contains(slug))
        .map(str::to_owned)
        .collect()
}

fn budget_bytes_of(body: &Value) -> Option<u64> {
    body.get("budget_bytes").and_then(Value::as_u64)
}

pub fn read_choice<R: BufRead>(
    input: &mut R,
    names: &[String],
    mut echo: impl FnMut(&str),
) -> Choice {
    for _ in 0..3 {
        echo("  plan name, or cancel: ");
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => return Choice::Absent,
            Ok(_) => {}
        }
        let typed = line.trim();
        if typed.eq_ignore_ascii_case("cancel") || typed.eq_ignore_ascii_case("c") {
            return Choice::Cancel;
        }
        if let Some(name) = names.iter().find(|n| n.eq_ignore_ascii_case(typed)) {
            return Choice::Plan(name.clone());
        }
        echo(&format!(
            "  {typed:?} is not a plan. choices: {}",
            names.join(", ")
        ));
    }
    Choice::Absent
}

/// Walk `root`, read the node's allocation once, and return the budget JSON.
/// Does not index. A node that is down still returns the file counts; `disk`
/// is null and the basis says the probe did not answer.
pub fn assess_path(
    root: &Path,
    url: &str,
    auth: Option<&str>,
    prefix: &str,
    max_index_bytes: Option<u64>,
    no_semantic: bool,
) -> Result<Value> {
    let (files, _) = walk::walk_reporting_opts(
        root,
        false,
        false,
        crate::ignore_rules::IgnoreOptions::default(),
    )?;
    let root_str = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .to_string();
    let (volume, duplicates) =
        match crate::esclient::Es::with_bulk_timeout(url, auth.map(str::to_string), 30) {
            Ok(es) => {
                let volume = flood::probe(&es).volume;
                let duplicates = other_prefixes(&es, &root_str, prefix).unwrap_or_default();
                (volume, duplicates)
            }
            Err(_) => (None, Vec::new()),
        };
    let assessment = assess(&Request {
        root: &root_str,
        files: &files,
        volume,
        max_index_bytes,
        no_semantic,
        plan: None,
        approve: None,
        duplicate_prefixes: &duplicates,
        memory_budget: xerj_common::resource::memory_safe_zone_bytes(),
    });
    Ok(assessment.body)
}

fn other_prefixes(es: &crate::esclient::Es, root: &str, prefix: &str) -> Result<Vec<String>> {
    let body = json!({
        "size": 500,
        "query": {"bool": {"filter": [{"term": {"doc_kind": "run"}}]}}
    });
    let Some(page) = es.search_present(crate::catalog::CATALOG_INDEX, &body)? else {
        return Ok(Vec::new());
    };
    let runs = page
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|hit| hit.get("_source").cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(crate::catalog::other_prefixes_for_root(&runs, root, prefix))
}

fn budget_view(
    volume: Option<Volume>,
    max_index_bytes: Option<u64>,
) -> (Option<u64>, String, Value) {
    let disk = volume.as_ref().and_then(disk_budget);
    let (disk_bytes, basis, disk_json) = match (&volume, &disk) {
        (Some(vol), Some(DiskBudget::UntilFlood { bytes, flood_percent, avail, total })) => {
            let basis = format!(
                "free {} minus the {}% flood-stage reserve on {}",
                human_bytes(*avail),
                flood_percent,
                human_bytes(*total)
            );
            (
                Some(*bytes),
                basis,
                json!({
                    "total": total,
                    "avail": avail,
                    "used_percent": vol.used_percent,
                    "flood_stage": flood_percent,
                    "threshold_enabled": vol.threshold_enabled,
                    "reserve_bytes": avail.saturating_sub(*bytes),
                    "budget_bytes": bytes,
                }),
            )
        }
        (Some(vol), Some(DiskBudget::WatermarkOff { bytes, avail })) => (
            Some(*bytes),
            "flood-stage watermark is off; budget is the free space".to_string(),
            json!({
                "total": vol.total,
                "avail": avail,
                "used_percent": vol.used_percent,
                "flood_stage": vol.flood_stage,
                "threshold_enabled": vol.threshold_enabled,
                "watermark": "off",
                "budget_bytes": bytes,
            }),
        ),
        (Some(_), None) => (
            None,
            "the node did not report a flood-stage threshold and a total, so no disk margin was applied".to_string(),
            json!(null_disk(volume.as_ref())),
        ),
        (None, _) => (
            None,
            "the node did not report disk allocation, so no disk margin was applied".to_string(),
            Value::Null,
        ),
    };
    let (bytes, basis) = match (disk_bytes, max_index_bytes) {
        (Some(disk), Some(cap)) => (
            Some(disk.min(cap)),
            format!(
                "{basis}; capped at {} by --max-index-size",
                human_bytes(cap)
            ),
        ),
        (None, Some(cap)) => (
            Some(cap),
            format!("--max-index-size {} ({basis})", human_bytes(cap)),
        ),
        (disk, None) => (disk, basis),
    };
    (bytes, basis, disk_json)
}

fn null_disk(volume: Option<&Volume>) -> Value {
    match volume {
        Some(vol) => json!({
            "total": vol.total,
            "avail": vol.avail,
            "used_percent": vol.used_percent,
            "flood_stage": vol.flood_stage,
            "threshold_enabled": vol.threshold_enabled,
        }),
        None => Value::Null,
    }
}

fn estimate_plan(
    name: &str,
    facts: &[Fact],
    semantic_default: bool,
    budget_bytes: Option<u64>,
    memory_budget: Option<u64>,
) -> Option<PlanEst> {
    let force_no_semantic = matches!(name, "no-vectors" | "code-and-docs-no-vectors");
    let no_semantic = force_no_semantic || !semantic_default;
    if force_no_semantic && !semantic_default {
        // Already indexing without vectors; this plan is the same as another.
        return None;
    }
    let mut kept = Vec::new();
    let mut dropped_kinds: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut dropped_vendored = 0u64;
    let mut dropped_cap = 0u64;
    for fact in facts {
        if keep(name, fact) {
            kept.push(fact);
        } else if fact.vendored && name == "skip-vendored" {
            dropped_vendored += 1;
        } else if fact.size > CAP_1MB && name == "cap-1mb" {
            dropped_cap += 1;
        } else {
            *dropped_kinds.entry(fact.kind.name()).or_default() += 1;
        }
    }
    let mut drops = Vec::new();
    if dropped_vendored > 0 {
        drops.push(format!(
            "{} vendored",
            count_phrase(dropped_vendored, "path", "paths")
        ));
    }
    for (kind, n) in &dropped_kinds {
        drops.push(format!("{n} {kind}"));
    }
    if dropped_cap > 0 {
        drops.push(format!(
            "{} over 1 MB",
            count_phrase(dropped_cap, "file", "files")
        ));
    }
    if force_no_semantic {
        drops.push("vectors (semantic_text)".to_string());
    }
    let (index_bytes, time_s) = project_kept(&kept, !no_semantic);
    let memory_bytes = project_memory(&kept, !no_semantic);
    let disk_fits = match (index_bytes, budget_bytes) {
        (Some(index), Some(budget)) => Some(index <= budget),
        _ => None,
    };
    let memory_fits = match (memory_bytes, memory_budget) {
        (Some(used), Some(budget)) => Some(used <= budget),
        _ => None,
    };
    // Memory can refuse. It cannot turn an unmeasured disk plan into a fit:
    // `disk == None && memory == Some(true)` stays `None`.
    let fits = combine_fits(disk_fits, memory_fits);
    let source_bytes = kept.iter().map(|f| f.size).sum();
    let keep_rels = if kept.len() == facts.len() && !force_no_semantic {
        None
    } else {
        Some(kept.iter().map(|f| f.rel.clone()).collect())
    };
    Some(PlanEst {
        name: PLAN_NAMES
            .iter()
            .copied()
            .find(|n| *n == name)
            .unwrap_or("default"),
        files: kept.len() as u64,
        source_bytes,
        index_bytes,
        time_s,
        drops,
        no_semantic,
        keep: keep_rels,
        fits,
        memory_bytes,
    })
}

fn combine_fits(disk: Option<bool>, memory: Option<bool>) -> Option<bool> {
    if disk == Some(false) || memory == Some(false) {
        Some(false)
    } else if disk == Some(true) {
        Some(true)
    } else {
        None
    }
}

/// Structural memory for probed JSON records this plan keeps.
/// `None` when none of the kept files were probed, so a tree of source
/// code is not given a made-up memory bill.
fn project_memory(kept: &[&Fact], semantic: bool) -> Option<u64> {
    let per = if semantic {
        SEMANTIC_RECORD_BYTES
    } else {
        PLAIN_RECORD_BYTES
    };
    let mut any = false;
    let mut bytes = 0u64;
    let mut records = 0u64;
    for fact in kept {
        if let Some(n) = fact.records {
            any = true;
            records = records.saturating_add(n);
            bytes = bytes.saturating_add(fact.size);
        }
    }
    any.then(|| bytes.saturating_add(per.saturating_mul(records)))
}

fn keep(name: &str, fact: &Fact) -> bool {
    match name {
        "skip-vendored" => !fact.vendored,
        "skip-data" => !matches!(fact.kind, Kind::Data | Kind::Binary),
        "code-and-docs" | "code-and-docs-no-vectors" => {
            matches!(fact.kind, Kind::Code | Kind::Prose | Kind::Pdf)
        }
        "cap-1mb" => fact.size <= CAP_1MB,
        _ => true,
    }
}

fn project_kept(kept: &[&Fact], semantic: bool) -> (Option<u64>, Option<f64>) {
    if MEASURED.is_empty() {
        return (None, None);
    }
    let mut index = 0u64;
    let mut wall_ms = 0u64;
    let mut timed = false;
    let mut measured_all = true;
    let mut groups: BTreeMap<Model, (u64, u64)> = BTreeMap::new();
    for fact in kept {
        let entry = groups.entry(fact.model).or_default();
        entry.0 += 1;
        entry.1 += fact.size;
    }
    for (model, (files, source)) in groups {
        let Some(cal) = MEASURED
            .iter()
            .find(|c| c.model == model && c.semantic == semantic)
        else {
            measured_all = false;
            continue;
        };
        index = index.saturating_add(project_points(&cal.small, &cal.large, files, source));
        let sample = if cal.large.source_bytes >= cal.small.source_bytes {
            &cal.large
        } else {
            &cal.small
        };
        if sample.source_bytes > 0 && sample.wall_ms > 0 {
            wall_ms = wall_ms.saturating_add(
                ((source as u128) * u128::from(sample.wall_ms) / u128::from(sample.source_bytes))
                    as u64,
            );
            timed = true;
        }
    }
    let index = measured_all.then_some(index);
    let time_s = (timed && measured_all).then_some((wall_ms as f64) / 1000.0);
    (index, time_s)
}

/// `index_bytes ≈ per_file * files + source_bytes * num / den`, fitted to the
/// two measured points. A negative per-file intercept is clamped to 0 and the
/// slope becomes the steeper of the two measured index/source ratios, so the
/// projection stays at or above both measurements.
fn project_points(a: &Point, b: &Point, files: u64, source: u64) -> u64 {
    let fit = fit_points(a, b);
    fit.per_file
        .saturating_mul(files)
        .saturating_add(mul_div(source, fit.num, fit.den))
}

struct Fit {
    per_file: u64,
    num: u64,
    den: u64,
}

fn fit_points(a: &Point, b: &Point) -> Fit {
    let f1 = a.files as i128;
    let s1 = a.source_bytes as i128;
    let i1 = a.index_bytes as i128;
    let f2 = b.files as i128;
    let s2 = b.source_bytes as i128;
    let i2 = b.index_bytes as i128;
    let pden = f1 * s2 - f2 * s1;
    let pnum = i1 * s2 - i2 * s1;
    if pden == 0 {
        return steeper_ratio(a, b);
    }
    // p and pden have opposite signs when the intercept is negative.
    if pnum.signum() * pden.signum() < 0 {
        return steeper_ratio(a, b);
    }
    let per_file = (pnum / pden) as u64;
    let rnum = i1 * pden - pnum * f1;
    let rden = s1 * pden;
    if rden == 0 || rnum.signum() * rden.signum() < 0 {
        return steeper_ratio(a, b);
    }
    let (num, den) = reduce(rnum.unsigned_abs(), rden.unsigned_abs());
    Fit { per_file, num, den }
}

fn steeper_ratio(a: &Point, b: &Point) -> Fit {
    let pick = |p: &Point| -> Option<(u128, u128)> {
        if p.source_bytes == 0 {
            None
        } else {
            Some((u128::from(p.index_bytes), u128::from(p.source_bytes)))
        }
    };
    let best = match (pick(a), pick(b)) {
        (Some((i1, s1)), Some((i2, s2))) => {
            if i1 * s2 >= i2 * s1 {
                Some((i1, s1))
            } else {
                Some((i2, s2))
            }
        }
        (Some(r), None) | (None, Some(r)) => Some(r),
        (None, None) => None,
    };
    match best {
        Some((num, den)) => {
            let (num, den) = reduce(num, den);
            Fit {
                per_file: 0,
                num,
                den,
            }
        }
        None => {
            let files = a.files.max(b.files).max(1);
            let index = a.index_bytes.max(b.index_bytes);
            Fit {
                per_file: index / files,
                num: 0,
                den: 1,
            }
        }
    }
}

fn reduce(num: u128, den: u128) -> (u64, u64) {
    if den == 0 {
        return (0, 1);
    }
    let g = gcd(num, den);
    ((num / g) as u64, (den / g) as u64)
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

fn mul_div(value: u64, num: u64, den: u64) -> u64 {
    if den == 0 {
        return 0;
    }
    ((u128::from(value) * u128::from(num)) / u128::from(den)) as u64
}

fn recommended_plan(plans: &[PlanEst]) -> Option<&PlanEst> {
    let mut best: Option<&PlanEst> = None;
    for plan in plans {
        if plan.fits != Some(true) {
            continue;
        }
        best = Some(match best {
            None => plan,
            Some(cur) if plan.source_bytes > cur.source_bytes => plan,
            Some(cur) => cur,
        });
    }
    best
}

fn plan_json(plan: &PlanEst) -> Value {
    json!({
        "name": plan.name,
        "fits": plan.fits,
        "index_bytes": plan.index_bytes,
        "source_bytes": plan.source_bytes,
        "files": plan.files,
        "drops": plan.drops,
        "no_semantic": plan.no_semantic,
        "memory_bytes": plan.memory_bytes,
        "time_s": plan.time_s,
        "time_basis": plan.time_s.map(|_| "rough projection from the calibration host, not this machine"),
    })
}

fn plan_size_phrase(plan: &PlanEst) -> String {
    let disk = match plan.index_bytes {
        Some(bytes) => format!(
            "estimated index {} for {} ({} source)",
            human_bytes(bytes),
            count_phrase(plan.files, "file", "files"),
            human_bytes(plan.source_bytes)
        ),
        None => format!(
            "index size unmeasured for {} ({} source)",
            count_phrase(plan.files, "file", "files"),
            human_bytes(plan.source_bytes)
        ),
    };
    match plan.memory_bytes {
        Some(bytes) => format!("{disk}; structural memory about {}", human_bytes(bytes)),
        None => disk,
    }
}

fn continue_note(plan: &PlanEst, budget: Option<u64>, basis: &str) -> String {
    let budget = budget
        .map(human_bytes)
        .unwrap_or_else(|| "an unknown".into());
    let drops = if plan.drops.is_empty() {
        String::new()
    } else {
        format!(". drops: {}", plan.drops.join(", "))
    };
    let basis = if basis.is_empty() {
        String::new()
    } else {
        format!(" ({basis})")
    };
    let over = if plan.fits == Some(false) {
        ". warning: the estimate is over the budget; running because the plan was named. \
         The node's flood-stage watermark still stops writes"
    } else {
        ""
    };
    format!(
        "--plan {} keeps {}{drops}. {} Budget {budget}{basis}{over}",
        plan.name,
        count_phrase(plan.files, "file", "files"),
        plan_size_phrase(plan)
    )
}

fn summary_lines(
    facts: &[Fact],
    plans: &[PlanEst],
    budget: Option<u64>,
    basis: &str,
    recommended: Option<&str>,
) -> Vec<String> {
    let bytes = facts.iter().map(|f| f.size).sum::<u64>();
    let mut lines = vec![format!(
        "budget: {} ({}), {}",
        count_phrase(facts.len() as u64, "file", "files"),
        human_bytes(bytes),
        kind_phrase(facts)
    )];
    if let Some(n) = json_record_total(facts) {
        lines.push(format!(
            "budget: {} in .json (structural memory ceiling, not a measured store ratio)",
            count_phrase(n, "record", "records")
        ));
    }
    if let Some(default) = plans.iter().find(|p| p.name == "default") {
        let fit = match default.fits {
            Some(true) => "fits",
            Some(false) => "does not fit",
            None => "is not compared (unmeasured, or no budget)",
        };
        lines.push(format!(
            "budget: default plan {fit}: {}. budget {} ({basis})",
            plan_size_phrase(default),
            budget.map(human_bytes).unwrap_or_else(|| "unset".into())
        ));
    }
    for plan in plans
        .iter()
        .filter(|p| p.name != "default" && p.fits != Some(false))
    {
        let mark = if Some(plan.name) == recommended {
            "recommended"
        } else {
            "option"
        };
        let drop = if plan.drops.is_empty() {
            String::new()
        } else {
            format!(" — drops {}", plan.drops.join(", "))
        };
        lines.push(format!(
            "  {mark} {}: {}{drop}",
            plan.name,
            plan_size_phrase(plan)
        ));
    }
    lines
}

fn kind_phrase(facts: &[Fact]) -> String {
    let mut parts = Vec::new();
    for kind in [
        Kind::Code,
        Kind::Prose,
        Kind::Pdf,
        Kind::Data,
        Kind::Binary,
        Kind::Other,
    ] {
        let n = facts.iter().filter(|f| f.kind == kind).count();
        if n > 0 {
            parts.push(format!("{n} {}", kind.name()));
        }
    }
    if parts.is_empty() {
        "empty".to_string()
    } else {
        parts.join(", ")
    }
}

fn counts_by(facts: &[Fact], key: impl Fn(&Fact) -> &str) -> Value {
    let mut map: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for fact in facts {
        let entry = map.entry(key(fact)).or_default();
        entry.0 += 1;
        entry.1 += fact.size;
    }
    let mut out = serde_json::Map::new();
    for (k, (files, bytes)) in map {
        out.insert(k.to_string(), json!({"files": files, "bytes": bytes}));
    }
    Value::Object(out)
}

fn top_folder(rel: &str) -> &str {
    match rel.split_once('/') {
        Some((dir, rest)) if !dir.is_empty() && !rest.is_empty() => dir,
        _ => "(root)",
    }
}

fn fact_of(file: &FileEntry) -> Fact {
    let (kind, model) = classify(&file.rel);
    Fact {
        rel: file.rel.clone(),
        size: file.size,
        kind,
        model,
        vendored: order::vendored_reason(&file.rel).is_some(),
        records: probe_json_records(file),
    }
}

/// Record count for one `.json` file, matching the extractor's split:
/// a top-level array counts every element; an object sums every top-level
/// array of objects of length ≥ 2; anything else is one record. Parse
/// failure, gzip, and sizes outside [`JSON_PROBE_MIN`]..[`JSON_PROBE_MAX`]
/// return `None` — the file is unmeasured and does not refuse the run.
fn probe_json_records(file: &FileEntry) -> Option<u64> {
    if extension(&file.rel) != "json" {
        return None;
    }
    if file.size < JSON_PROBE_MIN || file.size >= JSON_PROBE_MAX {
        return None;
    }
    let bytes = std::fs::read(&file.path).ok()?;
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    Some(json_value_records(&value))
}

fn json_value_records(value: &Value) -> u64 {
    match value {
        Value::Array(arr) => arr.len() as u64,
        Value::Object(obj) => {
            let mut sum = 0u64;
            let mut any = false;
            for child in obj.values() {
                let Value::Array(arr) = child else {
                    continue;
                };
                if arr.len() >= 2 && arr.iter().all(|el| el.is_object()) {
                    sum = sum.saturating_add(arr.len() as u64);
                    any = true;
                }
            }
            if any {
                sum
            } else {
                1
            }
        }
        _ => 1,
    }
}

fn json_record_total(facts: &[Fact]) -> Option<u64> {
    facts
        .iter()
        .fold(None, |acc, fact| match (acc, fact.records) {
            (None, None) => None,
            (Some(n), None) | (None, Some(n)) => Some(n),
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
        })
}

/// Extension only. Sniff has not run; the census says so.
fn classify(rel: &str) -> (Kind, Model) {
    let ext = extension(rel);
    if is_code_ext(&ext) {
        return (Kind::Code, Model::Code);
    }
    if ext == "pdf" {
        return (Kind::Pdf, Model::Pdf);
    }
    if matches!(
        ext.as_str(),
        "csv" | "tsv" | "jsonl" | "ndjson" | "sqlite" | "db" | "xlsx" | "xls" | "sql"
    ) {
        return (Kind::Data, Model::Data);
    }
    if matches!(
        ext.as_str(),
        "hdf"
            | "h5"
            | "hdf5"
            | "parquet"
            | "npy"
            | "npz"
            | "nc"
            | "cdf"
            | "feather"
            | "arrow"
            | "mat"
            | "fits"
            | "bvh"
    ) {
        return (Kind::Data, Model::Unindexed);
    }
    if matches!(
        ext.as_str(),
        "md" | "markdown"
            | "rst"
            | "txt"
            | "adoc"
            | "asciidoc"
            | "org"
            | "html"
            | "htm"
            | "tex"
            | "rtf"
            | "docx"
            | "doc"
            | "pptx"
            | "epub"
    ) {
        return (Kind::Prose, Model::Prose);
    }
    if matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "ico"
            | "zip"
            | "gz"
            | "tgz"
            | "bz2"
            | "xz"
            | "7z"
            | "rar"
            | "tar"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "bin"
            | "wasm"
            | "class"
            | "jar"
            | "mp3"
            | "mp4"
            | "mov"
            | "avi"
            | "pyc"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "dmg"
            | "iso"
            | "img"
    ) {
        return (Kind::Binary, Model::Unindexed);
    }
    (Kind::Other, Model::Other)
}

fn extension(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Test seam: project `files`/`source` through two measured points.
#[cfg(test)]
fn project_for_test(a: Point, b: Point, files: u64, source: u64) -> u64 {
    project_points(&a, &b, files, source)
}

#[cfg(test)]
fn entry(rel: &str, size: u64) -> FileEntry {
    FileEntry {
        path: Path::new(rel).to_path_buf(),
        rel: rel.to_string(),
        rel_id: rel.to_string(),
        is_symlink: false,
        size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(files: u64, source: u64, index: u64) -> Point {
        Point {
            files,
            source_bytes: source,
            index_bytes: index,
            wall_ms: 0,
        }
    }

    #[test]
    fn two_points_reproduce_within_one_byte() {
        let a = point(1, 100, 150);
        let b = point(2, 400, 500);
        assert_eq!(project_for_test(a, b, 1, 100), 150);
        assert_eq!(project_for_test(a, b, 2, 400), 500);
        // Same slope, a third size: 50 per file + 1 per source byte.
        assert_eq!(project_for_test(a, b, 4, 1000), 4 * 50 + 1000);
    }

    #[test]
    fn a_negative_per_file_intercept_uses_the_steeper_measured_ratio() {
        // Unconstrained per-file term is negative (see fit_points).
        let a = point(5, 100, 10);
        let b = point(1, 1000, 900);
        let fit = fit_points(&a, &b);
        assert_eq!(fit.per_file, 0);
        // 900/1000 is steeper than 10/100.
        assert_eq!(fit.num as u128 * 1000, fit.den as u128 * 900);
        let projected = project_for_test(a, b, 1, 1000);
        assert!(projected >= 900, "{projected}");
        assert!(project_for_test(a, b, 5, 100) >= 10);
    }

    #[test]
    fn disk_budget_uses_the_reported_flood_reserve_and_does_not_guess() {
        let volume = Volume {
            total: Some(10_000),
            avail: Some(4_000),
            used_percent: Some(60),
            flood_stage: Some(95),
            threshold_enabled: true,
            flood_blocked: false,
        };
        // Reserve is 5% of 10000 = 500. Budget is 4000 - 500.
        assert_eq!(
            disk_budget(&volume),
            Some(DiskBudget::UntilFlood {
                bytes: 3_500,
                flood_percent: 95,
                avail: 4_000,
                total: 10_000,
            })
        );
        let mut off = volume;
        off.flood_stage = Some(0);
        assert_eq!(
            disk_budget(&off),
            Some(DiskBudget::WatermarkOff {
                bytes: 4_000,
                avail: 4_000,
            })
        );
        let mut missing = volume;
        missing.flood_stage = None;
        assert!(disk_budget(&missing).is_none());
        let mut no_total = volume;
        no_total.total = None;
        assert!(disk_budget(&no_total).is_none());
    }

    fn sample_files() -> Vec<FileEntry> {
        vec![
            entry("src/main.rs", 100),
            entry("docs/readme.md", 50),
            entry("data/big.csv", 10_000),
            entry("vendor/lib/x.rs", 80),
            entry("pic.png", 500),
        ]
    }

    #[test]
    fn the_measured_points_are_not_projected_below_themselves() {
        for cal in MEASURED {
            for point in [&cal.small, &cal.large] {
                let got = project_points(&cal.small, &cal.large, point.files, point.source_bytes);
                assert!(
                    got + 8 >= point.index_bytes,
                    "{:?} semantic={} files={} source={} measured={} projected={got}",
                    cal.model,
                    cal.semantic,
                    point.files,
                    point.source_bytes,
                    point.index_bytes
                );
            }
        }
    }

    #[test]
    fn a_one_byte_cap_does_not_fit_and_does_not_start() {
        let files = sample_files();
        let assessment = assess(&Request {
            root: "/data",
            files: &files,
            volume: None,
            max_index_bytes: Some(1),
            no_semantic: true,
            plan: None,
            approve: None,
            duplicate_prefixes: &[],
            memory_budget: None,
        });
        assert!(assessment.needs_choice, "{}", assessment.body);
        assert_eq!(
            assessment.body["plans"][0]["fits"],
            serde_json::Value::Bool(false)
        );
        assert!(matches!(
            resolve(&assessment, Choice::Absent),
            Outcome::Stop {
                code: gate::EXIT_NEEDS_DECISION,
                ..
            }
        ));
    }

    #[test]
    fn a_named_plan_over_the_estimate_runs_with_a_warning() {
        let files = sample_files();
        let assessment = assess(&Request {
            root: "/data",
            files: &files,
            volume: None,
            max_index_bytes: Some(1),
            no_semantic: true,
            plan: Some("default"),
            approve: None,
            duplicate_prefixes: &[],
            memory_budget: None,
        });
        assert!(!assessment.needs_choice, "{}", assessment.body);
        match resolve(&assessment, Choice::Absent) {
            Outcome::Continue { keep, note, .. } => {
                assert!(keep.is_none(), "default keeps every file");
                assert!(note.contains("over the budget"), "{note}");
            }
            Outcome::Stop { code, body } => panic!("stopped with {code}: {body}"),
        }
    }

    #[test]
    fn kinds_follow_the_extension_before_sniff() {
        assert_eq!(classify("a.rs").0, Kind::Code);
        // #2357: brackets are directory names. A Windows rel that still
        // contains `\` (the walk rewrites those to `/` before this runs)
        // keeps the `.tsx` extension.
        assert_eq!(
            classify("app/[locale]/booking/[token]/temple-selection/TempleSelectionWrapper.tsx").0,
            Kind::Code
        );
        assert_eq!(
            classify(
                "app\\[locale]\\booking\\[token]\\temple-selection\\TempleSelectionWrapper.tsx"
            )
            .0,
            Kind::Code
        );
        assert_eq!(classify("a.md").0, Kind::Prose);
        assert_eq!(classify("a.pdf").0, Kind::Pdf);
        assert_eq!(classify("a.csv").0, Kind::Data);
        assert_eq!(classify("a.h5").0, Kind::Data);
        assert_eq!(classify("a.h5").1, Model::Unindexed);
        assert_eq!(classify("a.png").0, Kind::Binary);
        assert_eq!(classify("Makefile").0, Kind::Other);
        assert!(order::vendored_reason("vendor/lib.rs").is_some());
    }

    #[test]
    fn byte_sizes_reject_zero_and_accept_a_suffix() {
        assert_eq!(parse_byte_size("1024").unwrap(), 1024);
        assert_eq!(parse_byte_size("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_byte_size("1GB").unwrap(), 1024 * 1024 * 1024);
        assert!(parse_byte_size("0").is_err());
        assert!(parse_byte_size("0MB").is_err());
        assert!(parse_byte_size("soon").is_err());
    }

    #[test]
    fn a_choice_of_cancel_or_a_plan_name_is_read_once() {
        let names = vec!["code-and-docs".to_string()];
        let mut input = std::io::Cursor::new("code-and-docs\n");
        assert_eq!(
            read_choice(&mut input, &names, |_| {}),
            Choice::Plan("code-and-docs".into())
        );
        let mut input = std::io::Cursor::new("cancel\n");
        assert_eq!(read_choice(&mut input, &names, |_| {}), Choice::Cancel);
    }

    fn assignment(rel: &str, datasets: &[&str]) -> crate::state::FileAssignment {
        crate::state::FileAssignment {
            rel: rel.to_string(),
            path_id: format!("unix:{rel}"),
            is_symlink: Some(false),
            family: "text".into(),
            gzip: false,
            content_digest: None,
            assignments: datasets
                .iter()
                .map(|slug| (None, (*slug).to_string()))
                .collect(),
            as_document: false,
        }
    }

    fn walked(rel: &str) -> FileEntry {
        FileEntry {
            path: std::path::PathBuf::from(rel),
            rel: rel.to_string(),
            rel_id: format!("unix:{rel}"),
            is_symlink: false,
            size: 8,
        }
    }

    fn plan_with(files: Vec<(&str, &str, &[&str])>) -> crate::state::Plan {
        let mut plan = crate::state::Plan::default();
        for (key, rel, datasets) in files {
            plan.files
                .insert(key.to_string(), assignment(rel, datasets));
        }
        plan
    }

    #[test]
    fn a_reduced_plan_lists_indexed_files_it_would_drop_and_the_datasets_it_empties() {
        let prior = plan_with(vec![
            ("key-rs", "main.rs", &["code"]),
            ("key-md", "notes.md", &["docs"]),
            ("key-csv", "rows.csv", &["rows"]),
            ("key-gone", "old.csv", &["rows"]),
        ]);
        let discovered = vec![walked("main.rs"), walked("notes.md"), walked("rows.csv")];
        let body = removal_decision(
            "code-and-docs",
            &[&prior],
            &discovered,
            &["main.rs".into(), "notes.md".into()],
        )
        .expect("rows.csv is indexed and the plan leaves it out");
        assert_eq!(body["xerj"], "autoindex-plan-removal");
        assert_eq!(body["exit_code"], crate::gate::EXIT_NEEDS_DECISION);
        assert_eq!(body["decision_required"], true);
        assert_eq!(body["plan"], "code-and-docs");
        assert_eq!(body["would_remove_total"], 1);
        assert_eq!(body["would_remove_truncated"], false);
        assert_eq!(body["would_remove"][0]["path"], "rows.csv");
        assert_eq!(body["would_remove"][0]["file_key"], "key-csv");
        assert_eq!(body["would_remove"][0]["datasets"][0], "rows");
        // old.csv is not in the walk: a real deletion, not this decision.
        // rows.csv is the only remaining rows file, so the dataset would be empty.
        assert_eq!(body["datasets"][0], "rows");
        assert_eq!(body["datasets"].as_array().map(Vec::len), Some(1));
        let accept = body["accept"].as_str().unwrap();
        assert!(accept.contains("rows.csv"), "{accept}");
        assert!(accept.contains("rows"), "{accept}");
        assert!(accept.contains("--drop-indexed"), "{accept}");
        assert!(
            !accept.contains("old.csv"),
            "a file already gone from the folder is not a plan removal: {accept}"
        );
    }

    #[test]
    fn a_kept_sibling_keeps_the_dataset_off_the_emptied_list() {
        let prior = plan_with(vec![
            ("key-a", "keep.csv", &["tables"]),
            ("key-b", "drop.csv", &["tables"]),
        ]);
        let discovered = vec![walked("keep.csv"), walked("drop.csv")];
        let body = removal_decision("skip-data", &[&prior], &discovered, &["keep.csv".into()])
            .expect("drop.csv would be removed");
        assert_eq!(body["would_remove"][0]["path"], "drop.csv");
        assert!(
            body["datasets"].as_array().unwrap().is_empty(),
            "tables still has keep.csv: {}",
            body
        );
    }

    #[test]
    fn a_plan_that_keeps_every_indexed_file_has_nothing_to_confirm() {
        let prior = plan_with(vec![("key-rs", "main.rs", &["code"])]);
        let discovered = vec![walked("main.rs"), walked("new.rs")];
        assert!(removal_decision(
            "code-and-docs",
            &[&prior],
            &discovered,
            &["main.rs".into(), "new.rs".into()],
        )
        .is_none());
    }

    #[test]
    fn the_removal_list_is_capped_and_the_total_is_not() {
        let mut plan = crate::state::Plan::default();
        let mut discovered = Vec::new();
        for i in 0..(REMOVAL_LIST_CAP + 3) {
            let rel = format!("f{i:02}.csv");
            plan.files
                .insert(format!("key-{i}"), assignment(&rel, &["tables"]));
            discovered.push(walked(&rel));
        }
        let body = removal_decision("skip-data", &[&plan], &discovered, &[])
            .expect("every csv is indexed and the plan keeps none");
        assert_eq!(body["would_remove_total"], REMOVAL_LIST_CAP + 3);
        assert_eq!(body["would_remove_truncated"], true);
        assert_eq!(
            body["would_remove"].as_array().map(Vec::len),
            Some(REMOVAL_LIST_CAP)
        );
        assert_eq!(body["would_remove"][0]["path"], "f00.csv");
        assert_eq!(body["datasets"][0], "tables");
        let accept = body["accept"].as_str().unwrap();
        assert!(accept.contains("and 3 more"), "{accept}");
    }

    #[test]
    fn the_same_file_in_two_plans_is_listed_once() {
        let prior = plan_with(vec![("key-csv", "rows.csv", &["rows"])]);
        let discovered = vec![walked("rows.csv")];
        let body = removal_decision("code-and-docs", &[&prior, &prior], &discovered, &[])
            .expect("one file");
        assert_eq!(body["would_remove_total"], 1);
    }

    #[test]
    fn json_record_count_sums_top_level_arrays_of_objects() {
        let value = serde_json::json!({
            "functions": [{"n": 1}, {"n": 2}, {"n": 3}],
            "runs": [{"a": 1}, {"a": 2}],
            "note": "x"
        });
        assert_eq!(json_value_records(&value), 5);
        assert_eq!(json_value_records(&serde_json::json!([1, 2, 3, 4])), 4);
        assert_eq!(json_value_records(&serde_json::json!({"a": 1})), 1);
    }

    /// #2102 priced disk only. A wide JSON array has no measured store ratio
    /// (`.json` is `other`), so disk never refuses it. The record count does,
    /// against the memory ceiling, and `no-vectors` is the cheaper plan.
    /// A memory ceiling the file fits under must not flip an unmeasured disk
    /// plan to `fits: true`.
    #[test]
    fn counted_json_records_can_refuse_on_memory_without_a_disk_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let records = 3_000u64;
        let pad = "p".repeat(80);
        let path = dir.path().join("rows.json");
        let mut body = String::new();
        body.push('[');
        for i in 0..records {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!(r#"{{"id":{i},"pad":"{pad}"}}"#));
        }
        body.push(']');
        let size = body.len() as u64;
        assert!(size >= JSON_PROBE_MIN, "{size}");
        assert!(
            size <= CAP_1MB,
            "cap-1mb must still see this file, got {size}"
        );
        std::fs::write(&path, &body).unwrap();
        let files = vec![FileEntry {
            path,
            rel: "rows.json".into(),
            rel_id: "rows.json".into(),
            is_symlink: false,
            size,
        }];
        let plain = size + PLAIN_RECORD_BYTES * records;
        let semantic = size + SEMANTIC_RECORD_BYTES * records;
        assert!(plain < semantic);

        // Disk budget is large enough that the measured `other` ratio fits.
        // Memory is what separates default from no-vectors.
        let request = |memory_budget, max_index_bytes| Request {
            root: dir.path().to_str().unwrap(),
            files: &files,
            volume: None,
            max_index_bytes,
            no_semantic: false,
            plan: None,
            approve: None,
            duplicate_prefixes: &[],
            memory_budget,
        };
        let tight = assess(&request(Some(plain + 1), Some(u64::MAX)));
        assert_eq!(tight.body["json_records"], serde_json::json!(records));
        let plan = |body: &Value, name: &str| -> Value {
            body["plans"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == name)
                .unwrap()
                .clone()
        };
        let default = plan(&tight.body, "default");
        assert_eq!(default["fits"], serde_json::json!(false), "{default}");
        assert!(tight.needs_choice, "{}", tight.body);
        let no_vectors = plan(&tight.body, "no-vectors");
        assert_eq!(no_vectors["fits"], serde_json::json!(true), "{no_vectors}");
        assert_eq!(tight.body["recommended"], serde_json::json!("no-vectors"));
        assert!(
            tight
                .lines
                .iter()
                .any(|line| line.contains("3000") && line.contains("record")),
            "{:?}",
            tight.lines
        );

        let open = assess(&request(Some(u64::MAX), None));
        let default = plan(&open.body, "default");
        assert!(
            default["fits"].is_null(),
            "memory fitting must not accept an unmeasured disk plan: {default}"
        );
        assert!(default["memory_bytes"].as_u64().unwrap() >= semantic);
        assert!(!open.needs_choice);

        let missing = sample_files();
        let untouched = assess(&Request {
            root: "/data",
            files: &missing,
            volume: None,
            max_index_bytes: None,
            no_semantic: false,
            plan: None,
            approve: None,
            duplicate_prefixes: &[],
            memory_budget: Some(1),
        });
        assert!(untouched.body["json_records"].is_null());
        assert!(untouched.body["plans"][0]["fits"].is_null());
        assert!(!untouched.needs_choice);
    }

    #[test]
    fn a_json_file_that_does_not_parse_does_not_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        let body = "{".repeat(JSON_PROBE_MIN as usize + 8);
        std::fs::write(&path, &body).unwrap();
        let files = vec![FileEntry {
            path,
            rel: "broken.json".into(),
            rel_id: "broken.json".into(),
            is_symlink: false,
            size: body.len() as u64,
        }];
        let assessment = assess(&Request {
            root: dir.path().to_str().unwrap(),
            files: &files,
            volume: None,
            max_index_bytes: None,
            no_semantic: false,
            plan: None,
            approve: None,
            duplicate_prefixes: &[],
            memory_budget: Some(1),
        });
        assert!(assessment.body["json_records"].is_null());
        assert!(!assessment.needs_choice, "{}", assessment.body);
    }
}
