//! Type / semantics inference from bounded samples.
//! ≥95% of non-null values must parse for a typed verdict; everything else
//! falls back to keyword/text. Dates get an elected encoding with per-
//! encoding evidence counts; epoch numbers need guards + corroboration.

pub mod dates;
pub mod entities;

use dates::DateEnc;
use entities::Entity;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const DISTINCT_CAP: usize = 8192;
pub const RAW_CAP: usize = 8192;
pub const MAX_FIELDS_PER_DATASET: usize = 512;

#[derive(Debug, Default, Clone)]
pub struct FieldAcc {
    pub n: u64, // non-null values seen
    pub bool_ok: u64,
    pub long_ok: u64,
    pub double_ok: u64,
    pub json_bool: u64,
    pub json_num: u64,
    pub str_n: u64,
    pub date_hits: HashMap<DateEnc, u64>,
    pub distinct: HashSet<u64>,
    pub distinct_overflow: bool,
    pub raw_values: HashSet<String>,
    pub examples: Vec<String>,
    pub len_samples: Vec<u32>,
    pub token_samples: Vec<u32>,
    pub len_sum: u64,
    pub entity: HashMap<Entity, u64>,
    pub int_min: i64,
    pub int_max: i64,
    pub date_min: Option<chrono::DateTime<chrono::Utc>>,
    pub date_max: Option<chrono::DateTime<chrono::Utc>>,
}

impl FieldAcc {
    pub fn add(&mut self, v: &Value) {
        match v {
            Value::Null => {}
            Value::Array(a) => {
                for e in a {
                    if !e.is_array() {
                        self.add(e);
                    }
                }
            }
            Value::Bool(b) => {
                self.n += 1;
                self.bool_ok += 1;
                self.json_bool += 1;
                self.note_distinct(if *b { "true" } else { "false" });
            }
            Value::Number(num) => {
                self.n += 1;
                self.json_num += 1;
                self.double_ok += 1;
                if let Some(i) = num.as_i64() {
                    self.long_ok += 1;
                    self.track_int(i);
                }
                let s = num.to_string();
                self.note_distinct(&s);
                self.note_example(&s);
            }
            Value::String(s) => {
                self.n += 1;
                self.str_n += 1;
                let t = s.trim();
                if t == "true" || t == "false" {
                    self.bool_ok += 1;
                }
                if let Ok(i) = t.parse::<i64>() {
                    self.long_ok += 1;
                    self.double_ok += 1;
                    self.track_int(i);
                } else if t.parse::<f64>().is_ok() && t.chars().any(|c| c.is_ascii_digit()) {
                    self.double_ok += 1;
                }
                if let Some((dt, enc)) = dates::parse_date_str(t) {
                    *self.date_hits.entry(enc).or_default() += 1;
                    self.track_date(dt);
                }
                if let Some(e) = entities::classify(t) {
                    *self.entity.entry(e).or_default() += 1;
                }
                let len = t.chars().count() as u32;
                let toks = t.split_whitespace().count() as u32;
                self.len_sum += len as u64;
                if self.len_samples.len() < 512 {
                    self.len_samples.push(len);
                    self.token_samples.push(toks);
                }
                self.note_distinct(t);
                self.note_example(t);
            }
            Value::Object(_) => {} // flattened upstream; ignore
        }
    }

    fn track_int(&mut self, i: i64) {
        if self.long_ok == 1 {
            self.int_min = i;
            self.int_max = i;
        } else {
            self.int_min = self.int_min.min(i);
            self.int_max = self.int_max.max(i);
        }
    }

    fn track_date(&mut self, dt: chrono::DateTime<chrono::Utc>) {
        self.date_min = Some(match self.date_min {
            Some(m) => m.min(dt),
            None => dt,
        });
        self.date_max = Some(match self.date_max {
            Some(m) => m.max(dt),
            None => dt,
        });
    }

    fn note_distinct(&mut self, s: &str) {
        if self.distinct.len() < DISTINCT_CAP {
            self.distinct
                .insert(xxhash_rust::xxh3::xxh3_64(s.as_bytes()));
        } else {
            self.distinct_overflow = true;
        }
        if self.raw_values.len() < RAW_CAP {
            self.raw_values.insert(s.chars().take(256).collect());
        }
    }

    fn note_example(&mut self, s: &str) {
        if self.examples.len() < 3 && !s.is_empty() {
            let short: String = s.chars().take(120).collect();
            if !self.examples.contains(&short) {
                self.examples.push(short);
            }
        }
    }

