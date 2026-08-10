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

/// The extractor-owned fields that carry a document's text. Every document
/// extractor puts its retrievable content in exactly one of these
/// (`extract::emit_document`, `extract::code`, `extract::txt`), which is what
/// lets a document dataset elect them ALL as `semantic_text` without ever
/// embedding one record twice.
pub const DOC_BODY_FIELDS: &[&str] = &["body", "text"];

/// 256-slot byte histogram with a `Default` impl (`[u32; 256]` has none).
#[derive(Debug, Clone)]
pub struct ByteHist(pub [u32; 256]);
impl Default for ByteHist {
    fn default() -> Self {
        ByteHist([0u32; 256])
    }
}
impl std::ops::Deref for ByteHist {
    type Target = [u32; 256];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ByteHist {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

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
    /// Word-shaped tokens (`[A-Za-z]{3,}`) vs total tokens, over the sample.
    /// This ratio is what separates natural language from opaque identifiers,
    /// and it does so far more reliably than any embedding-derived signal:
    /// measured on real fields, `word_ratio` was 0.00 for `trace_id`,
    /// `user_id`, `order_id` and numeric columns, and 0.78–1.00 for log
    /// messages, prose and source code.  (An embedding-structure metric tried
    /// on the same fields ranked a numeric column ABOVE real log messages —
    /// embeddings measure similarity, not whether a field carries meaning.)
    pub word_tokens: u64,
    pub total_tokens: u64,
    pub len_sum: u64,
    /// Byte histogram over sampled string values, for Shannon entropy.  High
    /// entropy with a low `word_ratio` is the signature of a hash / base64 /
    /// uuid column.
    pub byte_hist: ByteHist,
    pub byte_total: u64,
    pub entity: HashMap<Entity, u64>,
    pub int_min: i64,
    pub int_max: i64,
    pub date_min: Option<chrono::DateTime<chrono::Utc>>,
    pub date_max: Option<chrono::DateTime<chrono::Utc>>,
}

impl FieldAcc {
    /// Fraction of sampled tokens that look like words. ~0 for identifiers,
    /// numbers and hashes; high for prose, log messages and source code.
    pub fn word_ratio(&self) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        self.word_tokens as f64 / self.total_tokens as f64
    }

    /// Mean whitespace-token count per sampled value — distinguishes a
    /// multi-word body from a single-token enum or code.
    pub fn mean_tokens(&self) -> f64 {
        if self.token_samples.is_empty() {
            return 0.0;
        }
        self.token_samples.iter().map(|t| *t as f64).sum::<f64>() / self.token_samples.len() as f64
    }

    /// Shannon entropy (bits/byte) over sampled string bytes.
    pub fn char_entropy(&self) -> f64 {
        if self.byte_total == 0 {
            return 0.0;
        }
        let tot = self.byte_total as f64;
        -self
            .byte_hist
            .iter()
            .filter(|c| **c > 0)
            .map(|c| {
                let p = *c as f64 / tot;
                p * p.log2()
            })
            .sum::<f64>()
    }

