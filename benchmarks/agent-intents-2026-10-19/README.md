# Agent-intent answer pages: captures, 2026-10-19

Raw output behind the five answer pages published from this run on 2026-10-19.
Every number on those pages is in a file here or in the timing table below.

The sixth capture, structured data from a saved web page into a JSON schema,
is not a page. That feature is still planned (issue #1797). Its files stay
here (`02-claims.json`, `02-verify.sh`, `02-verify-output.jsonl`) so the
58 ms field check is not detached from the run.

Environment: xerj v1.0.0-rc.97 installed with `curl -fsSL https://xerj.org/get | sh`
into a scratch HOME; ripgrep 14.1.1; an 8-core Linux box (16 GB RAM). Node started
with `xerj --insecure --data-dir <dir>` on localhost:9200. `xerj ask` runs used a
second scratch HOME and its automatic node on port 9300. Home paths are shortened
to `~`; nothing else was edited. `XERJ_DISABLE_FEEDBACK=true` was set.

| File | Page | Command |
| --- | --- | --- |
| 00-mcp-tools-list.json | local-code-search… | MCP `tools/list` against `xerj mcp` (17 tools) |
| 01-autoindex-requests.txt | local-code-search… | `xerj autoindex ./requests --prefix req` (psf/requests 611c6162) |
| 01-xerj-search.txt | local-code-search… | `xerj search --prefix req -k 3 --full 400 "rebuild auth on redirect strip authorization"` (1,303 B) |
| 01-xerj-search-first-phrasing.txt | local-code-search… | same, "strip the Authorization header when a redirect changes host" |
| 01-xerj-def.txt | local-code-search… | `xerj def --prefix req should_strip_auth` (949 B) |
| 01-rg-redirect.txt | local-code-search… | `rg -n -i redirect requests` (282 lines, 25,150 B) |
| 02-claims.json, 02-verify.sh, 02-verify-output.jsonl | extract-structured… | field check over 4 saved pages |
| 03-autoindex-jsonl-csv.txt | full-text-search… | `xerj autoindex ./scraped --prefix scr` |
| 03-csv-match.json, 03-jsonl-multi-match.json | full-text-search… | the two `_search` bodies on the page |
| 03-xerj-search-cli.txt | full-text-search… | `xerj search --prefix scr -k 3` on the same folder |
| 04-ask-snappy-cold.txt | ask-questions… | `xerj ask "how does snappy decompress a literal"`, fresh HOME |
| 04-ask-zstd-def.txt / .json | ask-questions… | `xerj ask --corpus zstd --def ZSTD_CLEVEL_DEFAULT [--json]` |
| 05-ids.txt, 05-batch.sh, 05-batch-output.txt | batch-lookup… | one `_search` over the rust-vulns pack |
| 05-xerj-code-exact-id.txt | batch-lookup… | `xerj code rust-vulns "CVE-2021-25900"` (wrong advisory first, #1788) |
| 06-*.txt | retrieval-that-says… | the four `xerj ask` outcomes |

## Timings (wall clock, `date +%s%N` around the command)

| Command | Result |
| --- | --- |
| `xerj search` above, 5 runs, session 1 | 8 to 12 ms |
| same, 5 runs, session 2 | 19 to 23 ms |
| `rg -n -i redirect`, 5 runs per session | 5 to 7 ms |
| autoindex psf/requests | done in 2.9s, 95 files, 36 code files, exit 3 (1 skipped file) |
| autoindex JSONL/CSV folder (6,444,108 B) | done in 12.4s, 2902 documents |
| autoindex 4 saved HTML pages | 0.9 s, 20 documents |
| field check, 5 fields (`02-verify.sh`) | 58 ms |
| `xerj ask` snappy, fresh HOME (node start + 10.8 MB pull) | 2,356 ms |
| `xerj ask` snappy, second run | 341 ms |
| `xerj ask` snappy, later runs | 30, 34, 35, 40 ms |
| `xerj ask --corpus zstd --def`, first (20.1 MB pull) | 962 ms |
| same with `--json`, cached | 20 ms |
| `xerj ask --corpus lz4 --def LZ4_compress_default`, cached | 20 ms |
| `xerj ask --corpus lz4 --def LZ4_DOES_NOT_EXIST_XYZ` | 20 ms cached, exit 1 |
| `xerj ask "what is the capital of australia"` | 14 ms, exit 1 |
| `xerj ask --corpus lz4 "kubernetes pod eviction policy"` | 485 ms first run (pulled lz4), exit 0 |
| `xerj corpus add rust-vulns … --verify-sig` | about 1.3 s, signature verified |
| `xerj corpus index rust-vulns` | 9,769 ms, 2,033 advisories |
| `05-batch.sh`, 6 IDs | 15 ms |

## Defects seen while capturing (not filed yet)

- `_msearch` with a wildcard index (`xc-rust-vulns-*`) returns 404 `index_not_found`.
- `_search` on a mid-name wildcard (`xc-rust-vulns-*-rustsec`) returns 0 hits and no error.
- `xerj search` over JSONL cites two different documents as `xerj-prs.jsonl:1`.
- `xerj ask` cites Markdown files as line 0 (the `xerj code` fix in #1908 is after rc.97).
- `highlight` in a `_search` body returned nothing.
