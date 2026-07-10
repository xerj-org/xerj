//! Client-side validation/coercion to the emitted mapping. The engine is
//! silently lenient (junk into typed fields → 201, verified) — so WE are the
//! type gate. Values that cannot be coerced are dropped from the record and
//! counted; the record itself still indexes.

use crate::infer::dates::{self, DateEnc};
use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Coerce {
    Date(Option<DateEnc>),
    Long,
    Double,
    Bool,
    Str,
}

pub fn enc_from_str(s: &str) -> Option<DateEnc> {
    for e in [
        DateEnc::Rfc3339,
        DateEnc::IsoNaive,
        DateEnc::SpaceNaive,
        DateEnc::DateOnly,
        DateEnc::Clf,
        DateEnc::Rfc2822,
        DateEnc::EpochMillis,
        DateEnc::EpochSeconds,
    ] {
        if e.as_str() == s {
            return Some(e);
        }
    }
    None
}

/// Build the coercion plan from the (serialized) field specs.
pub fn plan_from_specs(specs: &[crate::infer::FieldSpec]) -> HashMap<String, Coerce> {
    let mut plan = HashMap::new();
    for s in specs {
        let kind = match s.es_type.as_str() {
            "date" => Coerce::Date(s.date_enc.as_deref().and_then(enc_from_str)),
            "long" => Coerce::Long,
            "double" => Coerce::Double,
            "boolean" => Coerce::Bool,
            _ => Coerce::Str,
        };
        plan.insert(s.name.clone(), kind);
    }
    plan
}

/// Coerce in place; returns count of dropped (uncoercible) values.
pub fn coerce_record(fields: &mut Map<String, Value>, plan: &HashMap<String, Coerce>) -> u32 {
    let mut dropped = 0u32;
    let keys: Vec<String> = fields.keys().cloned().collect();
    for k in keys {
        let Some(kind) = plan.get(&k) else {
            continue; // unmapped overflow field — passes through
        };
        let v = fields.get(&k).unwrap().clone();
        let new = coerce_value(&v, kind);
        match new {
            Some(nv) => {
                fields.insert(k, nv);
            }
            None => {
                fields.remove(&k);
                dropped += 1;
            }
        }
    }
    dropped
}

fn coerce_value(v: &Value, kind: &Coerce) -> Option<Value> {
    if v.is_null() {
        return None;
    }
    if let Value::Array(a) = v {
        let out: Vec<Value> = a
            .iter()
            .filter_map(|e| coerce_value(e, kind))
            .collect();
        return if out.is_empty() {
            None
        } else {
            Some(Value::Array(out))
        };
    }
    match kind {
        Coerce::Date(enc) => dates::coerce_to_date(v, *enc).map(Value::String),
        Coerce::Long => match v {
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f.round() as i64))
                .map(|i| Value::Number(i.into())),
            Value::String(s) => {
                let t = s.trim();
                t.parse::<i64>()
                    .ok()
                    .or_else(|| t.parse::<f64>().ok().map(|f| f.round() as i64))
                    .map(|i| Value::Number(i.into()))
            }
            Value::Bool(b) => Some(Value::Number((*b as i64).into())),
            _ => None,
        },
        Coerce::Double => match v {
            Value::Number(n) => Some(Value::Number(n.clone())),
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            _ => None,
        },
        Coerce::Bool => match v {
            Value::Bool(b) => Some(Value::Bool(*b)),
            Value::String(s) => match s.trim() {
                "true" | "TRUE" | "True" => Some(Value::Bool(true)),
                "false" | "FALSE" | "False" => Some(Value::Bool(false)),
                _ => None,
            },
            Value::Number(n) => match n.as_i64() {
                Some(0) => Some(Value::Bool(false)),
                Some(1) => Some(Value::Bool(true)),
                _ => None,
            },
            _ => None,
        },
        Coerce::Str => match v {
            Value::String(s) => Some(Value::String(s.clone())),
            Value::Number(n) => Some(Value::String(n.to_string())),
            Value::Bool(b) => Some(Value::String(b.to_string())),
            Value::Object(m) => Some(Value::String(
                serde_json::to_string(m).unwrap_or_default(),
            )),
            _ => None,
        },
    }
}
