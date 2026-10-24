---
title: "Ask a GitHub repo without cloning: cited file:line answers"
h1: "Ask questions about a GitHub repo without cloning it"
description: "xerj ask answers from a signed, pre-indexed pack with file:line, pinned commit and licence. Nothing to clone. Works for repos in the Corpus Hub catalogue."
slug: "ask-questions-about-a-github-repo-without-cloning"
cluster: "Answers for coding agents"
question: "How can I ask questions about a GitHub repo without cloning it?"
intent: "how-to"
published: "2026-10-19"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
og_image: "og/answers-ask-questions-about-a-github-repo-without-cloning.png"
og_image_alt: "Cover: a question routed across the 14 corpora of the signed catalogue to the snappy tile, then to a local node on port 9300 with the answer at snappy.cc:1643; cold 2,356 ms, warm 30 to 40 ms"
agent_prompt: "Read https://xerj.org/llms.txt. Install xerj, then answer my question about the library with xerj ask (add --corpus and --def for an exact symbol). Quote the corpus, the pinned commit, the file:line and the licence line it prints, and say so when it exits 1 with no match."
queries:
  - "ask questions about a GitHub repo without cloning it"
  - "ask a GitHub repo without cloning"
commands:
  - cmd: "curl -fsSL https://xerj.org/get | sh"
    note: "Install the release binary into ~/.local/bin."
  - cmd: "xerj ask \"how does snappy decompress a literal\""
    note: "Route the question to a corpus, pull its signed pack once and answer with file:line."
  - cmd: "xerj ask --corpus zstd --def ZSTD_CLEVEL_DEFAULT --json"
    note: "Exact definition lookup in one corpus, as JSON with the pin, licence class and hits."
  - cmd: "xerj hub status"
    note: "List cached packs, the automatic node and the hosted setting."
evidence:
  - claim: "Cold ask: node on 9300, 10.8 MB snappy pack, snappy.cc:1643"
    source: "benchmarks/agent-intents-2026-10-19/04-ask-snappy-cold.txt"
  - claim: "zstd --def: 20.1 MB pack, 7874 definitions, zstd.h:134"
    source: "benchmarks/agent-intents-2026-10-19/04-ask-zstd-def.txt"
  - claim: "JSON answer with pin_sha, licence_class and score 17.13"
    source: "benchmarks/agent-intents-2026-10-19/04-ask-zstd-def.json"
  - claim: "Timings 2,356 ms cold, 341 ms, then 30 to 40 ms; 962 ms and 20 ms for --def"
    source: "benchmarks/agent-intents-2026-10-19/README.md"
  - claim: "Zero-setup xerj ask for any source is open as issue #1800"
    source: "https://github.com/xerj-org/xerj/issues/1800"
links_out:
  - "local-code-search-mcp-fewer-tokens-than-grep"
  - "retrieval-that-says-not-found-with-file-line-citations"
  - "code-search-mcp-for-claude-code"
  - "/compare/xerj-vs-ripgrep-for-code-agents"
  - "/blog/the-corpus-hub"
faq:
  - q: "How can I ask questions about a GitHub repo without cloning it?"
    a: "If the repo is in the Corpus Hub catalogue, run `xerj ask \"your question\"`. xerj downloads a signed pack that is already indexed, at a pinned commit, and answers with file:line. Nothing is cloned or indexed on your machine."
  - q: "Which repos can I ask about?"
    a: "The signed catalogue generated on 2026-10-18 lists 14 corpora, including lz4, snappy, zstd, aiohttp and astro. hub.xerj.org lists 513 live corpora in its registry, but only catalogue entries have packs `xerj ask` can pull."
  - q: "Can I search a GitHub repo without cloning if it is not in the catalogue?"
    a: "Not with xerj today. Clone it and run `xerj autoindex`. Pointing `xerj ask` at any repository is tracked as issue #1800, which is open."
  - q: "Does my question leave my machine?"
    a: "It can. `--hosted auto` is the default: off inside a git repository, on elsewhere. Pass `--offline` or `--hosted off` to answer only from cached packs. Our runs printed `answered from: local pack`."
  - q: "Can Claude Code or Cursor use this over MCP?"
    a: "Yes. `xerj mcp` serves `xerj_hub_find`, `xerj_hub_def` and `xerj_hub_ask`. They answer from packs already on the machine and never download one; they name the `xerj hub pull` line instead."
  - q: "Can I copy the code it shows?"
    a: "Check the licence line. Each answer prints the SPDX licence and a class. zstd and lz4 are approach-only, and the output says read, do not copy."
---

