//! Deterministic CVSS v3.1 and v4.0 scoring, EPSS/KEV corpus context, and
//! asset-driven business-impact evaluation (#2775, #2767, #2769).
//!
//! All calculations are pure Rust with no model inference, no network I/O, and
//! no guessed threat-intelligence values:
//! - CVSS v3.1 Base, Temporal, and Environmental scores follow the FIRST CVSS
//!   v3.1 specification (including Appendix A `RoundUp` and the v3.1 `MISS`
//!   exponent 13 in Modified Impact).
//! - CVSS v4.0 Base (CVSS-B), Base+Threat (CVSS-BT), Base+Environmental
//!   (CVSS-BE), and Base+Threat+Environmental (CVSS-BTE) scores follow the
//!   FIRST CVSS v4.0 MacroVector (`EQ1..EQ6`) lookup and severity-distance
//!   interpolation specification.
//! - EPSS and CISA KEV context require explicit hub corpus provenance
//!   (`source_corpus`, 40-hex `pin_sha`, and `fetched_at` date); when absent,
//!   they serialize as `{"status": "absent", ...}` rather than defaulting to
//!   `0.0` or `false`.
//! - When an evaluation names a live endpoint target in `AssetContext`, it
//!   requires a verified `cyber_scope::ScopeGuard` from an authorized control
//!   plane and echoes the `WorkOrder` inside the JCS-sealed report.

