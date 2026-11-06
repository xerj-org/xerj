//! `xerj sources`: every way to get data into XERJ, one command each.
//!
//! One registry feeds the CLI table and is checked against `docs/SOURCES.md`
//! by a test, so the page and the binary cannot drift apart. A row says
//! `available` only for what this binary does today; work being built is
//! `in progress`, with no date.

/// Whether a source works in this binary or is being built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Available,
    InProgress,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Available => "available",
            Status::InProgress => "in progress",
        }
    }
}

/// One ingest source.
#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub name: &'static str,
    pub command: &'static str,
    pub status: Status,
    pub note: &'static str,
}

/// The registry, in the order the table prints.
pub const SOURCES: &[Source] = &[
    Source {
        name: "Files and folders",
        command: "xerj autoindex <folder>",
        status: Status::Available,
        note: "PDF, Office, HTML, EPUB, text, CSV/Excel, JSON/JSONL, YAML, XML, email, code, logs",
    },
    Source {
        name: "Folder, kept current",
        command: "xerj autoindex <folder> --watch",
        status: Status::Available,
        note: "follows file changes in the working tree",
    },
    Source {
        name: "Archives (zip, tar, tar.gz, tar.zst)",
        command: "xerj autoindex <file.zip>",
        status: Status::Available,
        note: "Google Takeout zips; extracted once, with size and path checks",
    },
    Source {
        name: "SQLite database file",
        command: "xerj autoindex <folder with app.sqlite>",
        status: Status::Available,
        note: "read-only, one dataset per table",
    },
    Source {
        name: "SQL dump file",
        command: "xerj autoindex <folder with dump.sql>",
        status: Status::Available,
        note: "INSERT rows, one dataset per table; Postgres COPY rows are not read",
    },
    Source {
        name: "S3, R2, MinIO",
        command: "xerj autoindex s3://bucket/prefix [--watch]",
        status: Status::Available,
        note: "--endpoint-url for MinIO and other S3 gateways",
    },
    Source {
        name: "Any JSON, any Elasticsearch client",
        command: "POST /<index>/_bulk",
        status: Status::Available,
        note: "Elasticsearch 8.x compatible bulk and index API",
    },
    Source {
        name: "Vectors you already have",
        command: "PUT /<index> dense_vector mapping, then POST /<index>/_bulk",
        status: Status::Available,
        note: "export from your vector DB and post vectors with their payload",
    },
    Source {
        name: "Agent memory",
        command: "xerj memory / MCP xerj_memory_* tools",
        status: Status::Available,
        note: "lexical recall today",
    },
    Source {
        name: "Facts for XERJ Query-1",
        command: "POST /_facts/<ns>/_bulk",
        status: Status::Available,
        note: "needs xerj --facts-model-dir <DIR>; give each subject a subject_id",
    },
    Source {
        name: "Hub corpus packs",
        command: "xerj hub",
        status: Status::Available,
        note: "signed record packs",
    },
    Source {
        name: "Live Postgres / MySQL",
        command: "xerj autoindex postgres://... or mysql://...",
        status: Status::InProgress,
        note: "today: a dump file, or the Postgres CDC example in docs/case-studies",
    },
    Source {
        name: "Vector DB importers (Qdrant, Pinecone, Chroma, pgvector)",
        command: "xerj import <vendor> ...",
        status: Status::InProgress,
        note: "today: export and post through _bulk",
    },
    Source {
        name: "Table rows as facts",
        command: "rows become facts with subject_id = primary key",
        status: Status::InProgress,
        note: "today: write facts with /_facts/<ns>/_bulk",
    },
];

/// The table `xerj sources` prints.
pub fn render() -> String {
    let w_name = SOURCES
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0);
    let w_cmd = SOURCES
        .iter()
        .map(|s| s.command.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    out.push_str("Ways to get data into XERJ (docs: https://github.com/xerj-org/xerj/blob/main/docs/SOURCES.md)\n\n");
    for status in [Status::Available, Status::InProgress] {
        out.push_str(match status {
            Status::Available => "Available in this binary:\n",
            Status::InProgress => "\nIn progress (not in this binary yet):\n",
        });
        for s in SOURCES.iter().filter(|s| s.status == status) {
            out.push_str(&format!(
                "  {:<w_name$}  {:<w_cmd$}  {}\n",
                s.name, s.command, s.note
            ));
        }
    }
    out
}

/// `xerj sources [--json]`.
pub fn run_cli(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => {
            print!("{}", render());
            0
        }
        Some("--json") => {
            let rows: Vec<serde_json::Value> = SOURCES
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "source": s.name,
                        "command": s.command,
                        "status": s.status.as_str(),
                        "note": s.note,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&rows).unwrap_or_default()
            );
            0
        }
        Some("-h") | Some("--help") => {
            println!("usage: xerj sources [--json]\n\nLists every way to get data into XERJ, one command each.");
            0
        }
        Some(other) => {
            eprintln!("error: unknown argument {other:?}. Use `xerj sources --help` for usage.");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_lists_every_row() {
        let t = render();
        for s in SOURCES {
            assert!(t.contains(s.command), "{}", s.command);
        }
        assert!(t.contains("In progress"));
    }

    /// docs/SOURCES.md is the human page for the same registry: every command
    /// the CLI prints must appear there, and in-progress rows must say so.
    #[test]
    fn the_docs_page_matches_the_registry() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/SOURCES.md"),
        )
        .expect("docs/SOURCES.md exists");
        for s in SOURCES {
            assert!(
                page.contains(s.command),
                "docs/SOURCES.md is missing `{}`",
                s.command
            );
        }
        assert!(
            page.contains("In progress"),
            "docs/SOURCES.md must mark in-progress sources"
        );
    }
}