**TL;DR** — To ask questions about a GitHub repo without cloning it, run `xerj ask "your question"`. If the repo is in the Corpus Hub catalogue, xerj pulls a signed, pre-indexed pack and answers with file:line, the pinned commit and the licence. Measured 2026-10-19: first answer 2,356 ms including the download, later answers 30 to 40 ms.

## Run it

This ran in a fresh HOME with v1.0.0-rc.97. No node was running. `xerj ask` started a node on port 9300, pulled the snappy pack and answered.

```sh
curl -fsSL https://xerj.org/get | sh
xerj ask "how does snappy decompress a literal"
```

```text
xerj hub: no node answers at http://127.0.0.1:9555; starting a local node on port 9300 (data: ~/.xerj/hub/node, log: ~/.xerj/hub/node.log)
xerj hub: node ready on port 9300 (pid 1822371)
xerj hub: pulling 'snappy' (10.8 MB) — one-time download, later questions answer locally
xerj hub: pulled snappy 9c28114a 11338487 → 4 indices, 382 definitions
snappy  pin 2026-10-02 (18 days old)  BSD-3-Clause  snappy/snappy.cc:1643
  DecompressAllTags(Writer* writer) {
    const char* ip = ip_;
    ResetLimit(ip);
    auto op = writer->GetOutputPtr();
    ...
    ... [method DecompressAllTags truncated at 800 chars]
```

Paths are shortened to `~` and the passage is cut for the page. The run set `XERJ_URL` to an unused port, because a test node was already on 9200. With nothing on 9200, the first line names that port.

Ask for one symbol and get one line back:

```sh
xerj ask --corpus zstd --def ZSTD_CLEVEL_DEFAULT --json
```

```json
{
  "corpus": "zstd",
  "pin_sha": "01b7154f1172432f8abe9b3bb9909e14a1176b7d",
  "pin_date": "2026-10-02",
  "age_days": 18,
  "licence_spdx": "mixed: BSD-3 (lib) / GPL-2.0 (cli)",
  "licence_class": "approach-only",
  "answered_from": "local_pack",
  "mode": "def",
  "hits": [
    {
      "file": "zstd/lib/zstd.h",
      "line": 134,
      "kind": "const",
      "text": "#  define ZSTD_CLEVEL_DEFAULT 3",
      "value": "3",
      "score": 17.131968,
      "licence_warning": "approach-only licence (mixed: BSD-3 (lib) / GPL-2.0 (cli)): read, do not copy"
    }
  ],
  "truncated": false,
  "warnings": [
    "approach-only licence (mixed: BSD-3 (lib) / GPL-2.0 (cli)): read, do not copy"
  ]
}
```

`xerj hub status` shows what is cached and where the node runs:

```text
pack: lz4 0774d055  18 days old  5.2 MB  last used today
pack: snappy 9c28114a  18 days old  11.5 MB  last used today
pack: zstd 01b7154f  18 days old  23.5 MB  last used today
node: running  pid 1822371  port 9300
hosted: auto
last sync: never
catalogue: 2 days old
```

The exit code tells an agent the result. 0 is answered, 1 is no match and 2 is an unknown corpus. 3 is a failed pack check and 4 is more than one matching corpus.

## Measured on 2026-10-19

| Run | What happened | Wall time |
| --- | --- | --- |
| First question, fresh HOME | start node on 9300, pull 10.8 MB snappy pack, answer | 2,356 ms |
| Second question | answer from the local pack | 341 ms |
| Later questions | answer from the local pack | 30 to 40 ms |
| First `--def` on zstd | pull 20.1 MB pack (7,874 definitions), answer | 962 ms |
| Same `--def`, as JSON | answer from the local pack | 20 ms |

The catalogue the client verified was signed with ed25519 and generated on 2026-10-18 with 14 corpora. Packs are downloaded from GitHub Releases of xerj-org/xerj, so the GitHub API is not involved.

## How it compares

| Approach | Clone needed | Index on your machine | file:line | Pinned commit | Works for any repo |
| --- | --- | --- | --- | --- | --- |
| `xerj ask` | no | no, pack is pre-indexed | yes | yes | no, catalogue only (14 corpora) |
| Clone + `xerj autoindex` | yes | yes | yes | your checkout | yes |
| Clone + ripgrep | yes | no | yes | your checkout | yes |
| GitHub code search | no | no | yes | default branch | public repos, API rate limits |
| Ask a chat model | no | no | no | no | whatever it was trained on |

## When not to use xerj

- The repo is not in the catalogue. Clone it and use `xerj autoindex`; zero-setup asking for any source is issue #1800.
- You need today's code. Packs are pinned; these were 18 days old, and each answer prints the age.
- The question must not leave the machine and you have not set `--offline` or `--hosted off`.
- You want to copy code from an approach-only corpus into your project. The licence class says read, do not copy.
