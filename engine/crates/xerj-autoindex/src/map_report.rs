//! Offline map exports (#2643). Read-only enrichment; render an allowlist of
//! metadata, never raw catalog JSON, document bodies, credentials or endpoints.
//!
//! Existing contracts used here: catalog.rs:245-279 (dataset metadata),
//! detect/mod.rs:158-170 (file-card graph anchors), and
//! xerj-api/src/graph_api.rs:1374-1397 (overview hubs). This client feature
//! does not link the engine or change its wire/storage contracts.

use crate::esclient::Es;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

const TOP: usize = 10;
const ENRICH_DATASETS: usize = 20;
const ENRICH_BRAINS: usize = 5;

#[derive(Debug, Clone, Copy)]
pub enum Format {
    Html,
    Markdown,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub path: PathBuf,
    pub format: Format,
    pub redact_paths: bool,
}

/// Stage beside the destination, before any node requests. Never overwrite
/// an existing file or follow an output symlink, including a dangling one.
pub(crate) fn prepare(output: &Output) -> Result<tempfile::NamedTempFile> {
    match output.path.symlink_metadata() {
        Ok(_) => bail!("report output already exists; choose a new --html/--md path"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("cannot inspect report output"),
    }
    let parent = output.path.parent().filter(|p| !p.as_os_str().is_empty());
    tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| std::path::Path::new(".")))
        .context("cannot stage report; choose an existing writable output directory")
}

#[derive(Debug, Default)]
struct Section {
    title: String,
    note: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Default)]
pub(crate) struct Report {
    sections: Vec<Section>,
    questions: Vec<(String, String)>,
    notes: Vec<String>,
}

fn text(v: &Value, field: &str) -> String {
    v.get(field).map_or_else(
        || "unknown".into(),
        |v| match v {
            Value::String(s) => clean(s),
            Value::Number(n) => n.to_string(),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .map(clean)
                .collect::<Vec<_>>()
                .join(", "),
            _ => "unknown".into(),
        },
    )
}

fn clean(s: &str) -> String {
    s.chars()
        .take(300)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Escape both HTML and GFM table metacharacters; no catalog value becomes
/// markup, a URL, a script, an image, or an executable command outside quotes.
fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        out.push_str(match c {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&#39;",
            '|' => "&#124;",
            '[' => "&#91;",
            ']' => "&#93;",
            '(' => "&#40;",
            ')' => "&#41;",
            '!' => "&#33;",
            '*' => "&#42;",
            '_' => "&#95;",
            '#' => "&#35;",
            '~' => "&#126;",
            ':' => "&#58;",
            '`' => "&#96;",
            '\\' => "&#92;",
            '\n' | '\r' => " ",
            _ => {
                out.push(c);
                continue;
            }
        });
    }
    out
}

impl Report {
    fn metadata(datasets: &[Value], junk: &[Value], redacted: bool) -> Self {
        let mut report = Self::default();
        report.notes.push(if redacted {
            "File paths and skip reasons are redacted. Dataset/index/format and symbol names remain; review these before sharing."
        } else {
            "Contains dataset/index/format names, symbol names, file paths and skip reasons. No full document bodies or configured credentials/endpoints are exported. Skip reasons can contain excerpts or sensitive metadata. Review before sharing, or use --redact-paths."
        }.into());
        report.notes.push("Catalog snapshot, not a live inventory: at most 500 datasets and 500 skipped-file entries. Counts are per dataset; files shared across datasets are not deduplicated. Lists and enrichments are bounded and may be incomplete.".into());
        report.sections.push(Section {
            title: "Datasets".into(),
            note: format!("{} catalog dataset entries shown.", datasets.len()),
            headers: ["Dataset", "Index", "Records", "Files", "Formats"]
                .map(String::from)
                .into(),
            rows: datasets
                .iter()
                .map(|d| {
                    [
                        "slug",
                        "index_name",
                        "record_count",
                        "file_count",
                        "formats",
                    ]
                    .map(|f| text(d, f))
                    .into()
                })
                .collect(),
        });
        let mut formats = BTreeMap::<String, usize>::new();
        for d in datasets {
            match d.get("formats") {
                Some(Value::Array(a)) => {
                    let unique: BTreeSet<_> = a.iter().filter_map(Value::as_str).collect();
                    for f in unique {
                        *formats.entry(clean(f)).or_default() += 1;
                    }
                }
                Some(Value::String(s)) => {
                    *formats.entry(clean(s)).or_default() += 1;
                }
                _ => {}
            }
        }
        report.sections.push(Section {
            title: "File families".into(),
            note: "Catalog format labels. A dataset can contain more than one format; these are dataset counts, not file counts.".into(),
            headers: ["Format", "Datasets containing format"].map(String::from).into(),
            rows: formats.into_iter().map(|(f, n)| vec![f, n.to_string()]).collect(),
        });
        report.sections.push(Section {
            title: "Skipped and refused files".into(),
            note: "Catalog exclusions include skipped, junk and refused files. An empty list does not prove every file was indexed.".into(),
            headers: ["Path", "Format", "Status", "Reason"].map(String::from).into(),
            rows: junk.iter().map(|f| vec![
                if redacted { "[path redacted]".into() } else { text(f, "path") },
                text(f, "format"), text(f, "status"),
                if redacted { "[reason omitted: may contain paths]".into() } else { text(f, "reason") },
            ]).collect(),
        });
        report
    }

