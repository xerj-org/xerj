//! Cross-dataset correlation with captured evidence.
//! Key overlap: sampled distinct-value sets intersected across datasets,
//! then CONFIRMED post-index with term queries against the full data.
//! Time alignment: date_histogram per dataset, range overlap + Pearson r on
//! aligned bucket counts. Every number traces to an engine response.

use crate::esclient::Es;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;

pub struct Candidate {
    pub slug: String,
    pub index: String,
    pub field: String,
    pub kind: String, // es_type (+ entity)
    pub values: HashSet<String>,
    pub sampled_n: u64,
}

#[derive(Debug, Clone)]
pub struct KeyCorr {
    pub a_slug: String,
    pub a_index: String,
    pub a_field: String,
    pub b_slug: String,
    pub b_index: String,
    pub b_field: String,
    pub kind: String,
    pub a_set: usize,
    pub b_set: usize,
    pub overlap: usize,
    pub containment: f64,
    pub grade: String,
    pub examples: Vec<String>,
    pub confirmed: Option<(u32, u32)>,
    pub confirm_details: Vec<String>,
}

impl KeyCorr {
    pub fn id(&self) -> String {
        format!(
            "corr:{}:{}:{}:{}",
            self.a_slug, self.b_slug, self.a_field, self.b_field
        )
    }
    pub fn to_value(&self) -> Value {
        json!({
            "doc_kind": "correlation",
            "corr_kind": "key_overlap",
            "a_dataset": self.a_slug, "a_index": self.a_index, "a_field": self.a_field,
            "b_dataset": self.b_slug, "b_index": self.b_index, "b_field": self.b_field,
            "value_kind": self.kind,
            "a_sampled_distinct": self.a_set,
            "b_sampled_distinct": self.b_set,
            "overlap": self.overlap,
            "containment": self.containment,
            "grade": self.grade,
            "examples": self.examples,
            "confirmed_values": self.confirmed.map(|(c, _)| c),
            "tested_values": self.confirmed.map(|(_, t)| t),
            "confirm_details": self.confirm_details,
            "caveat": "overlap computed on bounded samples (≤8192 distinct values/field); confirmation queries ran against the fully indexed data",
        })
    }
}

/// Key-like candidate filter. Deviations from the design draft (recorded):
/// - keyword fields qualify at cardinality ≥ 20 regardless of distinct/n
///   ratio (join keys like user ids have LOW sample ratios in big datasets);
/// - numeric fields need ratio ≥ 0.5 AND must not look like a LOCAL ordinal
///   or measurement: a dense integer set anchored near zero (auto-increment
///   row ids, token counts, latencies) carries no cross-dataset identity and
///   overlaps everything shaped like it. Business identifiers (e.g. ids
///   starting at 500000) survive the guard.
pub fn is_candidate(
    es_type: &str,
    entity: Option<&str>,
    distinct: usize,
    n: u64,
    overflow: bool,
    int_range: Option<(i64, i64)>,
) -> bool {
    match es_type {
        "keyword" => distinct >= 20 || entity.is_some() && distinct >= 10,
        "long" => {
            if overflow || distinct < 25 || n == 0 || (distinct as f64 / n as f64) < 0.5 {
                return false;
            }
            if let Some((lo, hi)) = int_range {
                let span = (hi - lo).max(0) as f64 + 1.0;
                let density = distinct as f64 / span;
                if lo <= 100 && density >= 0.3 {
                    return false; // small dense ints: ordinal/measurement, not identity
                }
            }
            true
        }
        _ => false,
    }
}

pub fn key_overlaps(cands: &[Candidate]) -> Vec<KeyCorr> {
    let mut out = Vec::new();
    for i in 0..cands.len() {
        for j in (i + 1)..cands.len() {
            let (a, b) = (&cands[i], &cands[j]);
            if a.slug == b.slug || a.kind != b.kind {
                continue;
            }
            let overlap = a.values.intersection(&b.values).count();
            if overlap < 25 {
                continue;
            }
            let containment = overlap as f64 / a.values.len().min(b.values.len()).max(1) as f64;
            if containment < 0.3 {
                continue;
            }
            let grade = if containment >= 0.7 {
                "strong"
            } else {
                "moderate"
            };
            let mut examples: Vec<String> =
                a.values.intersection(&b.values).take(20).cloned().collect();
            examples.sort();
            out.push(KeyCorr {
                a_slug: a.slug.clone(),
                a_index: a.index.clone(),
                a_field: a.field.clone(),
                b_slug: b.slug.clone(),
                b_index: b.index.clone(),
                b_field: b.field.clone(),
                kind: a.kind.clone(),
                a_set: a.values.len(),
                b_set: b.values.len(),
                overlap,
                containment,
                grade: grade.into(),
                examples,
                confirmed: None,
                confirm_details: Vec::new(),
            });
        }
    }
    // strongest first, cap the report
    out.sort_by(|x, y| {
        y.containment
            .partial_cmp(&x.containment)
            .unwrap()
            .then(y.overlap.cmp(&x.overlap))
    });
    out.truncate(50);
    out
}

