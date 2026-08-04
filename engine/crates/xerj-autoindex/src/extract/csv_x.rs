//! CSV with sniffed dialect (delimiter / header / decimal-comma), streaming.

use super::{sanitize_field_name, ExtractStats, FieldOrigin, RawRecord, Sink};
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
            origin: FieldOrigin::Data,
        }) {
            break;
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sniff::{CsvDialect, Family};

    fn sniffed(delim: u8, has_header: bool, decimal_comma: bool) -> Sniffed {
        Sniffed {
            family: Family::Csv,
            gzip: false,
            binary_kind: None,
            csv: Some(CsvDialect {
                delim,
                has_header,
                decimal_comma,
            }),
            encoding: "utf-8",
        }
    }

    fn run(sn: &Sniffed, bytes: &[u8]) -> (Result<ExtractStats>, Vec<RawRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.csv");
        std::fs::write(&path, bytes).unwrap();
        let mut recs = Vec::new();
        let stats = extract(&path, sn, None, &mut |r| {
            recs.push(r);
            true
        });
        (stats, recs)
    }

    fn ok(sn: &Sniffed, bytes: &[u8]) -> (ExtractStats, Vec<RawRecord>) {
        let (stats, recs) = run(sn, bytes);
        (stats.unwrap(), recs)
    }

    #[test]
    fn the_header_row_names_the_fields_and_every_data_row_becomes_one_record() {
        let (stats, recs) = ok(
            &sniffed(b',', true, false),
            b"id,Full Name,amount\n1,Ann Lee,10.5\n2,Bob Ray,20\n",
        );
        assert_eq!(stats.records, 2);
        assert_eq!(stats.junk, 0);
        assert_eq!(recs[0].locator, "r0");
        assert_eq!(recs[1].locator, "r1");
        assert_eq!(recs[0].fields["id"], serde_json::json!("1"));
        assert_eq!(
            recs[0].fields["Full_Name"],
            serde_json::json!("Ann Lee"),
            "column names are sanitized, never guessed"
        );
        assert_eq!(recs[1].fields["amount"], serde_json::json!("20"));
        assert!(
            recs.iter().all(|r| r.group.is_none()),
            "a CSV file is one dataset"
        );
    }

    #[test]
    fn a_headerless_dialect_names_every_column_by_position() {
        let (stats, recs) = ok(&sniffed(b';', false, false), b"1;Ann;10\n2;Bob;20\n");
        assert_eq!(stats.records, 2, "the first row is data, not a header");
        assert_eq!(recs[0].fields["col_1"], serde_json::json!("1"));
        assert_eq!(recs[0].fields["col_2"], serde_json::json!("Ann"));
        assert_eq!(recs[0].fields["col_3"], serde_json::json!("10"));
    }

    #[test]
    fn a_decimal_comma_dialect_rewrites_only_whole_numeric_fields() {
        let (_, recs) = ok(
            &sniffed(b';', true, true),
            "id;amount;note\n1;10,5;Ann, Lee\n2;-3,25;a,b,c\n".as_bytes(),
        );
        assert_eq!(recs[0].fields["amount"], serde_json::json!("10.5"));
        assert_eq!(recs[1].fields["amount"], serde_json::json!("-3.25"));
        assert_eq!(
            recs[0].fields["note"],
            serde_json::json!("Ann, Lee"),
            "text that merely contains a comma must be left alone"
        );
        assert_eq!(recs[1].fields["note"], serde_json::json!("a,b,c"));
    }

    #[test]
    fn duplicate_header_names_are_disambiguated_instead_of_overwriting_each_other() {
        let (_, recs) = ok(&sniffed(b',', true, false), b"id,id,amount\n1,2,3\n");
        assert_eq!(recs[0].fields["id"], serde_json::json!("1"));
        assert_eq!(recs[0].fields["id_2"], serde_json::json!("2"));
        assert_eq!(recs[0].fields["amount"], serde_json::json!("3"));
    }

    #[test]
    fn quoted_fields_keep_embedded_delimiters_and_newlines() {
        let (stats, recs) = ok(
            &sniffed(b',', true, false),
            b"a,b\n\"x,y\",\"line1\nline2\"\n",
        );
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].fields["a"], serde_json::json!("x,y"));
        assert_eq!(recs[0].fields["b"], serde_json::json!("line1\nline2"));
    }

    #[test]
    fn ragged_rows_keep_their_extra_columns_and_all_blank_rows_are_dropped() {
        let (stats, recs) = ok(&sniffed(b',', true, false), b"a,b\n1,2,3,4\n,,\n5\n");
        assert_eq!(stats.records, 2, "the all-empty row yields no record");
        assert_eq!(stats.junk, 0, "a ragged row is not junk");
        assert_eq!(recs[0].fields["col_3"], serde_json::json!("3"));
        assert_eq!(recs[0].fields["col_4"], serde_json::json!("4"));
        assert_eq!(recs[1].fields["a"], serde_json::json!("5"));
        assert!(
            recs[1].fields.get("b").is_none(),
            "a short row omits the missing field rather than blanking it"
        );
        assert_eq!(
            recs[1].locator, "r2",
            "locators stay positional in the file, skipped rows included"
        );
    }

    #[test]
    fn an_undecodable_row_is_counted_as_junk_and_the_rest_of_the_file_still_extracts() {
        let mut bytes = b"a,b\n1,ok\n".to_vec();
        bytes.extend_from_slice(&[b'2', b',', 0xff, 0xfe, b'\n']);
        bytes.extend_from_slice(b"3,fine\n");
        let (stats, recs) = ok(&sniffed(b',', true, false), &bytes);
        assert_eq!(stats.records, 2);
        assert_eq!(stats.junk, 1);
        assert_eq!(recs[1].fields["b"], serde_json::json!("fine"));
    }

    /// The asymmetry is deliberate to record, not to endorse: a bad DATA row is
    /// junk-counted and skipped, but the header is read with `?`, so a header
    /// that will not decode fails the whole file. The caller junk-files it.
    #[test]
    fn an_undecodable_header_row_fails_the_file_rather_than_skipping_a_row() {
        let mut bytes = vec![b'a', b',', 0xff, 0xfe, b'\n'];
        bytes.extend_from_slice(b"1,2\n");
        let (stats, recs) = run(&sniffed(b',', true, false), &bytes);
        assert!(stats.is_err());
        assert!(recs.is_empty());
    }

    #[test]
    fn a_file_sniffed_without_a_dialect_falls_back_to_comma_with_a_header() {
        let mut sn = sniffed(b',', true, false);
        sn.csv = None;
        let (stats, recs) = ok(&sn, b"x,y\n1,2\n");
        assert_eq!(stats.records, 1);
        assert_eq!(recs[0].fields["x"], serde_json::json!("1"));
    }

    #[test]
    fn a_header_only_file_yields_no_records_and_no_junk() {
        let (stats, recs) = ok(&sniffed(b',', true, false), b"a,b\n");
        assert_eq!((stats.records, stats.junk), (0, 0));
        assert!(recs.is_empty());
    }
}