use crate::cyber_scope::{InputOrigin, ScopeError, ScopeGuard, WorkOrder};
use crate::hub_evidence::{sha256, PinnedCitation};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const SCORE_REPORT_VERSION: &str = "xerj-cyber-score/v1";
const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEXT: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum ScoreError {
    #[error("invalid CVSS vector: {0}")]
    InvalidCvssVector(String),
    #[error("invalid CVE identifier `{0}` (expected CVE-YYYY-NNNN+)")]
    InvalidCveId(String),
    #[error("invalid threat-intel context: {0}")]
    InvalidThreatIntel(&'static str),
    #[error("invalid asset context: {0}")]
    InvalidAssetContext(&'static str),
    #[error("cyber score refused: endpoint target requires a declared ScopeGuard")]
    ScopeRequired,
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error("cyber score report digest mismatch")]
    DigestMismatch,
    #[error("cyber score report scope does not match the supplied ScopeGuard")]
    ScopeMismatch,
    #[error("cyber score JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ScoreError>;

/// Qualitative severity band shared by CVSS v3.1 and v4.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CvssSeverity {
    None,
    Low,
    Medium,
    High,
    Critical,
}

impl CvssSeverity {
    pub fn from_score(score: f64) -> Self {
        if score <= 0.0 {
            Self::None
        } else if score < 4.0 {
            Self::Low
        } else if score < 7.0 {
            Self::Medium
        } else if score < 9.0 {
            Self::High
        } else {
            Self::Critical
        }
    }
}

/// FIRST CVSS v3.1 Appendix A `RoundUp` using exact integer arithmetic.
pub fn cvss31_round_up(input: f64) -> f64 {
    let int_input = (input * 100_000.0).round() as i64;
    if int_input % 10_000 == 0 {
        (int_input as f64) / 100_000.0
    } else {
        (((int_input / 10_000) + 1) as f64) / 10.0
    }
}

/// FIRST CVSS v4.0 one-decimal rounding (`roundHalfUp`).
pub fn cvss40_round(input: f64) -> f64 {
    let clamped = input.clamp(0.0, 10.0);
    ((clamped + 1e-9) * 10.0).round() / 10.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CvssV31Scores {
    pub vector: String,
    pub base_score: f64,
    pub base_severity: CvssSeverity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal_severity: Option<CvssSeverity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environmental_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environmental_severity: Option<CvssSeverity>,
    pub effective_score: f64,
    pub effective_severity: CvssSeverity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CvssV40Scores {
    pub vector: String,
    pub nomenclature: String,
    pub macro_vector: String,
    pub score: f64,
    pub severity: CvssSeverity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub enum CvssEvaluation {
    #[serde(rename = "3.1")]
    V31(CvssV31Scores),
    #[serde(rename = "4.0")]
    V40(CvssV40Scores),
}

impl CvssEvaluation {
    pub fn parse(vector: &str) -> Result<Self> {
        if vector.starts_with("CVSS:3.1/") {
            Ok(Self::V31(score_cvss_v31(vector)?))
        } else if vector.starts_with("CVSS:4.0/") {
            Ok(Self::V40(score_cvss_v40(vector)?))
        } else if vector.starts_with("CVSS:3.0/") {
            Err(ScoreError::InvalidCvssVector(
                "CVSS:3.0 is not supported by cyber.score; supply a CVSS:3.1/... or CVSS:4.0/... vector".into(),
            ))
        } else {
            Err(ScoreError::InvalidCvssVector(
                "vector must start with CVSS:3.1/ or CVSS:4.0/".into(),
            ))
        }
    }

    pub fn effective_score(&self) -> f64 {
        match self {
            Self::V31(s) => s.effective_score,
            Self::V40(s) => s.score,
        }
    }

    pub fn effective_severity(&self) -> CvssSeverity {
        match self {
            Self::V31(s) => s.effective_severity,
            Self::V40(s) => s.severity,
        }
    }

    pub fn vector(&self) -> &str {
        match self {
            Self::V31(s) => &s.vector,
            Self::V40(s) => &s.vector,
        }
    }
}

/// Compute CVSS v3.1 Base, Temporal, and Environmental scores from a vector string.
pub fn score_cvss_v31(vector: &str) -> Result<CvssV31Scores> {
    let rest = vector.strip_prefix("CVSS:3.1/").ok_or_else(|| {
        ScoreError::InvalidCvssVector("CVSS v3.1 vector must start with CVSS:3.1/".into())
    })?;
    if rest.is_empty() || rest.ends_with('/') || rest.starts_with('/') {
        return Err(ScoreError::InvalidCvssVector(
            "empty or trailing '/' in CVSS v3.1 vector".into(),
        ));
    }

    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for part in rest.split('/') {
        let (k, v) = part.split_once(':').ok_or_else(|| {
            ScoreError::InvalidCvssVector(format!("malformed metric segment `{part}`"))
        })?;
        if k.is_empty() || v.is_empty() {
            return Err(ScoreError::InvalidCvssVector(format!(
                "empty metric key or value in `{part}`"
            )));
        }
        if seen.insert(k, v).is_some() {
            return Err(ScoreError::InvalidCvssVector(format!(
                "duplicate CVSS v3.1 metric `{k}`"
            )));
        }
        validate_v31_metric(k, v)?;
    }

    for req in ["AV", "AC", "PR", "UI", "S", "C", "I", "A"] {
        if !seen.contains_key(req) {
            return Err(ScoreError::InvalidCvssVector(format!(
                "missing mandatory CVSS v3.1 base metric `{req}`"
            )));
        }
    }

    let av = v31_av(seen["AV"]);
    let ac = v31_ac(seen["AC"]);
    let s_changed = seen["S"] == "C";
    let pr = v31_pr(seen["PR"], s_changed);
    let ui = v31_ui(seen["UI"]);
    let c = v31_cia(seen["C"]);
    let i = v31_cia(seen["I"]);
    let a = v31_cia(seen["A"]);

    let iss = 1.0 - (1.0 - c) * (1.0 - i) * (1.0 - a);
    let impact = if s_changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powi(15)
    } else {
        6.42 * iss
    };
    let exploitability = 8.22 * av * ac * pr * ui;
    let base_score = if impact <= 0.0 {
        0.0
    } else if s_changed {
        cvss31_round_up((1.08 * (impact + exploitability)).min(10.0))
    } else {
        cvss31_round_up((impact + exploitability).min(10.0))
    };
    let base_severity = CvssSeverity::from_score(base_score);

    let e_val = seen.get("E").copied().unwrap_or("X");
    let rl_val = seen.get("RL").copied().unwrap_or("X");
    let rc_val = seen.get("RC").copied().unwrap_or("X");
    let has_temporal = e_val != "X" || rl_val != "X" || rc_val != "X";

    let e_w = v31_e(e_val);
    let rl_w = v31_rl(rl_val);
    let rc_w = v31_rc(rc_val);

    let (temporal_score, temporal_severity) = if has_temporal {
        let t = cvss31_round_up(base_score * e_w * rl_w * rc_w);
        (Some(t), Some(CvssSeverity::from_score(t)))
    } else {
        (None, None)
    };

    let env_keys = [
        "CR", "IR", "AR", "MAV", "MAC", "MPR", "MUI", "MS", "MC", "MI", "MA",
    ];
    let has_env = env_keys
        .iter()
        .any(|k| seen.get(k).copied().unwrap_or("X") != "X");

    let (environmental_score, environmental_severity) = if has_env {
        let cr = v31_req(seen.get("CR").copied().unwrap_or("X"));
        let ir = v31_req(seen.get("IR").copied().unwrap_or("X"));
        let ar = v31_req(seen.get("AR").copied().unwrap_or("X"));

        let ms_str = match seen.get("MS").copied().unwrap_or("X") {
            "X" => seen["S"],
            other => other,
        };
        let ms_changed = ms_str == "C";
        let mav = v31_av(match seen.get("MAV").copied().unwrap_or("X") {
            "X" => seen["AV"],
            other => other,
        });
        let mac = v31_ac(match seen.get("MAC").copied().unwrap_or("X") {
            "X" => seen["AC"],
            other => other,
        });
        let mpr = v31_pr(
            match seen.get("MPR").copied().unwrap_or("X") {
                "X" => seen["PR"],
                other => other,
            },
            ms_changed,
        );
        let mui = v31_ui(match seen.get("MUI").copied().unwrap_or("X") {
            "X" => seen["UI"],
            other => other,
        });
        let mc = v31_cia(match seen.get("MC").copied().unwrap_or("X") {
            "X" => seen["C"],
            other => other,
        });
        let mi = v31_cia(match seen.get("MI").copied().unwrap_or("X") {
            "X" => seen["I"],
            other => other,
        });
        let ma = v31_cia(match seen.get("MA").copied().unwrap_or("X") {
            "X" => seen["A"],
            other => other,
        });

        let miss = (1.0 - (1.0 - mc * cr) * (1.0 - mi * ir) * (1.0 - ma * ar)).min(0.915);
        let mod_impact = if ms_changed {
            7.52 * (miss - 0.029) - 3.25 * (miss * 0.9731 - 0.02).powi(13)
        } else {
            6.42 * miss
        };
        let mod_exploit = 8.22 * mav * mac * mpr * mui;
        let env = if mod_impact <= 0.0 {
            0.0
        } else if ms_changed {
            let inner = cvss31_round_up((1.08 * (mod_impact + mod_exploit)).min(10.0));
            cvss31_round_up(inner * e_w * rl_w * rc_w)
        } else {
            let inner = cvss31_round_up((mod_impact + mod_exploit).min(10.0));
            cvss31_round_up(inner * e_w * rl_w * rc_w)
        };
        (Some(env), Some(CvssSeverity::from_score(env)))
    } else {
        (None, None)
    };

    let effective_score = environmental_score.or(temporal_score).unwrap_or(base_score);
    let effective_severity = CvssSeverity::from_score(effective_score);

    Ok(CvssV31Scores {
        vector: vector.to_string(),
        base_score,
        base_severity,
        temporal_score,
        temporal_severity,
        environmental_score,
        environmental_severity,
        effective_score,
        effective_severity,
    })
}

fn validate_v31_metric(k: &str, v: &str) -> Result<()> {
    let ok = match k {
        "AV" => matches!(v, "N" | "A" | "L" | "P"),
        "AC" => matches!(v, "L" | "H"),
        "PR" => matches!(v, "N" | "L" | "H"),
        "UI" => matches!(v, "N" | "R"),
        "S" => matches!(v, "U" | "C"),
        "C" | "I" | "A" => matches!(v, "N" | "L" | "H"),
        "E" => matches!(v, "X" | "U" | "P" | "F" | "H"),
        "RL" => matches!(v, "X" | "O" | "T" | "W" | "U"),
        "RC" => matches!(v, "X" | "U" | "R" | "C"),
        "CR" | "IR" | "AR" => matches!(v, "X" | "L" | "M" | "H"),
        "MAV" => matches!(v, "X" | "N" | "A" | "L" | "P"),
        "MAC" => matches!(v, "X" | "L" | "H"),
        "MPR" => matches!(v, "X" | "N" | "L" | "H"),
        "MUI" => matches!(v, "X" | "N" | "R"),
        "MS" => matches!(v, "X" | "U" | "C"),
        "MC" | "MI" | "MA" => matches!(v, "X" | "N" | "L" | "H"),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ScoreError::InvalidCvssVector(format!(
            "invalid CVSS v3.1 metric or value `{k}:{v}`"
        )))
    }
}

fn v31_av(v: &str) -> f64 {
    match v {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.20,
        _ => unreachable!(),
    }
}
fn v31_ac(v: &str) -> f64 {
    match v {
        "L" => 0.77,
        "H" => 0.44,
        _ => unreachable!(),
    }
}
fn v31_pr(v: &str, scope_changed: bool) -> f64 {
    match (v, scope_changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.50,
        _ => unreachable!(),
    }
}
fn v31_ui(v: &str) -> f64 {
    match v {
        "N" => 0.85,
        "R" => 0.62,
        _ => unreachable!(),
    }
}
fn v31_cia(v: &str) -> f64 {
    match v {
        "N" => 0.00,
        "L" => 0.22,
        "H" => 0.56,
        _ => unreachable!(),
    }
}
fn v31_e(v: &str) -> f64 {
    match v {
        "X" | "H" => 1.00,
        "F" => 0.97,
        "P" => 0.94,
        "U" => 0.91,
        _ => unreachable!(),
    }
}
fn v31_rl(v: &str) -> f64 {
    match v {
        "X" | "U" => 1.00,
        "W" => 0.97,
        "T" => 0.96,
        "O" => 0.95,
        _ => unreachable!(),
    }
}
fn v31_rc(v: &str) -> f64 {
    match v {
        "X" | "C" => 1.00,
        "R" => 0.96,
        "U" => 0.92,
        _ => unreachable!(),
    }
}
fn v31_req(v: &str) -> f64 {
    match v {
        "X" | "M" => 1.0,
        "L" => 0.5,
        "H" => 1.5,
        _ => unreachable!(),
    }
}

const V40_ORDER: &[&str] = &[
    "AV", "AC", "AT", "PR", "UI", "VC", "VI", "VA", "SC", "SI", "SA", "E", "CR", "IR", "AR", "MAV",
    "MAC", "MAT", "MPR", "MUI", "MVC", "MVI", "MVA", "MSC", "MSI", "MSA", "S", "AU", "R", "V",
    "RE", "U",
];

/// Compute CVSS v4.0 score, nomenclature (`CVSS-B`, `CVSS-BT`, `CVSS-BE`,
/// `CVSS-BTE`), and MacroVector (`EQ1..EQ6`) from a `CVSS:4.0/...` vector.
pub fn score_cvss_v40(vector: &str) -> Result<CvssV40Scores> {
    let rest = vector.strip_prefix("CVSS:4.0/").ok_or_else(|| {
        ScoreError::InvalidCvssVector("CVSS v4.0 vector must start with CVSS:4.0/".into())
    })?;
    if rest.is_empty() || rest.ends_with('/') || rest.starts_with('/') {
        return Err(ScoreError::InvalidCvssVector(
            "empty or trailing '/' in CVSS v4.0 vector".into(),
        ));
    }

    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    let mut last_idx: Option<usize> = None;
    for part in rest.split('/') {
        let (k, v) = part.split_once(':').ok_or_else(|| {
            ScoreError::InvalidCvssVector(format!("malformed metric segment `{part}`"))
        })?;
        let idx = V40_ORDER.iter().position(|&m| m == k).ok_or_else(|| {
            ScoreError::InvalidCvssVector(format!("unknown CVSS v4.0 metric `{k}`"))
        })?;
        if let Some(prev) = last_idx {
            if idx <= prev {
                let reason = if seen.contains_key(k) {
                    format!("duplicate CVSS v4.0 metric `{k}`")
                } else {
                    format!(
                        "CVSS v4.0 metric `{k}` is out of canonical order (after `{}`)",
                        V40_ORDER[prev]
                    )
                };
                return Err(ScoreError::InvalidCvssVector(reason));
            }
        }
        last_idx = Some(idx);
        validate_v40_metric(k, v)?;
        seen.insert(k, v);
    }

    for &req in &V40_ORDER[..11] {
        if !seen.contains_key(req) {
            return Err(ScoreError::InvalidCvssVector(format!(
                "missing mandatory CVSS v4.0 base metric `{req}`"
            )));
        }
    }

    let has_threat = seen.get("E").copied().unwrap_or("X") != "X";
    let env_keys = &V40_ORDER[12..26];
    let has_env = env_keys
        .iter()
        .any(|k| seen.get(k).copied().unwrap_or("X") != "X");
    let nomenclature = match (has_threat, has_env) {
        (false, false) => "CVSS-B",
        (true, false) => "CVSS-BT",
        (false, true) => "CVSS-BE",
        (true, true) => "CVSS-BTE",
    }
    .to_string();

    // Effective metric resolution (FIRST CVSS v4.0 §8.2)
    let eff = |base_k: &str, mod_k: &str| -> &str {
        match seen.get(mod_k).copied().unwrap_or("X") {
            "X" => seen[base_k],
            other => other,
        }
    };
    let av = eff("AV", "MAV");
    let ac = eff("AC", "MAC");
    let at = eff("AT", "MAT");
    let pr = eff("PR", "MPR");
    let ui = eff("UI", "MUI");
    let vc = eff("VC", "MVC");
    let vi = eff("VI", "MVI");
    let va = eff("VA", "MVA");
    let sc = eff("SC", "MSC");
    let si = eff("SI", "MSI");
    let sa = eff("SA", "MSA");
    let e = match seen.get("E").copied().unwrap_or("X") {
        "X" => "A",
        other => other,
    };
    let cr = match seen.get("CR").copied().unwrap_or("X") {
        "X" => "H",
        other => other,
    };
    let ir = match seen.get("IR").copied().unwrap_or("X") {
        "X" => "H",
        other => other,
    };
    let ar = match seen.get("AR").copied().unwrap_or("X") {
        "X" => "H",
        other => other,
    };

    // If all effective impacts are None, score is 0.0.
    if vc == "N" && vi == "N" && va == "N" && sc == "N" && si == "N" && sa == "N" {
        let mv =
            compute_v40_macro_vector(av, ac, at, pr, ui, vc, vi, va, sc, si, sa, e, cr, ir, ar);
        let mv_str = format!("{}{}{}{}{}{}", mv[0], mv[1], mv[2], mv[3], mv[4], mv[5]);
        return Ok(CvssV40Scores {
            vector: vector.to_string(),
            nomenclature,
            macro_vector: mv_str,
            score: 0.0,
            severity: CvssSeverity::None,
        });
    }

    let mv = compute_v40_macro_vector(av, ac, at, pr, ui, vc, vi, va, sc, si, sa, e, cr, ir, ar);
    let mv_str = format!("{}{}{}{}{}{}", mv[0], mv[1], mv[2], mv[3], mv[4], mv[5]);
    let base_val = v40_lookup(mv);

    // Next-lower MacroVector scores (FIRST CVSS v4.0 §8.3)
    let next_eq1 = if mv[0] < 2 {
        Some(v40_lookup([mv[0] + 1, mv[1], mv[2], mv[3], mv[4], mv[5]]))
    } else {
        None
    };
    let next_eq2 = if mv[1] < 1 {
        Some(v40_lookup([mv[0], mv[1] + 1, mv[2], mv[3], mv[4], mv[5]]))
    } else {
        None
    };
    // EQ3 and EQ6 are coupled in the MacroVector lookup table.
    let next_eq3_eq6 = match (mv[2], mv[5]) {
        (1, 1) => Some(v40_lookup([mv[0], mv[1], 2, mv[3], mv[4], 1])),
        (0, 1) => Some(v40_lookup([mv[0], mv[1], 1, mv[3], mv[4], 1])),
        (1, 0) => Some(v40_lookup([mv[0], mv[1], 1, mv[3], mv[4], 1])),
        (0, 0) => {
            let left = v40_lookup([mv[0], mv[1], 0, mv[3], mv[4], 1]);
            let right = v40_lookup([mv[0], mv[1], 1, mv[3], mv[4], 0]);
            Some(left.max(right))
        }
        _ => None,
    };
    let next_eq4 = if mv[3] < 2 {
        Some(v40_lookup([mv[0], mv[1], mv[2], mv[3] + 1, mv[4], mv[5]]))
    } else {
        None
    };
    let next_eq5 = if mv[4] < 2 {
        Some(v40_lookup([mv[0], mv[1], mv[2], mv[3], mv[4] + 1, mv[5]]))
    } else {
        None
    };

    let step = 0.1_f64;
    let avail_eq1 = next_eq1.map(|low| base_val - low);
    let avail_eq2 = next_eq2.map(|low| base_val - low);
    let avail_eq3_eq6 = next_eq3_eq6.map(|low| base_val - low);
    let avail_eq4 = next_eq4.map(|low| base_val - low);
    let avail_eq5 = next_eq5.map(|low| base_val - low);

    let max_vecs = v40_max_vectors(mv[0], mv[1], mv[2], mv[3], mv[4], mv[5]);
    let mut sev_eq1 = 0.0;
    let mut sev_eq2 = 0.0;
    let mut sev_eq3_eq6 = 0.0;
    let mut sev_eq4 = 0.0;
    let mut sev_eq5 = 0.0;

    for max_v in &max_vecs {
        let d_av = v40_av_level(av) - v40_av_level(max_v[0]);
        let d_pr = v40_pr_level(pr) - v40_pr_level(max_v[1]);
        let d_ui = v40_ui_level(ui) - v40_ui_level(max_v[2]);
        let d_ac = v40_ac_level(ac) - v40_ac_level(max_v[3]);
        let d_at = v40_at_level(at) - v40_at_level(max_v[4]);
        let d_vc = v40_cia_level(vc) - v40_cia_level(max_v[5]);
        let d_vi = v40_cia_level(vi) - v40_cia_level(max_v[6]);
        let d_va = v40_cia_level(va) - v40_cia_level(max_v[7]);
        let d_sc = v40_cia_level(sc) - v40_cia_level(max_v[8]);
        let d_si = v40_si_sa_level(si) - v40_si_sa_level(max_v[9]);
        let d_sa = v40_si_sa_level(sa) - v40_si_sa_level(max_v[10]);
        let d_cr = v40_req_level(cr) - v40_req_level(max_v[11]);
        let d_ir = v40_req_level(ir) - v40_req_level(max_v[12]);
        let d_ar = v40_req_level(ar) - v40_req_level(max_v[13]);

        if [
            d_av, d_pr, d_ui, d_ac, d_at, d_vc, d_vi, d_va, d_sc, d_si, d_sa, d_cr, d_ir, d_ar,
        ]
        .iter()
        .all(|&d| d >= 0.0)
        {
            sev_eq1 = d_av + d_pr + d_ui;
            sev_eq2 = d_ac + d_at;
            sev_eq3_eq6 = d_vc + d_vi + d_va + d_cr + d_ir + d_ar;
            sev_eq4 = d_sc + d_si + d_sa;
            sev_eq5 = 0.0;
            break;
        }
    }

    let max_eq1 = match mv[0] {
        0 => 1.0 * step,
        1 => 4.0 * step,
        _ => 5.0 * step,
    };
    let max_eq2 = match mv[1] {
        0 => 1.0 * step,
        _ => 2.0 * step,
    };
    let max_eq3_eq6 = match (mv[2], mv[5]) {
        (0, 0) => 7.0 * step,
        (0, 1) => 6.0 * step,
        (1, 0) => 8.0 * step,
        (1, 1) => 8.0 * step,
        _ => 10.0 * step,
    };
    let max_eq4 = match mv[3] {
        0 => 6.0 * step,
        1 => 5.0 * step,
        _ => 4.0 * step,
    };
    let max_eq5 = step;

    let mut norm_terms: Vec<f64> = Vec::new();
    if let Some(av_d) = avail_eq1.filter(|&d| d >= 0.0) {
        norm_terms.push(av_d * (sev_eq1 / max_eq1));
    }
    if let Some(av_d) = avail_eq2.filter(|&d| d >= 0.0) {
        norm_terms.push(av_d * (sev_eq2 / max_eq2));
    }
    if let Some(av_d) = avail_eq3_eq6.filter(|&d| d >= 0.0) {
        norm_terms.push(av_d * (sev_eq3_eq6 / max_eq3_eq6));
    }
    if let Some(av_d) = avail_eq4.filter(|&d| d >= 0.0) {
        norm_terms.push(av_d * (sev_eq4 / max_eq4));
    }
    if let Some(av_d) = avail_eq5.filter(|&d| d >= 0.0) {
        norm_terms.push(av_d * (sev_eq5 / max_eq5));
    }

    let mean_distance = if norm_terms.is_empty() {
        0.0
    } else {
        norm_terms.iter().sum::<f64>() / (norm_terms.len() as f64)
    };
    let score = cvss40_round(base_val - mean_distance);
    let severity = CvssSeverity::from_score(score);

    Ok(CvssV40Scores {
        vector: vector.to_string(),
        nomenclature,
        macro_vector: mv_str,
        score,
        severity,
    })
}

#[allow(clippy::too_many_arguments)]
fn compute_v40_macro_vector(
    av: &str,
    ac: &str,
    at: &str,
    pr: &str,
    ui: &str,
    vc: &str,
    vi: &str,
    va: &str,
    sc: &str,
    si: &str,
    sa: &str,
    e: &str,
    cr: &str,
    ir: &str,
    ar: &str,
) -> [u8; 6] {
    let eq1 = if av == "N" && pr == "N" && ui == "N" {
        0
    } else if (av == "N" || pr == "N" || ui == "N")
        && !(av == "N" && pr == "N" && ui == "N")
        && av != "P"
    {
        1
    } else {
        2
    };
    let eq2 = if ac == "L" && at == "N" { 0 } else { 1 };
    let eq3 = if vc == "H" && vi == "H" {
        0
    } else if !(vc == "H" && vi == "H") && (vc == "H" || vi == "H" || va == "H") {
        1
    } else {
        2
    };
    let eq4 = if si == "S" || sa == "S" {
        0
    } else if !(si == "S" || sa == "S") && (sc == "H" || si == "H" || sa == "H") {
        1
    } else {
        2
    };
    let eq5 = match e {
        "A" => 0,
        "P" => 1,
        _ => 2,
    };
    let eq6 = if (cr == "H" && vc == "H") || (ir == "H" && vi == "H") || (ar == "H" && va == "H") {
        0
    } else {
        1
    };
    [eq1, eq2, eq3, eq4, eq5, eq6]
}

fn validate_v40_metric(k: &str, v: &str) -> Result<()> {
    let ok = match k {
        "AV" => matches!(v, "N" | "A" | "L" | "P"),
        "AC" => matches!(v, "L" | "H"),
        "AT" => matches!(v, "N" | "P"),
        "PR" => matches!(v, "N" | "L" | "H"),
        "UI" => matches!(v, "N" | "P" | "A"),
        "VC" | "VI" | "VA" | "SC" | "SI" | "SA" => matches!(v, "H" | "L" | "N"),
        "E" => matches!(v, "X" | "A" | "P" | "U"),
        "CR" | "IR" | "AR" => matches!(v, "X" | "H" | "M" | "L"),
        "MAV" => matches!(v, "X" | "N" | "A" | "L" | "P"),
        "MAC" => matches!(v, "X" | "L" | "H"),
        "MAT" => matches!(v, "X" | "N" | "P"),
        "MPR" => matches!(v, "X" | "N" | "L" | "H"),
        "MUI" => matches!(v, "X" | "N" | "P" | "A"),
        "MVC" | "MVI" | "MVA" | "MSC" => matches!(v, "X" | "H" | "L" | "N"),
        "MSI" | "MSA" => matches!(v, "X" | "S" | "H" | "L" | "N"),
        "S" => matches!(v, "X" | "N" | "P"),
        "AU" => matches!(v, "X" | "N" | "Y"),
        "R" => matches!(v, "X" | "A" | "U" | "I"),
        "V" => matches!(v, "X" | "D" | "C"),
        "RE" => matches!(v, "X" | "L" | "M" | "H"),
        "U" => matches!(v, "X" | "Clear" | "Green" | "Amber" | "Red"),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ScoreError::InvalidCvssVector(format!(
            "invalid CVSS v4.0 metric or value `{k}:{v}`"
        )))
    }
}

fn v40_av_level(v: &str) -> f64 {
    match v {
        "N" => 0.0,
        "A" => 0.1,
        "L" => 0.2,
        "P" => 0.3,
        _ => 0.0,
    }
}
fn v40_pr_level(v: &str) -> f64 {
    match v {
        "N" => 0.0,
        "L" => 0.1,
        "H" => 0.2,
        _ => 0.0,
    }
}
fn v40_ui_level(v: &str) -> f64 {
    match v {
        "N" => 0.0,
        "P" => 0.1,
        "A" => 0.2,
        _ => 0.0,
    }
}
fn v40_ac_level(v: &str) -> f64 {
    match v {
        "L" => 0.0,
        "H" => 0.1,
        _ => 0.0,
    }
}
fn v40_at_level(v: &str) -> f64 {
    match v {
        "N" => 0.0,
        "P" => 0.1,
        _ => 0.0,
    }
}
fn v40_cia_level(v: &str) -> f64 {
    match v {
        "H" => 0.0,
        "L" => 0.1,
        "N" => 0.2,
        _ => 0.0,
    }
}
fn v40_si_sa_level(v: &str) -> f64 {
    match v {
        "S" => 0.0,
        "H" => 0.1,
        "L" => 0.2,
        "N" => 0.3,
        _ => 0.0,
    }
}
fn v40_req_level(v: &str) -> f64 {
    match v {
        "H" => 0.0,
        "M" => 0.1,
        "L" => 0.2,
        _ => 0.0,
    }
}

/// Highest-severity constituent vectors per MacroVector equivalence class (FIRST CVSS v4.0 Table).
fn v40_max_vectors(
    eq1: u8,
    eq2: u8,
    eq3: u8,
    eq4: u8,
    _eq5: u8,
    eq6: u8,
) -> Vec<[&'static str; 14]> {
    let eq1_max: &[[&str; 3]] = match eq1 {
        0 => &[["N", "N", "N"]],
        1 => &[["A", "N", "N"], ["N", "L", "N"], ["N", "N", "P"]],
        _ => &[["P", "N", "N"], ["A", "L", "P"]],
    };
    let eq2_max: &[[&str; 2]] = match eq2 {
        0 => &[["L", "N"]],
        _ => &[["H", "N"], ["L", "P"]],
    };
    let eq3_eq6_max: &[[&str; 6]] = match (eq3, eq6) {
        (0, 0) => &[["H", "H", "H", "H", "H", "H"]],
        (0, 1) => &[
            ["H", "H", "L", "M", "M", "H"],
            ["H", "H", "H", "M", "M", "M"],
        ],
        (1, 0) => &[
            ["L", "H", "H", "H", "H", "H"],
            ["H", "L", "H", "H", "H", "H"],
        ],
        (1, 1) => &[
            ["L", "H", "L", "H", "M", "H"],
            ["L", "H", "H", "H", "M", "M"],
            ["H", "L", "H", "M", "H", "M"],
            ["H", "L", "L", "M", "H", "H"],
            ["L", "L", "H", "H", "H", "M"],
        ],
        _ => &[["L", "L", "L", "H", "H", "H"]],
    };
    let eq4_max: &[[&str; 3]] = match eq4 {
        0 => &[["H", "S", "S"]],
        1 => &[["H", "H", "H"]],
        _ => &[["L", "L", "L"]],
    };

    let mut out = Vec::new();
    for a in eq1_max {
        for b in eq2_max {
            for c in eq3_eq6_max {
                for d in eq4_max {
                    out.push([
                        a[0], a[1], a[2], b[0], b[1], c[0], c[1], c[2], d[0], d[1], d[2], c[3],
                        c[4], c[5],
                    ]);
                }
            }
        }
    }
    out
}

/// Official FIRST CVSS v4.0 MacroVector lookup table (`EQ1..EQ6`).
fn v40_lookup(mv: [u8; 6]) -> f64 {
    match mv {
        [0, 0, 0, 0, 0, 0] => 10.0,
        [0, 0, 0, 0, 0, 1] => 9.9,
        [0, 0, 0, 0, 1, 0] => 9.8,
        [0, 0, 0, 0, 1, 1] => 9.5,
        [0, 0, 0, 0, 2, 0] => 9.5,
        [0, 0, 0, 0, 2, 1] => 9.2,
        [0, 0, 0, 1, 0, 0] => 10.0,
        [0, 0, 0, 1, 0, 1] => 9.7,
        [0, 0, 0, 1, 1, 0] => 9.5,
        [0, 0, 0, 1, 1, 1] => 9.2,
        [0, 0, 0, 1, 2, 0] => 9.2,
        [0, 0, 0, 1, 2, 1] => 8.5,
        [0, 0, 0, 2, 0, 0] => 9.3,
        [0, 0, 0, 2, 0, 1] => 8.7,
        [0, 0, 0, 2, 1, 0] => 8.9,
        [0, 0, 0, 2, 1, 1] => 8.1,
        [0, 0, 0, 2, 2, 0] => 8.1,
        [0, 0, 0, 2, 2, 1] => 6.8,
        [0, 0, 1, 0, 0, 0] => 9.8,
        [0, 0, 1, 0, 0, 1] => 9.5,
        [0, 0, 1, 0, 1, 0] => 9.5,
        [0, 0, 1, 0, 1, 1] => 9.2,
        [0, 0, 1, 0, 2, 0] => 9.0,
        [0, 0, 1, 0, 2, 1] => 8.4,
        [0, 0, 1, 1, 0, 0] => 9.3,
        [0, 0, 1, 1, 0, 1] => 9.2,
        [0, 0, 1, 1, 1, 0] => 8.9,
        [0, 0, 1, 1, 1, 1] => 8.1,
        [0, 0, 1, 1, 2, 0] => 8.1,
        [0, 0, 1, 1, 2, 1] => 6.5,
        [0, 0, 1, 2, 0, 0] => 8.8,
        [0, 0, 1, 2, 0, 1] => 8.0,
        [0, 0, 1, 2, 1, 0] => 7.8,
        [0, 0, 1, 2, 1, 1] => 7.0,
        [0, 0, 1, 2, 2, 0] => 6.9,
        [0, 0, 1, 2, 2, 1] => 4.8,
        [0, 0, 2, 0, 0, 1] => 9.2,
        [0, 0, 2, 0, 1, 1] => 8.2,
        [0, 0, 2, 0, 2, 1] => 7.2,
        [0, 0, 2, 1, 0, 1] => 7.9,
        [0, 0, 2, 1, 1, 1] => 6.9,
        [0, 0, 2, 1, 2, 1] => 5.0,
        [0, 0, 2, 2, 0, 1] => 6.9,
        [0, 0, 2, 2, 1, 1] => 5.5,
        [0, 0, 2, 2, 2, 1] => 2.7,
        [0, 1, 0, 0, 0, 0] => 9.9,
        [0, 1, 0, 0, 0, 1] => 9.7,
        [0, 1, 0, 0, 1, 0] => 9.5,
        [0, 1, 0, 0, 1, 1] => 9.2,
        [0, 1, 0, 0, 2, 0] => 9.2,
        [0, 1, 0, 0, 2, 1] => 8.5,
        [0, 1, 0, 1, 0, 0] => 9.5,
        [0, 1, 0, 1, 0, 1] => 9.1,
        [0, 1, 0, 1, 1, 0] => 9.0,
        [0, 1, 0, 1, 1, 1] => 8.3,
        [0, 1, 0, 1, 2, 0] => 8.4,
        [0, 1, 0, 1, 2, 1] => 7.1,
        [0, 1, 0, 2, 0, 0] => 9.2,
        [0, 1, 0, 2, 0, 1] => 8.1,
        [0, 1, 0, 2, 1, 0] => 8.2,
        [0, 1, 0, 2, 1, 1] => 7.1,
        [0, 1, 0, 2, 2, 0] => 7.2,
        [0, 1, 0, 2, 2, 1] => 5.3,
        [0, 1, 1, 0, 0, 0] => 9.5,
        [0, 1, 1, 0, 0, 1] => 9.3,
        [0, 1, 1, 0, 1, 0] => 9.2,
        [0, 1, 1, 0, 1, 1] => 8.5,
        [0, 1, 1, 0, 2, 0] => 8.5,
        [0, 1, 1, 0, 2, 1] => 7.3,
        [0, 1, 1, 1, 0, 0] => 9.2,
        [0, 1, 1, 1, 0, 1] => 8.2,
        [0, 1, 1, 1, 1, 0] => 8.0,
        [0, 1, 1, 1, 1, 1] => 7.2,
        [0, 1, 1, 1, 2, 0] => 7.0,
        [0, 1, 1, 1, 2, 1] => 5.9,
        [0, 1, 1, 2, 0, 0] => 8.4,
        [0, 1, 1, 2, 0, 1] => 7.0,
        [0, 1, 1, 2, 1, 0] => 7.1,
        [0, 1, 1, 2, 1, 1] => 5.2,
        [0, 1, 1, 2, 2, 0] => 5.0,
        [0, 1, 1, 2, 2, 1] => 3.0,
        [0, 1, 2, 0, 0, 1] => 8.6,
        [0, 1, 2, 0, 1, 1] => 7.5,
        [0, 1, 2, 0, 2, 1] => 5.2,
        [0, 1, 2, 1, 0, 1] => 7.1,
        [0, 1, 2, 1, 1, 1] => 5.2,
        [0, 1, 2, 1, 2, 1] => 2.9,
        [0, 1, 2, 2, 0, 1] => 6.3,
        [0, 1, 2, 2, 1, 1] => 2.9,
        [0, 1, 2, 2, 2, 1] => 1.7,
        [1, 0, 0, 0, 0, 0] => 9.8,
        [1, 0, 0, 0, 0, 1] => 9.5,
        [1, 0, 0, 0, 1, 0] => 9.4,
        [1, 0, 0, 0, 1, 1] => 8.7,
        [1, 0, 0, 0, 2, 0] => 9.1,
        [1, 0, 0, 0, 2, 1] => 8.1,
        [1, 0, 0, 1, 0, 0] => 9.4,
        [1, 0, 0, 1, 0, 1] => 8.9,
        [1, 0, 0, 1, 1, 0] => 8.6,
        [1, 0, 0, 1, 1, 1] => 7.4,
        [1, 0, 0, 1, 2, 0] => 7.7,
        [1, 0, 0, 1, 2, 1] => 6.4,
        [1, 0, 0, 2, 0, 0] => 8.7,
        [1, 0, 0, 2, 0, 1] => 7.5,
        [1, 0, 0, 2, 1, 0] => 7.4,
        [1, 0, 0, 2, 1, 1] => 6.3,
        [1, 0, 0, 2, 2, 0] => 6.3,
        [1, 0, 0, 2, 2, 1] => 4.9,
        [1, 0, 1, 0, 0, 0] => 9.4,
        [1, 0, 1, 0, 0, 1] => 8.9,
        [1, 0, 1, 0, 1, 0] => 8.8,
        [1, 0, 1, 0, 1, 1] => 7.7,
        [1, 0, 1, 0, 2, 0] => 7.6,
        [1, 0, 1, 0, 2, 1] => 6.7,
        [1, 0, 1, 1, 0, 0] => 8.6,
        [1, 0, 1, 1, 0, 1] => 7.6,
        [1, 0, 1, 1, 1, 0] => 7.4,
        [1, 0, 1, 1, 1, 1] => 5.8,
        [1, 0, 1, 1, 2, 0] => 5.9,
        [1, 0, 1, 1, 2, 1] => 5.0,
        [1, 0, 1, 2, 0, 0] => 7.2,
        [1, 0, 1, 2, 0, 1] => 5.7,
        [1, 0, 1, 2, 1, 0] => 5.7,
        [1, 0, 1, 2, 1, 1] => 4.0,
        [1, 0, 1, 2, 2, 0] => 4.0,
        [1, 0, 1, 2, 2, 1] => 2.2,
        [1, 0, 2, 0, 0, 1] => 7.9,
        [1, 0, 2, 0, 1, 1] => 6.9,
        [1, 0, 2, 0, 2, 1] => 4.8,
        [1, 0, 2, 1, 0, 1] => 6.5,
        [1, 0, 2, 1, 1, 1] => 4.9,
        [1, 0, 2, 1, 2, 1] => 2.6,
        [1, 0, 2, 2, 0, 1] => 5.3,
        [1, 0, 2, 2, 1, 1] => 2.1,
        [1, 0, 2, 2, 2, 1] => 1.3,
        [1, 1, 0, 0, 0, 0] => 9.5,
        [1, 1, 0, 0, 0, 1] => 9.0,
        [1, 1, 0, 0, 1, 0] => 8.8,
        [1, 1, 0, 0, 1, 1] => 7.6,
        [1, 1, 0, 0, 2, 0] => 7.6,
        [1, 1, 0, 0, 2, 1] => 7.0,
        [1, 1, 0, 1, 0, 0] => 9.0,
        [1, 1, 0, 1, 0, 1] => 7.7,
        [1, 1, 0, 1, 1, 0] => 7.5,
        [1, 1, 0, 1, 1, 1] => 6.2,
        [1, 1, 0, 1, 2, 0] => 6.1,
        [1, 1, 0, 1, 2, 1] => 5.3,
        [1, 1, 0, 2, 0, 0] => 7.7,
        [1, 1, 0, 2, 0, 1] => 6.6,
        [1, 1, 0, 2, 1, 0] => 6.8,
        [1, 1, 0, 2, 1, 1] => 5.9,
        [1, 1, 0, 2, 2, 0] => 5.2,
        [1, 1, 0, 2, 2, 1] => 3.0,
        [1, 1, 1, 0, 0, 0] => 8.9,
        [1, 1, 1, 0, 0, 1] => 7.8,
        [1, 1, 1, 0, 1, 0] => 7.6,
        [1, 1, 1, 0, 1, 1] => 6.7,
        [1, 1, 1, 0, 2, 0] => 6.2,
        [1, 1, 1, 0, 2, 1] => 5.8,
        [1, 1, 1, 1, 0, 0] => 7.4,
        [1, 1, 1, 1, 0, 1] => 5.9,
        [1, 1, 1, 1, 1, 0] => 5.7,
        [1, 1, 1, 1, 1, 1] => 5.7,
        [1, 1, 1, 1, 2, 0] => 4.7,
        [1, 1, 1, 1, 2, 1] => 2.3,
        [1, 1, 1, 2, 0, 0] => 6.1,
        [1, 1, 1, 2, 0, 1] => 5.2,
        [1, 1, 1, 2, 1, 0] => 5.7,
        [1, 1, 1, 2, 1, 1] => 2.9,
        [1, 1, 1, 2, 2, 0] => 2.4,
        [1, 1, 1, 2, 2, 1] => 1.6,
        [1, 1, 2, 0, 0, 1] => 7.1,
        [1, 1, 2, 0, 1, 1] => 5.9,
        [1, 1, 2, 0, 2, 1] => 3.0,
        [1, 1, 2, 1, 0, 1] => 5.8,
        [1, 1, 2, 1, 1, 1] => 1.8,
        [1, 1, 2, 1, 2, 1] => 1.5,
        [1, 1, 2, 2, 0, 1] => 2.3,
        [1, 1, 2, 2, 1, 1] => 1.6,
        [1, 1, 2, 2, 2, 1] => 0.6,
        [2, 0, 0, 0, 0, 0] => 9.3,
        [2, 0, 0, 0, 0, 1] => 8.7,
        [2, 0, 0, 0, 1, 0] => 8.6,
        [2, 0, 0, 0, 1, 1] => 7.2,
        [2, 0, 0, 0, 2, 0] => 7.5,
        [2, 0, 0, 0, 2, 1] => 5.8,
        [2, 0, 0, 1, 0, 0] => 8.6,
        [2, 0, 0, 1, 0, 1] => 7.4,
        [2, 0, 0, 1, 1, 0] => 7.4,
        [2, 0, 0, 1, 1, 1] => 6.1,
        [2, 0, 0, 1, 2, 0] => 5.6,
        [2, 0, 0, 1, 2, 1] => 3.4,
        [2, 0, 0, 2, 0, 0] => 7.2,
        [2, 0, 0, 2, 0, 1] => 5.4,
        [2, 0, 0, 2, 1, 0] => 5.2,
        [2, 0, 0, 2, 1, 1] => 4.0,
        [2, 0, 0, 2, 2, 0] => 4.0,
        [2, 0, 0, 2, 2, 1] => 2.2,
        [2, 0, 1, 0, 0, 0] => 8.5,
        [2, 0, 1, 0, 0, 1] => 7.5,
        [2, 0, 1, 0, 1, 0] => 7.4,
        [2, 0, 1, 0, 1, 1] => 5.5,
        [2, 0, 1, 0, 2, 0] => 6.2,
        [2, 0, 1, 0, 2, 1] => 5.1,
        [2, 0, 1, 1, 0, 0] => 7.2,
        [2, 0, 1, 1, 0, 1] => 5.7,
        [2, 0, 1, 1, 1, 0] => 5.5,
        [2, 0, 1, 1, 1, 1] => 4.1,
        [2, 0, 1, 1, 2, 0] => 4.6,
        [2, 0, 1, 1, 2, 1] => 1.9,
        [2, 0, 1, 2, 0, 0] => 5.3,
        [2, 0, 1, 2, 0, 1] => 3.6,
        [2, 0, 1, 2, 1, 0] => 3.4,
        [2, 0, 1, 2, 1, 1] => 1.9,
        [2, 0, 1, 2, 2, 0] => 1.9,
        [2, 0, 1, 2, 2, 1] => 0.8,
        [2, 0, 2, 0, 0, 1] => 6.4,
        [2, 0, 2, 0, 1, 1] => 5.1,
        [2, 0, 2, 0, 2, 1] => 2.0,
        [2, 0, 2, 1, 0, 1] => 4.7,
        [2, 0, 2, 1, 1, 1] => 2.0,
        [2, 0, 2, 1, 2, 1] => 1.1,
        [2, 0, 2, 2, 0, 1] => 2.4,
        [2, 0, 2, 2, 1, 1] => 0.9,
        [2, 0, 2, 2, 2, 1] => 0.4,
        [2, 1, 0, 0, 0, 0] => 8.8,
        [2, 1, 0, 0, 0, 1] => 7.5,
        [2, 1, 0, 0, 1, 0] => 7.3,
        [2, 1, 0, 0, 1, 1] => 5.3,
        [2, 1, 0, 0, 2, 0] => 6.0,
        [2, 1, 0, 0, 2, 1] => 5.0,
        [2, 1, 0, 1, 0, 0] => 7.3,
        [2, 1, 0, 1, 0, 1] => 5.5,
        [2, 1, 0, 1, 1, 0] => 5.9,
        [2, 1, 0, 1, 1, 1] => 4.0,
        [2, 1, 0, 1, 2, 0] => 4.1,
        [2, 1, 0, 1, 2, 1] => 2.0,
        [2, 1, 0, 2, 0, 0] => 6.4,
        [2, 1, 0, 2, 0, 1] => 5.1,
        [2, 1, 0, 2, 1, 0] => 4.1,
        [2, 1, 0, 2, 1, 1] => 2.0,
        [2, 1, 0, 2, 2, 0] => 2.0,
        [2, 1, 0, 2, 2, 1] => 0.9,
        [2, 1, 1, 0, 0, 0] => 7.5,
        [2, 1, 1, 0, 0, 1] => 6.3,
        [2, 1, 1, 0, 1, 0] => 5.8,
        [2, 1, 1, 0, 1, 1] => 4.5,
        [2, 1, 1, 0, 2, 0] => 4.0,
        [2, 1, 1, 0, 2, 1] => 2.1,
        [2, 1, 1, 1, 0, 0] => 6.1,
        [2, 1, 1, 1, 0, 1] => 5.1,
        [2, 1, 1, 1, 1, 0] => 2.4,
        [2, 1, 1, 1, 1, 1] => 2.0,
        [2, 1, 1, 1, 2, 0] => 2.0,
        [2, 1, 1, 1, 2, 1] => 0.9,
        [2, 1, 1, 2, 0, 0] => 5.3,
        [2, 1, 1, 2, 0, 1] => 2.4,
        [2, 1, 1, 2, 1, 0] => 2.0,
        [2, 1, 1, 2, 1, 1] => 0.8,
        [2, 1, 1, 2, 2, 0] => 0.8,
        [2, 1, 1, 2, 2, 1] => 0.2,
        [2, 1, 2, 0, 0, 1] => 5.4,
        [2, 1, 2, 0, 1, 1] => 1.9,
        [2, 1, 2, 0, 2, 1] => 0.8,
        [2, 1, 2, 1, 0, 1] => 1.9,
        [2, 1, 2, 1, 1, 1] => 0.8,
        [2, 1, 2, 1, 2, 1] => 0.2,
        [2, 1, 2, 2, 0, 1] => 0.7,
        [2, 1, 2, 2, 1, 1] => 0.2,
        [2, 1, 2, 2, 2, 1] => 0.1,
        _ => 0.0,
    }
}

/// Explicit presence or absence for external corpus context (EPSS, CISA KEV).
/// Absent data is stated, never guessed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreatIntelField<T> {
    Present(T),
    Absent { reason: String },
}

