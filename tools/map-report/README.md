# Share an offline data map

`xerj autoindex map` can export the selected node catalog as one offline HTML
file or a Markdown report. It reads the node; it never indexes, edits or
removes source data. The exported HTML has inline CSS, no JavaScript, remote
fonts or CDN dependencies. Opening either report does not need the node.

For an already indexed prefix, choose a new output filename:

```sh
xerj autoindex map --prefix myproject --html xerj-map.html --redact-paths
xerj autoindex map --prefix myproject --md xerj-map.md --redact-paths
```

Use `--url` and, when needed, `--api-key` for the original node, just as with
the ordinary catalog map. Exporting requires a running node and read access
to its catalog and selected datasets; graph enrichment also needs access to
the recorded brain. There are no additional external services.

Existing outputs, directories and output symlinks are refused. A report is
staged next to its destination and published only after rendering succeeds.
On a missing/unreadable catalog, no report is published. After a successful
write, stdout says `report: <path>`. One invocation writes one format;
`--json`, `--offset`, `--limit`, and a positional folder cannot be combined
with report output. `--dataset` and `--prefix` retain their catalog filters.
Plain map and `--json` output are unchanged when no report flag is given.

## What is in a report

- Datasets, catalog record/file counts, and format labels. Format counts are
  datasets containing that format, not counts of files of that type. Counts
  across datasets need not represent unique files or unique source records.
- Up to ten symbol names per dataset, for the first twenty catalog datasets,
  ranked by indexed definition-record frequency. These are not call counts
  or a measure of importance. Unsupported name mappings are not aggregated.
- Up to ten incoming graph hubs per brain, for at most five brains recorded
  in the latest run of each selected prefix,
  resolved to file paths only inside the selected dataset indices. Counts
  belong to individual nodes, not every record from a file. Unresolved nodes
  are omitted and counted in a note; this is not an exhaustive file-degree
  ranking. With `--dataset`, the graph's top nodes can fall outside the
  selection. Hydration is restricted to datasets of prefixes recorded for
  that brain, so another prefix cannot supply a same-ID file's path. Graph
  data is a deterministic link index. An older graph run is not used when a
  newer run for that prefix has no graph metadata.
- Skipped/junk/refused files and their catalog reasons, capped at 300
  characters per value. Duplicate files are not described as failures.
- Five suggested lexical searches, preferring observed symbol names. Generic
  fallback terms are used when there are fewer symbols. Suggestions have not
  been executed and may return no matches. On a multi-prefix map they target
  the first recorded matching prefix alphabetically. The exact commands use
  POSIX shell quoting and the search client's endpoint/auth configuration;
  set `XERJ_URL` to the source node when the default is not appropriate.

The report inherits the current map fetch limits: at most 500 dataset rows,
500 skipped-file rows and 500 catalog run records. It labels these limits
instead of claiming a full filesystem inventory. Enrichment is best effort:
mapping, symbol-count or graph read failures are shown as unavailable; they
are not proof of an empty corpus. The ordinary map's required catalog reads
still fail normally. `--no-graph` indexed corpora work without graph calls;
their reports say that graph metadata is absent.

## Review before sharing

The default report contains file paths, dataset/index/format and symbol
names, and skip reasons. Reasons can contain short source excerpts or
sensitive metadata. It does not deliberately export full document bodies,
configured API credentials or node URLs. `--redact-paths` hides the full
file-path values and omits skip reasons, including paths embedded in them.
It does **not** anonymize names of datasets, indices, formats, brains or
symbols. Review the resulting report before publishing it.

All variable text is escaped in both formats. The HTML adds a restrictive
content security policy; no catalog text becomes active markup. The node is
only contacted while generating the report, never by the exported page.

## Example and reproduction

Open the [Markdown example](sample.md), or download [the HTML example](sample.html)
and open it in a browser. Both were generated from the committed
[synthetic parcel-ledger fixture](fixture/README.md), with paths redacted.
Their counts are fixture observations, not a performance or quality benchmark.

From repository root, after a scoped binary build, this harness starts its
own temporary lexical node on a random loopback port, copies the fixture,
adds one unsupported binary file, runs dry-run estimates, and indexes it
under separate graph/no-graph prefixes. It uses a 99% disk flood watermark
only for the tiny temporary fixture on a shared development disk. No model,
personal corpus or pre-existing node is used. It removes its temporary data
when finished.

```sh
python3 tools/map-report/smoke.py --binary engine/target/ci-test/xerj
```

The harness checks both output formats, symbols, graph/no-graph notes,
redaction, no-clobber refusals, empty-catalog refusal, and byte-identical
ordinary map/JSON output before and after exports. `--samples-dir <directory>`
writes regenerated redacted samples there. `--runner <es-yaml-runner>` also
runs the compatibility suite on the same executable snapshot first.

[Validation results and review boundary](VERIFICATION.md).
