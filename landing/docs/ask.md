# xerj ask: query a repo without cloning it

`xerj ask "<question>"`, or just `xerj "<question>"`, answers a question about an open-source project from a pre-indexed pack of that project, on your machine, and cites the file and line. Nothing is cloned or indexed locally. Every answer names the corpus, the pin and the licence. Needs v1.0.0-rc.97 or later.

HTML version: https://xerj.org/docs/ask

## First question (real output, rc.97, Linux x86_64, 2026-10-19, fresh $HOME)

```text
$ xerj "how does zstd pick a window size"
xerj hub: no node answers at http://localhost:9200; starting a local node on port 9300 (…)
xerj hub: node ready on port 9300 (pid 76)
xerj hub: pulling 'zstd' (20.1 MB) — one-time download, later questions answer locally
xerj hub: pulled zstd 01b7154f 21038204 → 8 indices, 7874 definitions
zstd  pin 2026-10-02 (18 days old)  mixed: BSD-3 (lib) / GPL-2.0 (cli)  zstd/lib/zstd.h:2487
ZSTDLIB_STATIC_API size_t ZSTD_DCtx_setMaxWindowSize(ZSTD_DCtx* dctx, size_t maxWindowSize);
  approach-only licence (hint only: BSD-3 OR GPL-2 dual (verify at pin)): read, do not copy
…
```

- Routing happens in the client, from per-corpus term sketches in the signed catalogue and the lockfiles of the current project.
- If nothing answers on `XERJ_URL` or `localhost:9200`, a node is started on a free port from 9300 up. `xerj hub stop` stops only that node.
- The pack comes from GitHub Releases; its ed25519 signature and SHA-256 are checked before extraction.

## Definition lookup

```text
$ xerj ask --def ZSTD_compressBound
zstd  …  zstd/lib/compress/zstd_compress.c:79
size_t ZSTD_compressBound(size_t srcSize)
answered from: local pack
real 0m0.030s
```

`--json` prints `{"corpus","pin_sha","pin_date","age_days","licence_spdx","licence_class","answered_from","mode","hits":[{"file","line","kind","text",…}],"truncated","warnings"}`. MCP tools: `xerj_hub_find`, `xerj_hub_def`, `xerj_hub_ask`.

## Options

`--corpus <slug>` skip routing · `--def <SYMBOL>` definition lookup · `--how` ranked passages · `--files` relevant files with licences · `--offline` cached catalogue and packs only · `--no-fetch` no download · `--hosted on|off|auto` may the question go to the hosted hub (default auto: off inside a git repository, on elsewhere; `xerj hub config hosted off` to keep it local) · `--max-tokens N` · `--json` · `--url <node>`.

## Exit codes

0 answered · 1 no match or nothing to answer from · 2 usage error or unknown corpus · 3 a pack failed verification · 4 more than one corpus matches (re-run with `--corpus`) · 5 this build has no hub public key (nothing downloaded).

## Packs

`xerj hub status | pull <slug> | sync [--install-timer] | stop`. `xerj ask` auto-downloads only packs of 50 MB or less and never an Excluded licence. Packs directory budget 4 GB, least recently used evicted first.

## Licences

adapt-with-attribution (permissive, public domain) or approach-only (GPL family: read, do not copy, warned on every hit). Closed standards are excluded from the hub.

## What you can ask today

14 corpora in the signed catalogue on 2026-10-18: act, age, agentops, aiohttp, alertmanager, arkworks-algebra, arroyo, astro, attack-mappings-explorer, autorag, avro, lz4, snappy, zstd. hub.xerj.org lists 513 live corpora; the rest are used through a manifest (`xerj corpus add`, `xerj code`), see https://xerj.org/docs/corpus-hub.

## When not to ask

Skip retrieval for facts the model can state exactly; on popular, well-memorised libraries it only adds cost.