    pub fn merge(&mut self, other: &FieldAcc) {
        let self_had_ints = self.long_ok > 0;
        self.bool_ok += other.bool_ok;
        self.long_ok += other.long_ok;
        self.double_ok += other.double_ok;
        self.json_bool += other.json_bool;
        self.json_num += other.json_num;
        self.str_n += other.str_n;
        for (k, v) in &other.date_hits {
            *self.date_hits.entry(*k).or_default() += v;
        }
        for h in &other.distinct {
            if self.distinct.len() >= DISTINCT_CAP {
                self.distinct_overflow = true;
                break;
            }
            self.distinct.insert(*h);
        }
        self.distinct_overflow |= other.distinct_overflow;
        for r in &other.raw_values {
            if self.raw_values.len() >= RAW_CAP {
                break;
            }
            self.raw_values.insert(r.clone());
        }
        for e in &other.examples {
            if self.examples.len() < 3 && !self.examples.contains(e) {
                self.examples.push(e.clone());
            }
        }
        for (i, l) in other.len_samples.iter().enumerate() {
            if self.len_samples.len() >= 512 {
                break;
            }
            self.len_samples.push(*l);
            self.token_samples.push(other.token_samples[i]);
        }
        self.len_sum += other.len_sum;
        for (k, v) in &other.entity {
            *self.entity.entry(*k).or_default() += v;
        }
        if other.long_ok > 0 {
            if !self_had_ints {
                self.int_min = other.int_min;
                self.int_max = other.int_max;
            } else {
                self.int_min = self.int_min.min(other.int_min);
                self.int_max = self.int_max.max(other.int_max);
            }
        }
        if let Some(d) = other.date_min {
            self.track_date(d);
        }
        if let Some(d) = other.date_max {
            self.track_date(d);
        }
        self.n += other.n;
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldSpec {
    pub name: String,
    pub es_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_enc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<String>,
    pub cardinality_est: u64,
    pub cardinality_overflow: bool,
    pub null_ratio: f64,
    pub avg_len: f64,
    pub coverage: f64,
    pub examples: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_max: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub date_evidence: Vec<String>,
}

fn p95(samples: &[u32]) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let mut v: Vec<u32> = samples.to_vec();
    v.sort_unstable();
    v[((v.len() - 1) * 95) / 100]
}

/// Infer the full field spec list for a dataset. `records` = sampled record
/// count (for null ratios). Two passes so epoch candidates can corroborate
/// against elected date fields.
pub fn infer_fields(
    fields: &HashMap<String, FieldAcc>,
    records: u64,
    no_semantic: bool,
) -> Vec<FieldSpec> {
    let mut specs: Vec<FieldSpec> = Vec::new();
    let mut date_ranges: Vec<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> =
        Vec::new();
    let mut epoch_pending: Vec<(usize, i64, i64, DateEnc)> = Vec::new(); // spec idx, min, max

    let mut names: Vec<&String> = fields.keys().collect();
    names.sort();
    for name in names {
        let acc = &fields[name];
        if acc.n == 0 {
            continue;
        }
        let mut spec = FieldSpec {
            name: name.clone(),
            es_type: "keyword".into(),
            date_enc: None,
            semantic: None,
            cardinality_est: acc.distinct.len() as u64,
            cardinality_overflow: acc.distinct_overflow,
            null_ratio: 1.0 - (acc.n.min(records) as f64 / records.max(1) as f64),
            avg_len: acc.len_sum as f64 / acc.str_n.max(1) as f64,
            coverage: acc.n.min(records) as f64 / records.max(1) as f64,
            examples: acc.examples.clone(),
            notes: Vec::new(),
            date_min: acc.date_min.map(|d| dates::to_rfc3339_millis(&d)),
            date_max: acc.date_max.map(|d| dates::to_rfc3339_millis(&d)),
            date_evidence: Vec::new(),
        };
        let n = acc.n;
        let th95 = |x: u64| x * 100 >= n * 95;

        // boolean
        if acc.json_bool == n || (acc.str_n == n && acc.bool_ok == n) {
            spec.es_type = "boolean".into();
            specs.push(spec);
            continue;
        }
        // string dates
        let date_total: u64 = acc.date_hits.values().sum();
        if acc.str_n > 0 && th95(date_total) && date_total > 0 {
            let elected = acc
                .date_hits
                .iter()
                .max_by_key(|(_, v)| **v)
                .map(|(k, _)| *k)
                .unwrap();
            spec.es_type = "date".into();
            spec.date_enc = Some(elected.as_str().into());
            let mut ev: Vec<(DateEnc, u64)> = acc.date_hits.iter().map(|(k, v)| (*k, *v)).collect();
            ev.sort();
            spec.date_evidence = ev
                .iter()
                .map(|(k, v)| format!("{}: {}", k.as_str(), v))
                .collect();
            if let (Some(a), Some(b)) = (acc.date_min, acc.date_max) {
                date_ranges.push((a, b));
            }
            specs.push(spec);
            continue;
        }
        // numeric
        if th95(acc.long_ok) && acc.long_ok > 0 {
            spec.es_type = "long".into();
            // epoch candidate?
            let (lo, hi) = (acc.int_min, acc.int_max);
            let in_ms = lo >= dates::EPOCH_MS_MIN && hi <= dates::EPOCH_MS_MAX;
            let in_s = lo >= dates::EPOCH_S_MIN && hi <= dates::EPOCH_S_MAX;
            if (in_ms || in_s) && acc.distinct.len() >= 20 {
                let enc = if in_ms {
                    DateEnc::EpochMillis
                } else {
                    DateEnc::EpochSeconds
                };
                let span_ms = if in_ms { hi - lo } else { (hi - lo) * 1000 };
                let twenty_years_ms: i64 = 20 * 365 * 24 * 3600 * 1000;
                if span_ms < twenty_years_ms {
                    spec.es_type = "date".into();
                    spec.date_enc = Some(enc.as_str().into());
                    spec.date_evidence =
                        vec![format!("{}: {} (range-guarded)", enc.as_str(), acc.long_ok)];
                    let to_dt = |v: i64| dates::parse_epoch(v).map(|(d, _)| d);
                    if let (Some(a), Some(b)) = (to_dt(lo), to_dt(hi)) {
                        spec.date_min = Some(dates::to_rfc3339_millis(&a));
                        spec.date_max = Some(dates::to_rfc3339_millis(&b));
                        date_ranges.push((a, b));
                    }
                } else {
                    epoch_pending.push((specs.len(), lo, hi, enc));
                    spec.notes.push(format!(
                        "possible {} (window match, span ≥20y — kept long pending corroboration)",
                        enc.as_str()
                    ));
                }
            }
            specs.push(spec);
            continue;
        }
        if th95(acc.double_ok) && acc.double_ok > 0 {
            spec.es_type = "double".into();
            specs.push(spec);
            continue;
        }
        // strings → entity / keyword / text
        let ent = acc
            .entity
            .iter()
            .max_by_key(|(_, v)| **v)
            .filter(|(_, v)| **v * 10 >= n * 9 && n >= 20)
            .map(|(k, _)| *k);
        if let Some(e) = ent {
            spec.es_type = "keyword".into();
            spec.semantic = Some(e.as_str().into());
            specs.push(spec);
            continue;
        }
        let p95_len = p95(&acc.len_samples);
        let p95_tok = p95(&acc.token_samples);
        let card_ratio = if acc.distinct_overflow {
            1.0
        } else {
            acc.distinct.len() as f64 / n as f64
        };
        let is_keyword =
            (p95_len <= 128 && p95_tok <= 3) || (!acc.distinct_overflow && card_ratio < 0.1);
        let is_text = p95_tok > 8 || p95_len > 256;
        spec.es_type = if is_keyword && !is_text {
            "keyword".into()
        } else if is_text {
            "text".into()
        } else if card_ratio < 0.5 {
            "keyword".into()
        } else {
            "text".into()
        };
        specs.push(spec);
    }

    // epoch corroboration pass
    for (idx, lo, hi, enc) in epoch_pending {
        let to_dt = |v: i64| dates::parse_epoch(v).map(|(d, _)| d);
        if let (Some(a), Some(b)) = (to_dt(lo), to_dt(hi)) {
            let overlaps = date_ranges.iter().any(|(ra, rb)| a <= *rb && *ra <= b);
            if overlaps {
                let spec = &mut specs[idx];
                spec.es_type = "date".into();
                spec.date_enc = Some(enc.as_str().into());
                spec.notes
                    .push("epoch corroborated by sibling date field range".into());
                spec.date_min = Some(dates::to_rfc3339_millis(&a));
                spec.date_max = Some(dates::to_rfc3339_millis(&b));
            }
        }
    }

    // semantic body election: largest avg_len text field ≥ 200 chars
    if !no_semantic {
        let best = specs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.es_type == "text" && s.avg_len >= 200.0)
            .max_by(|a, b| a.1.avg_len.partial_cmp(&b.1.avg_len).unwrap());
        if let Some((i, _)) = best {
            specs[i].es_type = "semantic_text".into();
            specs[i].notes.push(
                "hybrid lexical+vector body (embedded server-side: lexical by default, neural/proxy if configured)".into(),
            );
        }
    }

    // field cap
    if specs.len() > MAX_FIELDS_PER_DATASET {
        specs.sort_by(|a, b| {
            b.coverage
                .partial_cmp(&a.coverage)
                .unwrap()
                .then(a.name.cmp(&b.name))
        });
        let overflow: Vec<String> = specs[MAX_FIELDS_PER_DATASET..]
            .iter()
            .map(|s| s.name.clone())
            .collect();
        specs.truncate(MAX_FIELDS_PER_DATASET);
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(first) = specs.first_mut() {
            first.notes.push(format!(
                "dataset field cap hit; unmapped overflow fields: {}",
                overflow.join(", ")
            ));
        }
    }
    specs
}

/// Elect the dataset time field: date-typed field with the highest coverage.
pub fn elect_time_field(specs: &[FieldSpec]) -> Option<String> {
    specs
        .iter()
        .filter(|s| s.es_type == "date")
        .max_by(|a, b| {
            a.coverage
                .partial_cmp(&b.coverage)
                .unwrap()
                .then_with(|| b.name.cmp(&a.name))
        })
        .map(|s| s.name.clone())
}