    pub(crate) fn collect(
        es: &Es,
        runs: &[Value],
        datasets: &[Value],
        junk: &[Value],
        redacted: bool,
    ) -> Self {
        let mut report = Self::metadata(datasets, junk, redacted);
        let prefixes: BTreeSet<_> = runs
            .iter()
            .filter_map(|r| r.get("prefix").and_then(Value::as_str))
            .filter(|p| {
                datasets
                    .iter()
                    .any(|d| crate::catalog::dataset_matches_prefix(d, p))
            })
            .collect();
        let query_prefix = prefixes.first().copied();
        let mut symbols = Section {
            title: "Top symbols".into(),
            note: format!("Up to {TOP} names per dataset, for the first {ENRICH_DATASETS} datasets. Counts are indexed definition records, not calls or relevance. Only keyword-mapped name fields are aggregated."),
            headers: ["Index", "Symbol", "Definition records"].map(String::from).into(),
            ..Default::default()
        };
        let mut query_terms = Vec::new();
        for d in datasets.iter().take(ENRICH_DATASETS) {
            let Some(index) = d.get("index_name").and_then(Value::as_str) else {
                continue;
            };
            // Inspect the mapping before aggregating: user datasets can have
            // numeric or text `name` fields. Do not invent a keyword subfield.
            let Ok(mapping) = es.get_json(&format!("/{}/_mapping", enc(index))) else {
                report
                    .notes
                    .push(format!("Symbol mapping unavailable for {}.", clean(index)));
                continue;
            };
            let name = mapping
                .get(index)
                .and_then(|v| v.pointer("/mappings/properties/name"));
            let field = match name {
                Some(v) if v.get("type").and_then(Value::as_str) == Some("keyword") => "name",
                Some(v)
                    if v.pointer("/fields/keyword/type").and_then(Value::as_str)
                        == Some("keyword") =>
                {
                    "name.keyword"
                }
                _ => continue,
            };
            let response = es.search_without_savings(
                index,
                &json!({"size":0,"query":{"exists":{"field":"kind"}},
                "aggs":{"symbols":{"terms":{"field":field,"size":TOP}}}}),
            );
            let Ok(response) = response else {
                report
                    .notes
                    .push(format!("Symbol counts unavailable for {}.", clean(index)));
                continue;
            };
            let Some(buckets) = response
                .pointer("/aggregations/symbols/buckets")
                .and_then(Value::as_array)
            else {
                report
                    .notes
                    .push(format!("Symbol counts malformed for {}.", clean(index)));
                continue;
            };
            for bucket in buckets.iter().take(TOP) {
                let Some(name) = bucket.get("key").and_then(Value::as_str) else {
                    continue;
                };
                symbols
                    .rows
                    .push(vec![clean(index), clean(name), text(bucket, "doc_count")]);
                if !name.is_empty()
                    && name.chars().count() <= 160
                    && !name.chars().any(char::is_control)
                    && query_prefix.is_some_and(|p| crate::catalog::dataset_matches_prefix(d, p))
                {
                    query_terms.push(name.to_owned());
                }
            }
        }
        report.sections.push(symbols);
        // run_map supplies newest-first runs. A later --no-graph run must
        // not resurrect graph metadata from a superseded run of that prefix.
        let mut seen_prefixes = BTreeSet::new();
        let latest_runs: Vec<_> = runs
            .iter()
            .filter(|r| {
                r.get("prefix").and_then(Value::as_str).is_some_and(|p| {
                    datasets
                        .iter()
                        .any(|d| crate::catalog::dataset_matches_prefix(d, p))
                        && seen_prefixes.insert(p)
                })
            })
            .cloned()
            .collect();
        report.graph(es, &latest_runs, datasets, redacted);
        // Suggestions are lexical probes, not generated answers. They reuse
        // observed symbols where available; generic fallbacks are labelled.
        if let Some(prefix) = prefixes.first() {
            query_terms.extend(
                [
                    "configuration",
                    "error handling",
                    "data flow",
                    "validation",
                    "startup",
                ]
                .map(String::from),
            );
            let mut seen = BTreeSet::new();
            for term in query_terms
                .into_iter()
                .filter(|q| seen.insert(q.clone()))
                .take(5)
            {
                report.questions.push((
                    format!("Where is {term} described or implemented?"),
                    format!("xerj search --prefix {} {}", quote(prefix), quote(&term)),
                ));
            }
            report.notes.push("Suggested searches are unexecuted lexical probes, not guaranteed answers. Commands use your search client's endpoint/auth settings; select the original node with XERJ_URL if needed. On a multi-prefix map, suggestions target the first prefix alphabetically.".into());
        } else {
            report.notes.push("Search suggestions unavailable: no matching catalog run records a prefix. No prefix was guessed.".into());
        }
        report
    }

