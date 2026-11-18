# Agent-query captures — 2026-10-20

Raw outputs behind the `/answers/` pages written from 1,000 agent queries.
Every figure on those pages comes from a file in this folder. Nothing here
was rounded.

## Machine

| | |
|---|---|
| OS | Linux 6.12.94+, x86_64, 4 CPUs (`00-environment.txt`) |
| RAM | MemTotal 16,387,280 kB |
| Binary | `/workspace/engine/target/release/xerj`, `xerj v1.0.0-rc.97` (`00-version.txt`) |
| Build | `cargo +stable build --release -j 2 -p xerj-server` in `engine/` |
| Node | `--insecure --data-dir /tmp/xerj-aq-2026-10-20/node --bind 127.0.0.1 --port 9217` |
| Cap | `XERJ_MAX_PROCESS_MEMORY_MB=4096` |

The node log is `00-node.log`. Issue and pull-request bodies were not
committed. `gh` wrote them under `/tmp/xerj-aq-2026-10-20/`. `01-export-stats.txt`
is the size record: 532 issues, 1,000 pull requests (`--limit 1000` returned
1,000).

## Two passes

`run.py` is the first driver. It queried an index name `gh-issues` and a field
`text` that this autoindex did not create. Those responses are the 404s and
the 0-hit files (`03-verify-output.jsonl`, `04-batch-terms.json`,
`04-fuzzy.json`, `04-msearch-named.txt`, `06-histogram.json`, `07-search.json`).
They stay, because they are what a guessed name does.

`08-*` are the corrected queries, same node and same data directory, after
`xerj autoindex map` named `gh-jsonl`, `gh-json`, `docs-docs`, `sessions-jsonl`,
`sessions-docs` and `epoch-jsonl`.

## Replays the same day

The histogram request bodies were not stored in `08-histogram*.json` (those
files are the responses). Replayed against the still-running node:

- `match_phrase` on `title` `autoindex`, `date_histogram` month on `createdAt`:
  buckets matched `08-histogram-phrase.json`. First-run `took` is 123. The
  warm replay `took` was 0.
- `match` on `title` `autoindex`, same histogram: buckets matched
  `08-histogram.json`. First-run `took` is 630. The warm replay `took` was 0.
- `match_all` plus `date_histogram` on `epoch-jsonl` field `ts`: the response
  bytes matched `08-epoch-histogram-all.json` (`took` 0, 40 docs, `buckets: []`).
- `fixtures/msearch-named.ndjson` (issue numbers 71–76) returned the same
  response bytes as `08-msearch-named.txt`. The original request body was not
  in that file. The page cites the response, and the command is this replay.

Published times are the first-run files, not the warm replay.

## Fixtures

`fixtures/sessions/` is three made-up JSONL files in the Claude Code session
shape. No `~/.claude` directory existed on this machine. `fixtures/epoch/epoch.jsonl`
is 40 rows with fractional epoch strings in `ts`.

## Redaction

Three private values were removed before this folder was committed. The
measured numbers on the answer pages still come from the capture files named
in each page's evidence.

- `00-node.log`: the first-launch console setup token was replaced with
  `REDACTED`.
- `00-environment.txt`: the `uname` hostname was replaced with `REDACTED`.
  The kernel version is unchanged.
- `01-gh-search.txt`: four Windows home paths, two usernames, were replaced
  with `<redacted-home-path>`. The file is 36,426 bytes after that.
  `01-byte-compare.txt` still records the original capture size, 36,550 bytes,
  which is the figure the answer page cites.

`/workspace` and `/tmp/xerj-aq-2026-10-20` are the capture's checkout and temp
directories. They contain no username, and the answer pages quote those
commands, so they stay. Loopback addresses stay. No email address, no
non-loopback IP, and no machine serial was in these files.
