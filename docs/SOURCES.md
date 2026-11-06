# Sources: how to get data into XERJ

Every way to get data into XERJ, one command each. `xerj sources` prints the
same list from the binary (`xerj sources --json` for tools), and a test keeps
this page and that list in step.

What this page covers: rc.101 and later. Rows marked **In progress** are being
built and are not in a released binary yet; they say what to do today instead.

## Available

| Source | Command | Notes |
|---|---|---|
| Files and folders | `xerj autoindex <folder>` | PDF, DOCX/DOC, PPTX, EPUB, HTML, text, man pages, notebooks, CSV, XLS/XLSX, JSON, JSONL, YAML, XML, email (mbox, EML), source code, logs, agent session stores. Text extraction only, no OCR. |
| Folder, kept current | `xerj autoindex <folder> --watch` | Follows file changes in the working tree. |
| Archives (zip, tar, tar.gz, tar.zst) | `xerj autoindex <file.zip>` | Built for Google Takeout downloads. See [Archives](#archives). |
| SQLite database file | `xerj autoindex <folder with app.sqlite>` | Opened read-only, one dataset per table. A file snapshot: re-run, or use `--watch`, to pick up changes. |
| SQL dump file | `xerj autoindex <folder with dump.sql>` | MySQL and Postgres `INSERT` rows, streamed, one dataset per table. Postgres `COPY … FROM stdin` rows are not read yet. |
| S3, R2, MinIO | `xerj autoindex s3://bucket/prefix [--watch]` | Add `--endpoint-url <URL>` for MinIO and other S3 gateways; `r2://` needs the account endpoint. |
| Any JSON, any Elasticsearch client | `POST /<index>/_bulk` | Elasticsearch 8.x compatible bulk and index API. You shape the documents. |
| Vectors you already have | `PUT /<index> dense_vector mapping, then POST /<index>/_bulk` | Export from Pinecone, Qdrant, Weaviate, Chroma or pgvector and post the vectors with their payload. kNN and hybrid queries then work. Dimensions must match the mapping. |
| Agent memory | `xerj memory / MCP xerj_memory_* tools` | Store, recall and forget notes for agents. Recall is lexical today. |
| Facts for XERJ Query-1 | `POST /_facts/<ns>/_bulk` | Needs a node started with `xerj --facts-model-dir <DIR>`. Give each subject a `subject_id`; counting needs it. See [FACTS_API.md](FACTS_API.md). |
| Hub corpus packs | `xerj hub` | Signed record packs from the corpus hub. |

Verify any folder run:

```sh
xerj autoindex ./my-folder
# expect a summary ending with the datasets created, e.g. "ax-<name>: N docs"
xerj search "<a word you know is in there>"
# expect hits with file paths
```

## In progress

| Source | Command | Today, instead |
|---|---|---|
| Live Postgres / MySQL | `xerj autoindex postgres://... or mysql://...` | Index a dump file, or run the Postgres CDC example in [case-studies/daily-dev-postgres-cdc](case-studies/daily-dev-postgres-cdc/). |
| Vector DB importers (Qdrant, Pinecone, Chroma, pgvector) | `xerj import <vendor> ...` | Export your vectors and post them through `_bulk` with a `dense_vector` mapping. |
| Table rows as facts | `rows become facts with subject_id = primary key` | Write facts with `/_facts/<ns>/_bulk`, one `subject_id` per row. |

These have no release date. They will move to the table above when a released
binary has them.

## Archives

`xerj autoindex takeout.zip` extracts the archive once into a folder under the
run's state directory and indexes that folder. Running it again on the same,
unchanged archive reuses the extracted copy, so it behaves like a re-run over a
folder.

Extraction is bounded, because an archive is untrusted input:

- members with absolute paths, `..` components or drive prefixes are skipped;
- symlinks, hard links and device entries are skipped, never created;
- caps on what is actually written, not on what the headers claim:
  64 GiB in total (`XERJ_ARCHIVE_MAX_BYTES`), 16 GiB per member
  (`XERJ_ARCHIVE_MAX_ENTRY_BYTES`), 2,000,000 members
  (`XERJ_ARCHIVE_MAX_ENTRIES`), and an expansion ratio of 200× the archive's
  size once more than 1 GiB has been written (`XERJ_ARCHIVE_MAX_RATIO`), the
  zip-bomb guard. Hitting a cap stops the run and names the variable to raise.

Archives found *inside* a folder run are not opened; the run lists them with the
command to index each one on its own. 7z, rar and xz archives are not opened at
all: extract them first.

Verify:

```sh
xerj autoindex ./takeout-20261105.zip
# expect: "extracted zip archive …: N file(s), … bytes into …; skipped 0 unsafe path(s) …"
```