    /// Is this field worth embedding?  Natural language only: mostly
    /// word-shaped tokens AND more than a couple of tokens per value.
    /// Deliberately conservative — embedding an identifier column is pure
    /// cost (the built-in neural backend runs at ~3 docs/s) and produces a
    /// vector space with no useful neighbourhoods.
    pub fn looks_natural_language(&self) -> bool {
        self.word_ratio() >= 0.55 && self.mean_tokens() >= 3.0
    }

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
                // Lexical shape — cheap, deterministic, and computed from the
                // same sample everything else uses.
                for tok in t.split(|c: char| !c.is_alphanumeric()) {
                    if tok.is_empty() {
                        continue;
                    }
                    self.total_tokens += 1;
                    if tok.len() >= 3 && tok.chars().all(|c| c.is_ascii_alphabetic()) {
                        self.word_tokens += 1;
                    }
                }
                if self.byte_total < 1 << 20 {
                    for b in t.bytes() {
                        self.byte_hist[b as usize] += 1;
                        self.byte_total += 1;
                    }
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

/// Elect the date encoding recorded in the field's mapping.
///
/// `date_hits` is a `HashMap`, so this comparator has to be TOTAL: anything
/// left tied would be settled by the map's per-instance random hash seed, and
/// the same sample would write a different `date_enc` on every run — which
/// then reaches the catalog and the coercion plan (`coerce::plan_from_specs`),
/// where the elected encoding decides how bare integers are read.
///
/// Most evidence wins; a tie goes to the lowest `DateEnc` in declaration
/// order. That order is not arbitrary: it is the order `parse_date_str` tries
/// the encodings in, richest first — rfc3339 carries an explicit zone,
/// iso-naive and space-naive carry a time we have to assume is UTC, date-only
/// carries no time at all. On a genuine 50/50 split (a field mixing
/// `2026-03-17T00:00:13` with `2026-03-17 00:00:13`) we name the encoding that
/// concedes the least. It is also the order `date_evidence` is sorted in, so
/// the elected encoding is always the first of the tied ones listed there.
fn elect_date_enc(date_hits: &HashMap<DateEnc, u64>) -> Option<DateEnc> {
    date_hits
        .iter()
        .min_by(|(a_enc, a_n), (b_enc, b_n)| b_n.cmp(a_n).then_with(|| a_enc.cmp(b_enc)))
        .map(|(k, _)| *k)
}

/// Elect the field's semantic entity tag (keyword + `semantic`).
///
/// `entity` is a `HashMap`, so the comparator has to be TOTAL for the same
/// reason as above. Note that a tie cannot change TODAY's verdict: the caller
/// demands the winner hold ≥90% of the field's values, and two entities each
/// holding ≥90% of the same sample is arithmetically impossible. That safety
/// is an accident of one constant, not a property of this election — relax the
/// supermajority to a plurality and the hash seed picks the mapping. Ordering
/// it totally costs nothing and does not wait for that edit.
///
/// Most values wins; a tie goes to the most specific classifier, so a field
/// split between shapes is tagged the way `entities::classify` tags a value
/// that could be read as either.
fn elect_entity(entity: &HashMap<Entity, u64>) -> Option<Entity> {
    entity
        .iter()
        .min_by(|(a_ent, a_n), (b_ent, b_n)| {
            b_n.cmp(a_n)
                .then_with(|| a_ent.precedence().cmp(&b_ent.precedence()))
        })
        .map(|(k, _)| *k)
}

/// Infer the full field spec list for a dataset. `records` = sampled record
/// count (for null ratios). Two passes so epoch candidates can corroborate
/// against elected date fields.
pub fn infer_fields(
    fields: &HashMap<String, FieldAcc>,
    records: u64,
    no_semantic: bool,
) -> Vec<FieldSpec> {
    infer_fields_with_policy(fields, records, no_semantic, false)
}

/// `infer_fields` with the semantic election policy explicit.
///
/// `semantic_all = false` (data datasets): elect at most ONE semantic body —
/// the longest qualifying field. A data record usually carries several long
/// text columns holding the SAME entity, and embedding every one multiplies
/// cost (the built-in neural backend measures ~3 docs/s) for no retrieval
/// gain.
///
/// `semantic_all = true` (document datasets): elect every qualifying BODY
/// CARRIER (`DOC_BODY_FIELDS` — extractor-owned names, so this is a contract
/// of this crate, not a heuristic). A document dataset merges several
/// extractor vocabularies (`body` from code/prose/pdf/html, `text` from
/// line-oriented chunks) and each record populates exactly one carrier, so
/// electing all of them costs the same embedding work as electing one —
/// while electing one silently cuts entire format families out of the
/// vector arm (#173: 98.8% of the corpus had no `semantic_text` at all).
/// Non-carrier text fields (`defs` symbol lists) stay lexical: they ride on
/// the same record as a carrier, so electing them would embed every code
/// record twice.
pub fn infer_fields_with_policy(
    fields: &HashMap<String, FieldAcc>,
    records: u64,
    no_semantic: bool,
    semantic_all: bool,
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
            let elected = elect_date_enc(&acc.date_hits).unwrap();
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
        let ent = elect_entity(&acc.entity).filter(|e| acc.entity[e] * 10 >= n * 9 && n >= 20);
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

    // Semantic body election.
    //
    // Previously: "largest avg_len text field >= 200 chars".  That is a proxy
    // for "is this natural language" and it is wrong in both directions — a
    // 300-char base64 blob or a concatenated id column qualifies, while a
    // genuinely semantic 150-char summary field does not.  Embedding the wrong
    // column is expensive (the built-in neural backend measures ~3 docs/s) and
    // yields a vector space with no useful neighbourhoods.
    //
    // Now: require the field to actually look like natural language
    // (`word_ratio >= 0.55 && mean_tokens >= 3`), then pick the longest such
    // field.  Measured `word_ratio` on real columns: 0.00 for trace_id /
    // user_id / order_id / numerics, 0.78-1.00 for prose, log messages and
    // source code.  The length floor is kept but relaxed, because the
    // language test now does the discriminating.
    if !no_semantic {
        let qualifying: Vec<usize> = specs
            .iter()
            .enumerate()
            .filter(|(_i, s)| {
                s.es_type == "text"
                    && s.avg_len >= 80.0
                    && fields
                        .get(&s.name)
                        .map(|a| a.looks_natural_language())
                        .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect();
        let elected: Vec<usize> = if semantic_all {
            qualifying
                .into_iter()
                .filter(|&i| DOC_BODY_FIELDS.contains(&specs[i].name.as_str()))
                .collect()
        } else {
            qualifying
                .into_iter()
                .max_by(|&a, &b| specs[a].avg_len.partial_cmp(&specs[b].avg_len).unwrap())
                .into_iter()
                .collect()
        };
        for i in elected {
            let a = fields.get(&specs[i].name);
            specs[i].es_type = "semantic_text".into();
            specs[i].notes.push(format!(
                "hybrid lexical+vector body — elected because it looks like natural language \
                 (word_ratio {:.2}, {:.1} tokens/value); embedded server-side (lexical by \
                 default; Candle neural, proxy, or experimental ONNX if configured)",
                a.map(|x| x.word_ratio()).unwrap_or(0.0),
                a.map(|x| x.mean_tokens()).unwrap_or(0.0),
            ));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every election runs over a freshly built `HashMap`, and every fresh map
    /// gets its own random hash seed — so 500 in-process elections sample 500
    /// different iteration orders. `max_by_key` settled a tie by whichever of
    /// them came last.
    #[test]
    fn a_tied_date_encoding_election_elects_the_richest_encoding_every_run() {
        for _ in 0..500 {
            let hits: HashMap<DateEnc, u64> = [
                (DateEnc::SpaceNaive, 10),
                (DateEnc::IsoNaive, 10),
                (DateEnc::DateOnly, 10),
            ]
            .into_iter()
            .collect();
            assert_eq!(
                elect_date_enc(&hits),
                Some(DateEnc::IsoNaive),
                "of the tied encodings, iso-naive concedes the least"
            );

            let hits: HashMap<DateEnc, u64> = [(DateEnc::Clf, 4), (DateEnc::Rfc2822, 4)]
                .into_iter()
                .collect();
            assert_eq!(elect_date_enc(&hits), Some(DateEnc::Clf));

            let hits: HashMap<DateEnc, u64> = [(DateEnc::DateOnly, 9), (DateEnc::Rfc3339, 8)]
                .into_iter()
                .collect();
            assert_eq!(
                elect_date_enc(&hits),
                Some(DateEnc::DateOnly),
                "declaration order is only the tie-break; more evidence still wins"
            );

            assert_eq!(elect_date_enc(&HashMap::new()), None);
        }
    }

    /// The end-to-end symptom: one unchanged sample, split 50/50 between two
    /// string encodings, must always write the same `date_enc` into the spec —
    /// that string reaches the catalog and `coerce::plan_from_specs`.
    #[test]
    fn a_field_tied_between_two_date_encodings_maps_identically_every_run() {
        for _ in 0..200 {
            let mut acc = FieldAcc::default();
            for i in 0..10 {
                acc.add(&Value::String(format!("2026-03-{:02}T00:00:13", i + 1)));
                acc.add(&Value::String(format!("2026-03-{:02} 00:00:13", i + 1)));
            }
            assert_eq!(acc.date_hits[&DateEnc::IsoNaive], 10);
            assert_eq!(acc.date_hits[&DateEnc::SpaceNaive], 10);

            let mut fields = HashMap::new();
            fields.insert("ts".to_string(), acc);
            let specs = infer_fields(&fields, 20, true);
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].es_type, "date");
            assert_eq!(
                specs[0].date_enc.as_deref(),
                Some(DateEnc::IsoNaive.as_str())
            );
            assert_eq!(
                specs[0].date_evidence,
                [
                    "iso-naive (assumed UTC): 10".to_string(),
                    "yyyy-mm-dd hh:mm:ss (assumed UTC): 10".to_string(),
                ],
                "the elected encoding is the first of the tied ones listed"
            );
        }
    }

    #[test]
    fn a_tied_entity_election_elects_the_most_specific_entity_every_run() {
        for _ in 0..500 {
            let votes: HashMap<Entity, u64> = [
                (Entity::Url, 6),
                (Entity::Uuid, 6),
                (Entity::Ip, 6),
                (Entity::Email, 6),
            ]
            .into_iter()
            .collect();
            assert_eq!(
                elect_entity(&votes),
                Some(Entity::Ip),
                "an exact IpAddr parse is the most specific classifier"
            );

            let votes: HashMap<Entity, u64> =
                [(Entity::Url, 3), (Entity::Email, 3)].into_iter().collect();
            assert_eq!(elect_entity(&votes), Some(Entity::Email));

            let votes: HashMap<Entity, u64> =
                [(Entity::Url, 7), (Entity::Ip, 6)].into_iter().collect();
            assert_eq!(
                elect_entity(&votes),
                Some(Entity::Url),
                "precedence is only the tie-break; more values still wins"
            );

            assert_eq!(elect_entity(&HashMap::new()), None);
        }
    }

    /// #173: a document dataset merges extractor vocabularies whose records
    /// each populate exactly one body carrier (`body` for code/prose, `text`
    /// for line chunks). The docs policy must elect EVERY qualifying carrier
    /// — electing only the longest silently cut whole format families out of
    /// the vector arm — while a non-carrier text field (`defs`) stays
    /// lexical, because it rides on the same record as a carrier and electing
    /// it would embed every code record twice.
    #[test]
    fn a_document_dataset_elects_every_body_carrier_and_only_the_carriers() {
        let prose = "The connection pool retries every failed handshake with backoff. \
                     Each worker owns one socket and never shares it across threads.";
        let mut fields = HashMap::new();
        for name in ["body", "text", "defs"] {
            let mut acc = FieldAcc::default();
            for _ in 0..20 {
                acc.add(&Value::String(prose.to_string()));
            }
            fields.insert(name.to_string(), acc);
        }

        // Data policy (semantic_all = false): at most one semantic field.
        let single = infer_fields_with_policy(&fields, 60, false, false);
        assert_eq!(
            single
                .iter()
                .filter(|s| s.es_type == "semantic_text")
                .count(),
            1,
            "{single:#?}"
        );

        // Docs policy: both carriers elected, the non-carrier stays text.
        let docs = infer_fields_with_policy(&fields, 60, false, true);
        let by_name = |n: &str| docs.iter().find(|s| s.name == n).unwrap();
        assert_eq!(by_name("body").es_type, "semantic_text", "{docs:#?}");
        assert_eq!(by_name("text").es_type, "semantic_text", "{docs:#?}");
        assert_eq!(by_name("defs").es_type, "text", "{docs:#?}");
    }

    /// Pins the reason the entity tie is latent rather than live: the ≥90%
    /// supermajority gate rejects any tied field, so the winner never reaches
    /// the spec. If this test starts failing because the gate was relaxed, the
    /// total ordering in `elect_entity` is the only thing keeping the mapping
    /// stable — do not remove it.
    #[test]
    fn a_tied_entity_field_is_rejected_by_the_supermajority_gate() {
        let mut acc = FieldAcc::default();
        for i in 0..15u32 {
            acc.add(&Value::String(format!("10.0.0.{i}")));
            acc.add(&Value::String(format!(
                "741e7b6b-dbd2-4a7f-93a9-4ba50fb561{i:02}"
            )));
        }
        assert_eq!(acc.entity[&Entity::Ip], 15);
        assert_eq!(acc.entity[&Entity::Uuid], 15);
        let mut fields = HashMap::new();
        fields.insert("addr".to_string(), acc);
        let specs = infer_fields(&fields, 30, true);
        assert_eq!(specs[0].semantic, None, "neither side holds 90% of 30");
    }
}