    fn graph(&mut self, es: &Es, runs: &[Value], datasets: &[Value], redacted: bool) {
        let mut brains = BTreeSet::new();
        for brain in runs
            .iter()
            .filter_map(|r| r.pointer("/graph/brain").and_then(Value::as_str))
        {
            if brains.contains(brain) {
                continue;
            }
            if brains.len() >= ENRICH_BRAINS {
                break;
            }
            brains.insert(brain);
            // IDs are only unique within an index namespace. Hydrate from
            // selected datasets owned by this brain's recorded prefixes;
            // another prefix's identical file must not supply the location.
            let indices: Vec<_> = datasets
                .iter()
                .filter(|d| {
                    runs.iter().any(|r| {
                        r.pointer("/graph/brain").and_then(Value::as_str) == Some(brain)
                            && r.get("prefix")
                                .and_then(Value::as_str)
                                .is_some_and(|p| crate::catalog::dataset_matches_prefix(d, p))
                    })
                })
                .filter_map(|d| d.get("index_name").and_then(Value::as_str))
                .collect();
            if crate::detect::validate_brain(brain).is_err() {
                self.notes
                    .push("Graph metadata contains an invalid brain name; omitted.".into());
                continue;
            }
            let response = es.get_json(&format!("/_graph/{}/overview?top={TOP}", enc(brain)));
            let mut section = Section {
                title: format!("Most-linked files — {}", clean(brain)),
                note: format!("Top {TOP} incoming graph nodes, hydrated only from the selected datasets. Edge counts belong to each node, not a sum over all records in a file. At most {ENRICH_BRAINS} brains are inspected. Relationships are deterministic links, not semantic understanding."),
                headers: ["File", "Incoming live edges"].map(String::from).into(),
                ..Default::default()
            };
            let hubs = response
                .as_ref()
                .ok()
                .and_then(|v| v.pointer("/hubs/in"))
                .and_then(Value::as_array);
            if let Some(hubs) = hubs {
                let ids: Vec<_> = hubs
                    .iter()
                    .take(TOP)
                    .filter_map(|h| h.get("id").and_then(Value::as_str))
                    .collect();
                let locations = if ids.is_empty() || indices.is_empty() {
                    None
                } else {
                    es.search_without_savings(
                        &indices.join(","),
                        &json!({"size":TOP,"query":{"ids":{"values":ids}},
                        "_source":["ax_path","path"]}),
                    )
                    .ok()
                };
                let mut paths = BTreeMap::new();
                if let Some(hits) = locations
                    .as_ref()
                    .and_then(|v| v.pointer("/hits/hits"))
                    .and_then(Value::as_array)
                {
                    for h in hits {
                        if let (Some(id), Some(path)) = (
                            h.get("_id").and_then(Value::as_str),
                            h.pointer("/_source/ax_path")
                                .or_else(|| h.pointer("/_source/path"))
                                .and_then(Value::as_str),
                        ) {
                            paths.insert(id, path);
                        }
                    }
                }
                let mut missing = 0;
                for hub in hubs.iter().take(TOP) {
                    if let Some(path) = hub
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| paths.get(id))
                    {
                        section.rows.push(vec![
                            if redacted {
                                "[path redacted]".into()
                            } else {
                                clean(path)
                            },
                            text(hub, "live_edges"),
                        ]);
                    } else {
                        missing += 1;
                    }
                }
                if missing > 0 {
                    section.note.push_str(&format!(" {missing} nodes could not be resolved to a file in this selection and are omitted."));
                }
            } else {
                section.note = "Graph overview unavailable or malformed; this is not evidence that the corpus has no links.".into();
            }
            self.sections.push(section);
        }
        if brains.is_empty() {
            self.notes.push("Graph sections omitted: the selected catalog runs contain no graph metadata (for example, indexed with --no-graph). No graph request was made.".into());
        }
    }

    pub(crate) fn publish(
        &self,
        mut staging: tempfile::NamedTempFile,
        output: &Output,
    ) -> Result<()> {
        let body = match output.format {
            Format::Html => self.html(),
            Format::Markdown => self.markdown(),
        };
        staging
            .write_all(body.as_bytes())
            .context("cannot write staged report")?;
        staging
            .as_file()
            .sync_all()
            .context("cannot sync staged report")?;
        // Reference: sessions.rs:166-170 uses the same no-clobber publish
        // contract. A failure leaves any previous destination untouched.
        staging.persist_noclobber(&output.path).map_err(|_| {
            anyhow::anyhow!(
                "cannot publish report: output exists or is not writable; choose a new path"
            )
        })?;
        Ok(())
    }

    pub(crate) fn markdown(&self) -> String {
        let mut out = String::from(
            "# XERJ data map\n\nAn offline inventory of the selected catalog datasets.\n\n",
        );
        for note in &self.notes {
            let _ = writeln!(out, "{}\n", escape(note));
        }
        for section in &self.sections {
            let _ = writeln!(
                out,
                "## {}\n\n{}\n",
                escape(&section.title),
                escape(&section.note)
            );
            if section.rows.is_empty() {
                out.push_str("No entries available.\n\n");
                continue;
            }
            let _ = writeln!(
                out,
                "| {} |",
                section
                    .headers
                    .iter()
                    .map(|s| escape(s))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            let _ = writeln!(
                out,
                "| {} |",
                vec!["---"; section.headers.len()].join(" | ")
            );
            for row in &section.rows {
                let _ = writeln!(
                    out,
                    "| {} |",
                    row.iter()
                        .map(|s| escape(s))
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
            out.push('\n');
        }
        out.push_str("## Questions to explore\n\nCommands below use POSIX shell quoting.\n\n");
        for (question, command) in &self.questions {
            let _ = writeln!(
                out,
                "{}\n\n<pre><code>{}</code></pre>\n",
                escape(question),
                escape(command)
            );
        }
        if out.ends_with("\n\n") {
            out.pop();
        }
        out
    }

    pub(crate) fn html(&self) -> String {
        let mut out = String::from("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><title>XERJ data map</title><style>");
        out.push_str(include_str!("map_report.css"));
        out.push_str("</style></head><body><main><header><p class=\"brand\">XERJ / Data map</p><h1>A map of what’s indexed.</h1><p class=\"intro\">An offline inventory of the selected catalog datasets.</p></header><aside aria-label=\"Scope and sharing notes\">");
        for note in &self.notes {
            let _ = write!(out, "<p>{}</p>", escape(note));
        }
        out.push_str("</aside>");
        for section in &self.sections {
            let _ = write!(
                out,
                "<section><h2>{}</h2><p>{}</p>",
                escape(&section.title),
                escape(&section.note)
            );
            if section.rows.is_empty() {
                out.push_str("<p class=\"empty\">No entries available.</p>");
            } else {
                out.push_str("<div class=\"table-wrap\" tabindex=\"0\"><table><thead><tr>");
                for h in &section.headers {
                    let _ = write!(out, "<th scope=\"col\">{}</th>", escape(h));
                }
                out.push_str("</tr></thead><tbody>");
                for row in &section.rows {
                    out.push_str("<tr>");
                    for cell in row {
                        let _ = write!(out, "<td>{}</td>", escape(cell));
                    }
                    out.push_str("</tr>");
                }
                out.push_str("</tbody></table></div>");
            }
            out.push_str("</section>");
        }
        out.push_str(
            "<section><h2>Questions to explore</h2><p>Commands use POSIX shell quoting.</p>",
        );
        for (q, command) in &self.questions {
            let _ = write!(
                out,
                "<h3>{}</h3><pre><code>{}</code></pre>",
                escape(q),
                escape(command)
            );
        }
        out.push_str("</section><footer>Generated by XERJ · Local metadata, ready to review and share.</footer></main></body></html>\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bounded read-only node: each unexpected endpoint/method fails the
    /// test. No corpus mutation, raw document body or external endpoint is
    /// needed to enrich a report.
    fn node(steps: Vec<(u16, &'static str, Value)>) -> (Es, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let join = std::thread::spawn(move || {
            for (status, expected, response) in steps {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "request missing: {expected}");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(expected), "{line}");
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                    if let Some(n) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if length > 0 {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    if request.pointer("/query/ids").is_some() {
                        assert_eq!(request["_source"], json!(["ax_path", "path"]));
                        assert!(request["size"].as_u64().unwrap() <= TOP as u64);
                    } else {
                        assert_eq!(request["size"], 0);
                    }
                }
                let body = response.to_string();
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (Es::new(&url, None).unwrap(), join)
    }

    #[test]
    fn report_enrichment_uses_bounded_metadata_reads_and_scoped_graph_locations() {
        let (es, join) = node(vec![
            (
                200,
                "GET /p-docs/_mapping",
                json!({"p-docs":{"mappings":{"properties":{"name":{"type":"keyword"}}}}}),
            ),
            (
                200,
                "POST /p-docs/_search",
                json!({"aggregations":{"symbols":{"buckets":[{"key":"Store","doc_count":2}]}}}),
            ),
            (
                200,
                "GET /q-docs/_mapping",
                json!({"q-docs":{"mappings":{"properties":{"name":{"type":"keyword"}}}}}),
            ),
            (
                200,
                "POST /q-docs/_search",
                json!({"aggregations":{"symbols":{"buckets":[{"key":"OtherCorpusSymbol","doc_count":20}]}}}),
            ),
            (
                200,
                "GET /_graph/p/overview?top=10",
                json!({"hubs":{"in":[{"id":"anchor","live_edges":4},{"id":"foreign","live_edges":9}]}}),
            ),
            (
                200,
                "POST /p-docs/_search",
                json!({"hits":{"hits":[{"_id":"anchor","_source":{"ax_path":"/private/project/store.rs"}}]}}),
            ),
        ]);
        let runs = [
            json!({"prefix":"p","graph":{"brain":"p"}}),
            json!({"prefix":"q"}),
        ];
        let datasets = [
            json!({"slug":"docs","index_name":"p-docs"}),
            json!({"slug":"docs","index_name":"q-docs"}),
        ];
        let report = Report::collect(&es, &runs, &datasets, &[], true);
        join.join().unwrap();
        let body = report.markdown();
        assert!(body.contains("Store"));
        assert!(body.contains("1 nodes could not be resolved"));
        assert!(body.contains("path redacted"));
        assert!(!body.contains("private/project"));
        assert_eq!(report.questions.len(), 5);
        assert_eq!(report.questions[0].1, "xerj search --prefix 'p' 'Store'");
        assert!(report
            .questions
            .iter()
            .all(|(_, command)| !command.contains("OtherCorpusSymbol")));
    }

    #[test]
    fn optional_read_failure_is_not_misreported_as_no_graph() {
        let (es, join) = node(vec![
            (
                403,
                "GET /p-docs/_mapping",
                json!({"error":"DENIED_SECRET"}),
            ),
            (
                403,
                "GET /_graph/p/overview?top=10",
                json!({"error":"DENIED_SECRET"}),
            ),
        ]);
        let report = Report::collect(
            &es,
            &[json!({"prefix":"p","graph":{"brain":"p"}})],
            &[json!({"slug":"docs","index_name":"p-docs"})],
            &[],
            false,
        );
        join.join().unwrap();
        let body = report.markdown();
        assert!(body.contains("Symbol mapping unavailable"));
        assert!(body.contains("Graph overview unavailable"));
        assert!(!body.contains("Graph sections omitted"));
        assert!(!body.contains("DENIED_SECRET"));
    }

    #[test]
    fn no_graph_catalog_does_not_probe_graph_or_guess_symbol_fields() {
        let (es, join) = node(vec![(
            200,
            "GET /p-docs/_mapping",
            json!({"p-docs":{"mappings":{"properties":{"name":{"type":"long"}}}}}),
        )]);
        let report = Report::collect(
            &es,
            &[
                json!({"prefix":"p"}),
                json!({"prefix":"p", "graph":{"brain":"old"}}),
            ],
            &[json!({"slug":"docs","index_name":"p-docs"})],
            &[],
            false,
        );
        join.join().unwrap();
        assert!(report.markdown().contains("Graph sections omitted"));
    }

    #[test]
    fn reports_escape_markup_and_redact_path_bearing_fields() {
        let ds = json!({"slug":"<script>alert(1)</script>|`x` ![remote](https://example.test/x)", "index_name":"p-docs", "record_count":3,"file_count":2,"formats":["rs","md"], "api_key":"SECRET_KEY", "body":"PRIVATE_FULL_BODY"});
        let junk = json!({"path":"/home/alice/private.rs", "format":"rs", "status":"junk", "reason":"failed at C:\\Users\\alice\\private.rs <img src=x>"});
        let raw = Report::metadata(
            std::slice::from_ref(&ds),
            std::slice::from_ref(&junk),
            false,
        );
        assert!(raw.html().contains("/home/alice/private.rs"));
        for body in [raw.html(), raw.markdown()] {
            assert!(!body.contains("<script>"));
            assert!(!body.contains("<img src=x>"));
            assert!(body.contains("&lt;script&gt;"));
            assert!(body.contains("&#124;"));
            assert!(!body.contains("![remote]"));
            assert!(!body.contains("https://"));
            assert!(!body.contains("SECRET_KEY"));
            assert!(!body.contains("PRIVATE_FULL_BODY"));
        }
        let private = Report::metadata(&[ds], &[junk], true);
        for body in [private.html(), private.markdown()] {
            assert!(!body.contains("alice"));
            assert!(!body.contains("private.rs"));
            assert!(body.contains("path redacted"));
        }
        assert!(!private.html().contains("<script"));
        assert!(!private.html().contains(" src="));
        assert!(!private.html().contains("href="));
    }

    #[test]
    fn report_publication_never_overwrites_and_cleans_staging_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let output = Output {
            path: dir.path().join("map.html"),
            format: Format::Html,
            redact_paths: false,
        };
        let stage = prepare(&output).unwrap();
        std::fs::write(&output.path, "existing").unwrap();
        assert!(Report::default().publish(stage, &output).is_err());
        assert_eq!(std::fs::read_to_string(&output.path).unwrap(), "existing");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(prepare(&output).is_err());
        std::fs::remove_file(&output.path).unwrap();
        let stage = prepare(&output).unwrap();
        Report::default().publish(stage, &output).unwrap();
        assert!(std::fs::read_to_string(&output.path)
            .unwrap()
            .starts_with("<!doctype html>"));
    }

    #[cfg(unix)]
    #[test]
    fn report_output_refuses_dangling_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.html");
        std::os::unix::fs::symlink(dir.path().join("missing"), &path).unwrap();
        assert!(prepare(&Output {
            path,
            format: Format::Html,
            redact_paths: false
        })
        .is_err());
    }
}