impl<T> ThreatIntelField<T> {
    pub fn absent(reason: impl Into<String>) -> Self {
        Self::Absent {
            reason: reason.into(),
        }
    }

    pub fn as_present(&self) -> Option<&T> {
        match self {
            Self::Present(v) => Some(v),
            Self::Absent { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpssData {
    pub cve_id: String,
    /// Probability of exploitation in the next 30 days (`0.0..=1.0`).
    pub score: f64,
    /// Percentile rank (`0.0..=1.0`).
    pub percentile: f64,
    pub source_corpus: String,
    pub pin_sha: String,
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_citation: Option<PinnedCitation>,
}

impl PartialEq for EpssData {
    fn eq(&self, other: &Self) -> bool {
        self.cve_id == other.cve_id
            && self.score == other.score
            && self.percentile == other.percentile
            && self.source_corpus == other.source_corpus
            && self.pin_sha == other.pin_sha
            && self.fetched_at == other.fetched_at
    }
}

impl EpssData {
    pub fn validate(&self) -> Result<()> {
        validate_cve_id(&self.cve_id)?;
        if !self.score.is_finite() || !(0.0..=1.0).contains(&self.score) {
            return Err(ScoreError::InvalidThreatIntel(
                "EPSS score must be finite in 0.0..=1.0",
            ));
        }
        if !self.percentile.is_finite() || !(0.0..=1.0).contains(&self.percentile) {
            return Err(ScoreError::InvalidThreatIntel(
                "EPSS percentile must be finite in 0.0..=1.0",
            ));
        }
        validate_provenance(
            &self.source_corpus,
            &self.pin_sha,
            &self.fetched_at,
            self.pinned_citation.as_ref(),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KevData {
    pub cve_id: String,
    pub in_kev: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_added: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_ransomware_campaign_use: Option<String>,
    pub source_corpus: String,
    pub pin_sha: String,
    pub fetched_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_citation: Option<PinnedCitation>,
}

impl PartialEq for KevData {
    fn eq(&self, other: &Self) -> bool {
        self.cve_id == other.cve_id
            && self.in_kev == other.in_kev
            && self.date_added == other.date_added
            && self.due_date == other.due_date
            && self.known_ransomware_campaign_use == other.known_ransomware_campaign_use
            && self.source_corpus == other.source_corpus
            && self.pin_sha == other.pin_sha
            && self.fetched_at == other.fetched_at
    }
}

impl KevData {
    pub fn validate(&self) -> Result<()> {
        validate_cve_id(&self.cve_id)?;
        if self.in_kev {
            let added = self
                .date_added
                .as_deref()
                .ok_or(ScoreError::InvalidThreatIntel(
                    "KEV entry with in_kev=true must carry date_added",
                ))?;
            if !valid_date_or_rfc3339(added) {
                return Err(ScoreError::InvalidThreatIntel(
                    "KEV date_added must be YYYY-MM-DD or RFC 3339",
                ));
            }
        }
        if let Some(due) = &self.due_date {
            if !valid_date_or_rfc3339(due) {
                return Err(ScoreError::InvalidThreatIntel(
                    "KEV due_date must be YYYY-MM-DD or RFC 3339",
                ));
            }
        }
        validate_provenance(
            &self.source_corpus,
            &self.pin_sha,
            &self.fetched_at,
            self.pinned_citation.as_ref(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetCriticality {
    Low,
    Medium,
    High,
    MissionCritical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClassification {
    Public,
    Internal,
    Confidential,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetExposure {
    Isolated,
    InternalNetwork,
    PartnerFacing,
    InternetFacing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetContext {
    pub asset_id: String,
    pub criticality: AssetCriticality,
    pub data_classification: DataClassification,
    pub exposure: AssetExposure,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compensating_controls: Vec<String>,
    /// Optional live host/IP target; when set, requires a declared `ScopeGuard`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_target: Option<String>,
}

impl AssetContext {
    pub fn validate(&self) -> Result<()> {
        if !valid_slug(&self.asset_id, 128) {
            return Err(ScoreError::InvalidAssetContext(
                "asset_id must be 1..=128 ASCII [A-Za-z0-9._:-]",
            ));
        }
        if self.compensating_controls.len() > 32 {
            return Err(ScoreError::InvalidAssetContext(
                "too many compensating_controls (max 32)",
            ));
        }
        for c in &self.compensating_controls {
            if !valid_line(c) {
                return Err(ScoreError::InvalidAssetContext(
                    "compensating_controls entries must be non-empty single-line text",
                ));
            }
        }
        if let Some(t) = &self.endpoint_target {
            if !valid_line(t) || t.contains("://") || t.contains('/') {
                return Err(ScoreError::InvalidAssetContext(
                    "endpoint_target must be a bare host or IP without scheme or path",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PriorityTier {
    P1,
    P2,
    P3,
    P4,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessImpact {
    pub priority_tier: PriorityTier,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CyberScoreReport {
    pub report_version: String,
    pub finding_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cve_id: Option<String>,
    pub cvss: CvssEvaluation,
    pub epss: ThreatIntelField<EpssData>,
    pub kev: ThreatIntelField<KevData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_context: Option<AssetContext>,
    pub business_impact: BusinessImpact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<WorkOrder>,
    pub report_sha256: String,
}

#[derive(Serialize)]
struct Preimage<'a> {
    report_version: &'a str,
    finding_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cve_id: &'a Option<String>,
    cvss: &'a CvssEvaluation,
    epss: &'a ThreatIntelField<EpssData>,
    kev: &'a ThreatIntelField<KevData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    asset_context: &'a Option<AssetContext>,
    business_impact: &'a BusinessImpact,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: &'a Option<WorkOrder>,
}

impl CyberScoreReport {
    /// Build a deterministic `cyber.score` report without an endpoint scope.
    /// Refuses an `AssetContext` that names `endpoint_target` (`ScopeRequired`).
    pub fn evaluate(
        finding_id: impl Into<String>,
        cve_id: Option<String>,
        cvss_vector: &str,
        epss: ThreatIntelField<EpssData>,
        kev: ThreatIntelField<KevData>,
        asset_context: Option<AssetContext>,
    ) -> Result<Self> {
        if asset_context
            .as_ref()
            .and_then(|a| a.endpoint_target.as_ref())
            .is_some()
        {
            return Err(ScoreError::ScopeRequired);
        }
        Self::build_inner(
            finding_id.into(),
            cve_id,
            cvss_vector,
            epss,
            kev,
            asset_context,
            None,
        )
    }

    /// Build a deterministic `cyber.score` report authorized against a `ScopeGuard`.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_with_scope(
        finding_id: impl Into<String>,
        cve_id: Option<String>,
        cvss_vector: &str,
        epss: ThreatIntelField<EpssData>,
        kev: ThreatIntelField<KevData>,
        asset_context: Option<AssetContext>,
        guard: &ScopeGuard,
        origin: InputOrigin,
        now: DateTime<Utc>,
    ) -> Result<Self> {
        if origin != InputOrigin::AuthorizedControlPlane {
            return Err(ScoreError::Scope(ScopeError::UntrustedInput));
        }
        let wo = guard.scope();
        if now < wo.not_before {
            return Err(ScoreError::Scope(ScopeError::NotYetValid));
        }
        if now > wo.expires_at {
            return Err(ScoreError::Scope(ScopeError::Expired));
        }
        if let Some(target) = asset_context
            .as_ref()
            .and_then(|a| a.endpoint_target.as_deref())
        {
            let _ = guard.authorize(target, origin, now)?;
        }
        Self::build_inner(
            finding_id.into(),
            cve_id,
            cvss_vector,
            epss,
            kev,
            asset_context,
            Some(wo.clone()),
        )
    }

    fn build_inner(
        finding_id: String,
        cve_id: Option<String>,
        cvss_vector: &str,
        epss: ThreatIntelField<EpssData>,
        kev: ThreatIntelField<KevData>,
        asset_context: Option<AssetContext>,
        scope: Option<WorkOrder>,
    ) -> Result<Self> {
        if !valid_slug(&finding_id, 128) {
            return Err(ScoreError::InvalidThreatIntel(
                "finding_id must be 1..=128 ASCII [A-Za-z0-9._:-]",
            ));
        }
        if let Some(cve) = &cve_id {
            validate_cve_id(cve)?;
        }
        let cvss = CvssEvaluation::parse(cvss_vector)?;
        match &epss {
            ThreatIntelField::Present(d) => {
                d.validate()?;
                if let Some(cve) = &cve_id {
                    if &d.cve_id != cve {
                        return Err(ScoreError::InvalidThreatIntel(
                            "EPSS cve_id does not match report cve_id",
                        ));
                    }
                }
            }
            ThreatIntelField::Absent { reason } => {
                if !valid_line(reason) {
                    return Err(ScoreError::InvalidThreatIntel(
                        "absent EPSS reason must be non-empty single-line text",
                    ));
                }
            }
        }
        match &kev {
            ThreatIntelField::Present(d) => {
                d.validate()?;
                if let Some(cve) = &cve_id {
                    if &d.cve_id != cve {
                        return Err(ScoreError::InvalidThreatIntel(
                            "KEV cve_id does not match report cve_id",
                        ));
                    }
                }
            }
            ThreatIntelField::Absent { reason } => {
                if !valid_line(reason) {
                    return Err(ScoreError::InvalidThreatIntel(
                        "absent KEV reason must be non-empty single-line text",
                    ));
                }
            }
        }
        if let Some(a) = &asset_context {
            a.validate()?;
        }

        let business_impact = derive_business_impact(&cvss, &epss, &kev, asset_context.as_ref());
        let mut report = Self {
            report_version: SCORE_REPORT_VERSION.to_string(),
            finding_id,
            cve_id,
            cvss,
            epss,
            kev,
            asset_context,
            business_impact,
            scope,
            report_sha256: String::new(),
        };
        report.report_sha256 = report.compute_digest()?;
        Ok(report)
    }

    fn compute_digest(&self) -> Result<String> {
        let pre = Preimage {
            report_version: &self.report_version,
            finding_id: &self.finding_id,
            cve_id: &self.cve_id,
            cvss: &self.cvss,
            epss: &self.epss,
            kev: &self.kev,
            asset_context: &self.asset_context,
            business_impact: &self.business_impact,
            scope: &self.scope,
        };
        let bytes = serde_json_canonicalizer::to_vec(&pre)?;
        Ok(sha256(&bytes))
    }

    pub fn verify(&self) -> Result<()> {
        if self.report_version != SCORE_REPORT_VERSION {
            return Err(ScoreError::InvalidThreatIntel("unsupported report_version"));
        }
        let recomputed = CvssEvaluation::parse(self.cvss.vector())?;
        if recomputed != self.cvss {
            return Err(ScoreError::InvalidCvssVector(
                "stored CVSS scores do not match deterministic recalculation".into(),
            ));
        }
        if self
            .asset_context
            .as_ref()
            .and_then(|a| a.endpoint_target.as_ref())
            .is_some()
            && self.scope.is_none()
        {
            return Err(ScoreError::ScopeRequired);
        }
        let expected_impact = derive_business_impact(
            &self.cvss,
            &self.epss,
            &self.kev,
            self.asset_context.as_ref(),
        );
        if expected_impact != self.business_impact {
            return Err(ScoreError::InvalidAssetContext(
                "business_impact does not match deterministic derivation",
            ));
        }
        let expected_sha = self.compute_digest()?;
        if expected_sha != self.report_sha256 {
            return Err(ScoreError::DigestMismatch);
        }
        Ok(())
    }

    pub fn to_jcs(&self) -> Result<Vec<u8>> {
        self.verify()?;
        let out = serde_json_canonicalizer::to_vec(self)?;
        if out.len() > MAX_REPORT_BYTES {
            return Err(ScoreError::InvalidThreatIntel("report exceeds size limit"));
        }
        Ok(out)
    }

    pub fn verify_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(ScoreError::InvalidThreatIntel("report exceeds size limit"));
        }
        reject_duplicate_json_keys(bytes)?;
        let report: Self = serde_json::from_slice(bytes)?;
        report.verify()?;
        Ok(report)
    }
}

fn derive_business_impact(
    cvss: &CvssEvaluation,
    epss: &ThreatIntelField<EpssData>,
    kev: &ThreatIntelField<KevData>,
    asset: Option<&AssetContext>,
) -> BusinessImpact {
    let mut notes = Vec::new();
    let score = cvss.effective_score();
    let sev = cvss.effective_severity();

    match cvss {
        CvssEvaluation::V31(s) => {
            notes.push(format!(
                "CVSS v3.1 effective score {:.1} ({:?}, base {:.1})",
                s.effective_score, s.effective_severity, s.base_score
            ));
        }
        CvssEvaluation::V40(s) => {
            notes.push(format!(
                "CVSS v4.0 ({}) score {:.1} ({:?}, MacroVector {})",
                s.nomenclature, s.score, s.severity, s.macro_vector
            ));
        }
    }

    let kev_active = match kev {
        ThreatIntelField::Present(k) => {
            if k.in_kev {
                notes.push(format!(
                    "Listed in CISA KEV (added {}, source {}@{}, fetched {})",
                    k.date_added.as_deref().unwrap_or("unknown"),
                    k.source_corpus,
                    &k.pin_sha[..12],
                    k.fetched_at
                ));
                true
            } else {
                notes.push(format!(
                    "Not in CISA KEV (source {}@{}, fetched {})",
                    k.source_corpus,
                    &k.pin_sha[..12],
                    k.fetched_at
                ));
                false
            }
        }
        ThreatIntelField::Absent { reason } => {
            notes.push(format!("CISA KEV status absent (not guessed): {reason}"));
            false
        }
    };

    let high_epss = match epss {
        ThreatIntelField::Present(e) => {
            notes.push(format!(
                "EPSS {:.4} (percentile {:.2}%, source {}@{}, fetched {})",
                e.score,
                e.percentile * 100.0,
                e.source_corpus,
                &e.pin_sha[..12],
                e.fetched_at
            ));
            e.score >= 0.10
        }
        ThreatIntelField::Absent { reason } => {
            notes.push(format!("EPSS score absent (not guessed): {reason}"));
            false
        }
    };

    let mut tier_rank: i32 = match sev {
        CvssSeverity::Critical => 3,
        CvssSeverity::High => 2,
        CvssSeverity::Medium => 1,
        CvssSeverity::Low | CvssSeverity::None => 0,
    };

    if kev_active {
        tier_rank = tier_rank.max(3);
    } else if high_epss && score >= 7.0 {
        tier_rank = (tier_rank + 1).min(3);
    }

    if let Some(a) = asset {
        notes.push(format!(
            "Asset `{}`: criticality={:?}, data={:?}, exposure={:?}",
            a.asset_id, a.criticality, a.data_classification, a.exposure
        ));
        if matches!(
            a.criticality,
            AssetCriticality::High | AssetCriticality::MissionCritical
        ) && matches!(
            a.exposure,
            AssetExposure::InternetFacing | AssetExposure::PartnerFacing
        ) && score > 0.0
        {
            tier_rank = (tier_rank + 1).min(3);
        } else if a.exposure == AssetExposure::Isolated
            && !a.compensating_controls.is_empty()
            && !kev_active
        {
            tier_rank = (tier_rank - 1).max(0);
        }
        if !a.compensating_controls.is_empty() {
            notes.push(format!(
                "Compensating controls declared ({}): {}",
                a.compensating_controls.len(),
                a.compensating_controls.join("; ")
            ));
        }
    } else {
        notes.push("Asset context absent: business impact reflects technical severity and threat intelligence only".into());
    }

    let priority_tier = match tier_rank {
        3.. => PriorityTier::P1,
        2 => PriorityTier::P2,
        1 => PriorityTier::P3,
        _ => PriorityTier::P4,
    };

    BusinessImpact {
        priority_tier,
        notes,
    }
}

fn validate_cve_id(cve: &str) -> Result<()> {
    let Some(rest) = cve.strip_prefix("CVE-") else {
        return Err(ScoreError::InvalidCveId(cve.to_string()));
    };
    let Some((year, seq)) = rest.split_once('-') else {
        return Err(ScoreError::InvalidCveId(cve.to_string()));
    };
    if year.len() != 4
        || !year.bytes().all(|b| b.is_ascii_digit())
        || seq.len() < 4
        || seq.len() > 10
        || !seq.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ScoreError::InvalidCveId(cve.to_string()));
    }
    Ok(())
}

fn validate_provenance(
    corpus: &str,
    pin_sha: &str,
    fetched_at: &str,
    citation: Option<&PinnedCitation>,
) -> Result<()> {
    if !valid_slug(corpus, 128) {
        return Err(ScoreError::InvalidThreatIntel(
            "source_corpus must be 1..=128 ASCII [A-Za-z0-9._:-]",
        ));
    }
    if pin_sha.len() != 40
        || !pin_sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ScoreError::InvalidThreatIntel(
            "pin_sha must be a 40-character lowercase hex git SHA",
        ));
    }
    if !valid_date_or_rfc3339(fetched_at) {
        return Err(ScoreError::InvalidThreatIntel(
            "fetched_at must be YYYY-MM-DD or RFC 3339",
        ));
    }
    if let Some(cit) = citation {
        if cit.corpus != corpus || cit.commit != pin_sha {
            return Err(ScoreError::InvalidThreatIntel(
                "pinned_citation corpus/commit must match source_corpus and pin_sha",
            ));
        }
    }
    Ok(())
}

fn valid_slug(s: &str, max_len: usize) -> bool {
    !s.is_empty()
        && s.len() <= max_len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn valid_line(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty() && s.len() <= MAX_TEXT && !s.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn valid_date_or_rfc3339(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() || DateTime::parse_from_rfc3339(s).is_ok()
}

fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<()> {
    use serde::de::{MapAccess, Visitor};
    struct DupCheck;
    impl<'de> Deserialize<'de> for DupCheck {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = DupCheck;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("any JSON value without duplicate object keys")
                }
                fn visit_bool<E>(self, _: bool) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_i64<E>(self, _: i64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_u64<E>(self, _: u64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_f64<E>(self, _: f64) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_str<E>(self, _: &str) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_string<E>(self, _: String) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_none<E>(self) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_unit<E>(self) -> std::result::Result<DupCheck, E> {
                    Ok(DupCheck)
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> std::result::Result<DupCheck, A::Error> {
                    while seq.next_element::<DupCheck>()?.is_some() {}
                    Ok(DupCheck)
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> std::result::Result<DupCheck, A::Error> {
                    let mut seen = HashSet::new();
                    while let Some((k, _)) = map.next_entry::<String, DupCheck>()? {
                        if !seen.insert(k) {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(DupCheck)
                }
            }
            deserializer.deserialize_any(V)
        }
    }
    let _: DupCheck = serde_json::from_slice(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cvss_v31_matches_first_official_test_vectors() {
        let cases = [
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N", 7.5, None, None),
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H", 10.0, None, None),
            ("CVSS:3.1/AV:N/AC:L/PR:L/UI:N/S:C/C:L/I:L/A:N", 6.4, None, None),
            ("CVSS:3.1/AV:N/AC:H/PR:N/UI:R/S:U/C:L/I:N/A:N", 3.1, None, None),
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N", 0.0, None, None),
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H/E:P/RL:O/RC:C",
                10.0,
                Some(9.0),
                None,
            ),
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H/E:U/RL:T/RC:U/CR:H/IR:M/AR:L/MAV:A/MAC:H/MPR:L/MUI:R/MS:C/MC:L/MI:H/MA:H",
                10.0,
                Some(8.1),
                Some(5.8),
            ),
        ];

        for (vec_str, exp_base, exp_temp, exp_env) in cases {
            let res = score_cvss_v31(vec_str).expect(vec_str);
            assert_eq!(res.base_score, exp_base, "base mismatch for {vec_str}");
            assert_eq!(
                res.temporal_score, exp_temp,
                "temporal mismatch for {vec_str}"
            );
            assert_eq!(
                res.environmental_score, exp_env,
                "environmental mismatch for {vec_str}"
            );
        }
    }

    #[test]
    fn cvss_v40_matches_first_official_test_vectors() {
        let cases = [
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:H/SI:H/SA:H",
                10.0,
                "CVSS-B",
                "000100",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:H/SI:H/SA:H/MSI:S/MSA:S",
                10.0,
                "CVSS-BE",
                "000000",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N",
                9.3,
                "CVSS-B",
                "000200",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:N/VA:N/SC:N/SI:N/SA:N",
                8.7,
                "CVSS-B",
                "001200",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:N/SC:N/SI:N/SA:N",
                0.0,
                "CVSS-B",
                "002201",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N/E:P",
                8.9,
                "CVSS-BT",
                "000210",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N/E:U",
                8.1,
                "CVSS-BT",
                "000220",
            ),
            (
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N/E:P/CR:L/IR:L/AR:L/MAV:L/S:P/AU:Y",
                5.9,
                "CVSS-BTE",
                "100211",
            ),
        ];

        for (vec_str, exp_score, exp_nom, exp_mv) in cases {
            let res = score_cvss_v40(vec_str).expect(vec_str);
            assert_eq!(res.score, exp_score, "score mismatch for {vec_str}");
            assert_eq!(
                res.nomenclature, exp_nom,
                "nomenclature mismatch for {vec_str}"
            );
            assert_eq!(
                res.macro_vector, exp_mv,
                "macro_vector mismatch for {vec_str}"
            );
        }
    }

    #[test]
    fn invalid_vectors_produce_precise_errors() {
        assert!(CvssEvaluation::parse("CVSS:3.0/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N").is_err());
        assert!(CvssEvaluation::parse("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N").is_err());
        assert!(
            CvssEvaluation::parse("CVSS:3.1/AV:N/AV:L/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N").is_err()
        );
        assert!(CvssEvaluation::parse("CVSS:3.1/AV:Z/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N").is_err());
        let e = CvssEvaluation::parse(
            "CVSS:4.0/AC:L/AV:N/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("out of canonical order"), "{e}");
    }

    #[test]
    fn absent_epss_and_kev_are_stated_not_guessed_and_present_values_carry_provenance() {
        let rep = CyberScoreReport::evaluate(
            "finding-1",
            Some("CVE-2024-12345".into()),
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N",
            ThreatIntelField::absent("no offline EPSS pack installed"),
            ThreatIntelField::absent("no offline KEV pack installed"),
            None,
        )
        .unwrap();

        let json_bytes = rep.to_jcs().unwrap();
        let text = String::from_utf8(json_bytes.clone()).unwrap();
        assert!(text.contains("\"status\":\"absent\""), "{text}");
        let round_trip = CyberScoreReport::verify_json(&json_bytes).unwrap();
        assert_eq!(round_trip.report_sha256, rep.report_sha256);

        let epss = ThreatIntelField::Present(EpssData {
            cve_id: "CVE-2021-44228".into(),
            score: 0.975,
            percentile: 0.999,
            source_corpus: "first-epss".into(),
            pin_sha: "a".repeat(40),
            fetched_at: "2026-11-05".into(),
            pinned_citation: None,
        });
        let kev = ThreatIntelField::Present(KevData {
            cve_id: "CVE-2021-44228".into(),
            in_kev: true,
            date_added: Some("2021-12-10".into()),
            due_date: Some("2021-12-24".into()),
            known_ransomware_campaign_use: Some("Known".into()),
            source_corpus: "cisa-kev".into(),
            pin_sha: "b".repeat(40),
            fetched_at: "2026-11-05T12:00:00Z".into(),
            pinned_citation: None,
        });
        let asset = AssetContext {
            asset_id: "edge-gateway".into(),
            criticality: AssetCriticality::MissionCritical,
            data_classification: DataClassification::Restricted,
            exposure: AssetExposure::InternetFacing,
            compensating_controls: vec!["WAF virtual patch active".into()],
            endpoint_target: Some("api.example.com".into()),
        };

        assert!(matches!(
            CyberScoreReport::evaluate(
                "finding-log4j",
                Some("CVE-2021-44228".into()),
                "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:H/SI:H/SA:H",
                epss.clone(),
                kev.clone(),
                Some(asset.clone()),
            ),
            Err(ScoreError::ScopeRequired)
        ));

        let t0 = Utc.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 11, 7, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 0, 0, 0).unwrap();
        let guard = ScopeGuard::from_work_order(
            Some(WorkOrder {
                id: "wo-2026-11".into(),
                authorization: "Signed ROE-42".into(),
                targets: vec!["api.example.com".into()],
                not_before: t0,
                expires_at: t1,
            }),
            InputOrigin::AuthorizedControlPlane,
        )
        .unwrap();

        let scoped = CyberScoreReport::evaluate_with_scope(
            "finding-log4j",
            Some("CVE-2021-44228".into()),
            "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:H/SI:H/SA:H",
            epss,
            kev,
            Some(asset),
            &guard,
            InputOrigin::AuthorizedControlPlane,
            now,
        )
        .unwrap();
        assert_eq!(scoped.business_impact.priority_tier, PriorityTier::P1);
        assert!(scoped.scope.is_some());
        let jcs = scoped.to_jcs().unwrap();
        assert_eq!(
            CyberScoreReport::verify_json(&jcs).unwrap().report_sha256,
            scoped.report_sha256
        );
    }
}
