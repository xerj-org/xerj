//! CSV with sniffed dialect (delimiter / header / decimal-comma), streaming.

use super::{sanitize_field_name, ExtractStats, RawRecord, Sink};
use crate::sniff::Sniffed;
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::Path;

pub fn extract(
    path: &Path,
    sn: &Sniffed,
    limit_bytes: Option<u64>,
    sink: Sink,
) -> Result<ExtractStats> {
    let dialect = sn.csv.unwrap_or(crate::sniff::CsvDialect {
        delim: b',',
        has_header: true,
        decimal_comma: false,
    });
    let r = super::open_reader(path, sn.gzip, limit_bytes)?;
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(dialect.delim)
        .has_headers(dialect.has_header)
        .flexible(true)
        .from_reader(r);
    let headers: Vec<String> = if dialect.has_header {
        let h = rdr.headers()?.clone();
        let mut seen = std::collections::HashSet::new();
        h.iter()
            .map(|f| {
                let mut name = sanitize_field_name(f);
                while !seen.insert(name.clone()) {
                    name.push('_');
                    name.push('2');
                }
                name
            })
            .collect()
    } else {
        Vec::new()
    };
    let decimal_comma_re = regex::Regex::new(r"^-?\d{1,12},\d+$").unwrap();
    let mut stats = ExtractStats::default();
    for (i, rec) in rdr.into_records().enumerate() {
        let rec = match rec {
            Ok(r) => r,
            Err(_) => {
                stats.junk += 1;
                continue;
            }
        };
        let mut fields = Map::new();
        for (j, val) in rec.iter().enumerate() {
            let name = if j < headers.len() {
                headers[j].clone()
            } else {
                format!("col_{}", j + 1)
            };
            let mut v = val.trim().to_string();
            if v.is_empty() {
                continue;
            }
            if dialect.decimal_comma && decimal_comma_re.is_match(&v) {
                v = v.replace(',', ".");
            }
            fields.insert(name, Value::String(v));
        }
        if fields.is_empty() {
            continue;
        }
        stats.records += 1;
        if !sink(RawRecord {
            fields,
            locator: format!("r{i}"),
            group: None,
        }) {
            break;
        }
    }
    Ok(stats)
}
