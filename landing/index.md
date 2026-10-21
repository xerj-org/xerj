# XERJ: search for AI agents

A new way for AI to search data: give your coding agent one prompt, and it answers from a local index, citing the file and line.

HTML version: https://xerj.org/ · Agent guide: https://xerj.org/llms.txt

## Copy the prompt

Paste this into your coding agent:

```text
Install XERJ (docs: https://xerj.org/llms.txt), index this project's sources, and set up reference coding: clone and index the open-source repos closest to what we're building, and search how they solved a problem before writing code.
```

Or install it yourself:

```sh
curl -fsSL https://xerj.org/get | sh          # Linux, macOS; SHA-256 verified, fail-closed
irm https://xerj.org/get.ps1 | iex            # Windows PowerShell
```

## Real output (xerj v1.0.0-rc.97, Linux x86_64, 2026-10-19, fresh $HOME)

```text
$ xerj "how does lz4 frame format work"
xerj hub: no node answers at http://localhost:9200; starting a local node on port 9300 (data: ~/.xerj/hub/node, …)
xerj hub: node ready on port 9300 (pid 77)
xerj hub: pulling 'lz4' (4.4 MB) — one-time download, later questions answer locally
xerj hub: pulled lz4 0774d055 4623814 → 4 indices, 1702 definitions
…
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/lib/lz4frame.h:186
#define LZ4F_INIT_FRAMEINFO   { LZ4F_max64KB, LZ4F_blockLinked, … }    /* v1.8.3+ */
  approach-only licence (mixed: BSD-2 (lib) / GPL-2.0+ (rest)): read, do not copy
…
answered from: local pack
real 0m1.750s

$ xerj ask --def LZ4_compress_default
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/lib/lz4.c:1481
int LZ4_compress_default(const char* src, char* dst, int srcSize, int dstCapacity)
…
answered from: local pack
real 0m0.022s
```

`…` marks cut passages.

## Three ways in

1. **Ask a library**: `xerj "how does zstd pick a window size"`, `xerj ask --def ZSTD_compressBound`. Guide: https://xerj.org/docs/ask
2. **Search your files**: `xerj --insecure --data-dir ~/xerj-data &`, then `xerj autoindex ~/my-project` and `xerj search "<words>"`. Guide: https://xerj.org/docs/recipes/zero-config-autoindex
3. **Connect over MCP**: 17 tools; `xerj init` registers `xerj mcp` for Claude Code and Cursor. Guide: https://xerj.org/for-agents

## Proof, with sources

| Number | What | Source |
|---|---|---|
| 2.7× | fewer output tokens than a grep-driven agent, same 16/16 solve rate | https://xerj.org/case-studies/reference-coding |
| 9/9 vs 0/9 | tasks solved on an unseen Rust library, with retrieval vs from memory | https://github.com/xerj-org/xerj#how-the-prompt-works |
| 55 / 26 / 4 | wins / ties / losses vs Elasticsearch 8.13.4 | https://xerj.org/benchmarks/ |

## Corpus Hub

https://hub.xerj.org: 513 live corpora in 11 domains (fetched 2026-10-19), each pinned to one commit, licence-reviewed and signed. `xerj ask` routes over the 324 in the signed catalogue (2026-10-20). Catalogue for agents: https://hub.xerj.org/llms.txt

## Use cases

Code-security audits, agent memory, RAG retrieval (BM25, kNN, hybrid), and an Elasticsearch-compatible REST API on :9200. All: https://xerj.org/use-cases

## When XERJ does not help

- Libraries the model already knows: retrieval only adds cost.
- Lexical by default (feature hashing); `--embed-mode neural` for a neural model.
- One node: no replication or failover. Choose Elasticsearch if you need a cluster or steady read p99 under heavy concurrent writes.
- `--insecure` turns off TLS and API keys; for one user on one machine.