/// Post-index confirmation: term-query up to `n_test` overlapping values
/// against BOTH indices on the full data.
pub fn confirm(es: &Es, corr: &mut KeyCorr, n_test: usize) -> Result<()> {
    let mut confirmed = 0u32;
    let mut tested = 0u32;
    let vals: Vec<String> = corr.examples.iter().take(n_test).cloned().collect();
    for v in vals {
        tested += 1;
        // long-typed keys must be queried as numbers, not strings
        let tv: Value = if corr.kind == "long" {
            v.parse::<i64>()
                .map(|n| json!(n))
                .unwrap_or_else(|_| json!(v.clone()))
        } else {
            json!(v.clone())
        };
        let qa = json!({"size":0,"track_total_hits":true,
            "query":{"term":{(corr.a_field.clone()): tv.clone()}}});
        let qb = json!({"size":0,"track_total_hits":true,
            "query":{"term":{(corr.b_field.clone()): tv}}});
        let ha = es
            .search(&corr.a_index, &qa)?
            .pointer("/hits/total/value")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let hb = es
            .search(&corr.b_index, &qb)?
            .pointer("/hits/total/value")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        if ha > 0 && hb > 0 {
            confirmed += 1;
        }
        if corr.confirm_details.len() < 5 {
            corr.confirm_details.push(format!(
                "value {:?}: {} hits in {}, {} hits in {}",
                v, ha, corr.a_index, hb, corr.b_index
            ));
        }
    }
    corr.examples.truncate(5);
    corr.confirmed = Some((confirmed, tested));
    Ok(())
}

pub struct TimeSeries {
    pub slug: String,
    pub index: String,
    pub field: String,
    pub buckets: Vec<(i64, u64)>, // epoch ms → count
}

/// Fetch a day (or hour when span < 7d) histogram for a dataset time field.
pub fn fetch_histogram(
    es: &Es,
    slug: &str,
    index: &str,
    field: &str,
) -> Result<Option<TimeSeries>> {
    let fetch = |interval: &str| -> Result<Vec<(i64, u64)>> {
        let body = json!({"size":0,"aggs":{"t":{"date_histogram":{
            "field": field, "calendar_interval": interval}}}});
        let v = es.search(index, &body)?;
        Ok(v.pointer("/aggregations/t/buckets")
            .and_then(|b| b.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| Some((b.get("key")?.as_i64()?, b.get("doc_count")?.as_u64()?)))
                    .collect()
            })
            .unwrap_or_default())
    };
    let daily = fetch("day")?;
    if daily.is_empty() {
        return Ok(None);
    }
    let span_ms = daily.last().unwrap().0 - daily.first().unwrap().0;
    let buckets = if span_ms < 7 * 86_400_000 {
        let hourly = fetch("hour")?;
        if hourly.is_empty() {
            daily
        } else {
            hourly
        }
    } else {
        daily
    };
    Ok(Some(TimeSeries {
        slug: slug.to_string(),
        index: index.to_string(),
        field: field.to_string(),
        buckets,
    }))
}

fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    let n = pairs.len() as f64;
    if pairs.len() < 3 {
        return None;
    }
    let (mut sx, mut sy) = (0.0, 0.0);
    for (x, y) in pairs {
        sx += x;
        sy += y;
    }
    let (mx, my) = (sx / n, sy / n);
    let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
    for (x, y) in pairs {
        num += (x - mx) * (y - my);
        dx += (x - mx) * (x - mx);
        dy += (y - my) * (y - my);
    }
    if dx <= 0.0 || dy <= 0.0 {
        return None;
    }
    Some(num / (dx * dy).sqrt())
}

/// Pairwise time alignment across all datasets with elected time fields.
pub fn time_alignment(series: &[TimeSeries]) -> Vec<Value> {
    let mut out = Vec::new();
    for i in 0..series.len() {
        for j in (i + 1)..series.len() {
            let (a, b) = (&series[i], &series[j]);
            if a.buckets.is_empty() || b.buckets.is_empty() {
                continue;
            }
            let (a0, a1) = (a.buckets.first().unwrap().0, a.buckets.last().unwrap().0);
            let (b0, b1) = (b.buckets.first().unwrap().0, b.buckets.last().unwrap().0);
            let inter = (a1.min(b1) - a0.max(b0)).max(0) as f64;
            let union = (a1.max(b1) - a0.min(b0)).max(1) as f64;
            let range_overlap = inter / union;
            if range_overlap < 0.5 {
                continue;
            }
            // align buckets by key
            let bm: std::collections::HashMap<i64, u64> = b.buckets.iter().cloned().collect();
            let mut pairs: Vec<(f64, f64)> = Vec::new();
            let mut peaks: Vec<(u64, i64)> = Vec::new();
            for (k, c) in &a.buckets {
                if let Some(cb) = bm.get(k) {
                    pairs.push((*c as f64, *cb as f64));
                    peaks.push((*c + *cb, *k));
                }
            }
            let r = if pairs.len() >= 10 {
                pearson(&pairs)
            } else {
                None
            };
            peaks.sort_by_key(|p| std::cmp::Reverse(p.0));
            let top: Vec<String> = peaks
                .iter()
                .take(3)
                .map(|(c, k)| {
                    format!(
                        "{} (combined {} docs)",
                        chrono::DateTime::from_timestamp_millis(*k)
                            .map(|d| d.to_rfc3339())
                            .unwrap_or_else(|| k.to_string()),
                        c
                    )
                })
                .collect();
            let fmt_ms = |ms: i64| {
                chrono::DateTime::from_timestamp_millis(ms)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| ms.to_string())
            };
            out.push(json!({
                "doc_kind": "correlation",
                "corr_kind": "time_alignment",
                "a_dataset": a.slug, "a_index": a.index, "a_field": a.field,
                "b_dataset": b.slug, "b_index": b.index, "b_field": b.field,
                "a_range": [fmt_ms(a0), fmt_ms(a1)],
                "b_range": [fmt_ms(b0), fmt_ms(b1)],
                "range_overlap": range_overlap,
                "shared_buckets": pairs.len(),
                "pearson_r": r,
                "activity_correlated": r.map(|r| r >= 0.5).unwrap_or(false),
                "top_co_peaks": top,
                "evidence": "date_histogram aggregations against the indexed data",
            }));
        }
    }
    out
}
