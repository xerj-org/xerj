---
title: "Retrieval that cites file:line and says not found"
h1: "Retrieval that says not found, and cites file and line when it does answer"
description: "xerj answers with file:line and exits 1 with no match when a symbol or corpus is missing. Free-text questions still return passages; confidence is #1786."
slug: "retrieval-that-says-not-found-with-file-line-citations"
cluster: "Answers for coding agents"
question: "Is there a retrieval tool that says not found, cites file and line, and abstains when unsure?"
intent: "tool-selection"
published: "2026-10-19"
updated: "2026-10-20"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
structured_data: [faq]
og_image: "og/answers-retrieval-that-says-not-found-with-file-line-citations.png"
og_image_alt: "Cover: three dot discs for three outcomes: answered at zstd.h:134 with exit 0, no match with exit 1, and an off-topic question that still returned passages with exit 0 (issue #1786)"
queries:
  - "retrieval that says not found and cites file and line"
  - "retrieval tool that abstains when unsure and cites file:line"
commands:
  - cmd: "xerj ask --corpus lz4 --def LZ4_compress_default"
    note: "An exact definition: file:line, declaration, pinned commit and licence, exit 0."
  - cmd: "xerj ask --corpus lz4 --def LZ4_DOES_NOT_EXIST_XYZ"
    note: "A symbol that is not there: no match, exit 1."
  - cmd: "xerj ask \"what is the capital of australia\""
    note: "A question no corpus covers: no corpus matches, exit 1."
evidence:
  - claim: "Missing symbol: no match, exit 1"
    source: "benchmarks/agent-intents-2026-10-19/06-nodef.txt"
  - claim: "No corpus matches: exit 1"
    source: "benchmarks/agent-intents-2026-10-19/06-noc.txt"
  - claim: "Off-topic question: four lz4 passages, exit 0"
    source: "benchmarks/agent-intents-2026-10-19/06-offtopic.txt"
  - claim: "LZ4_compress_default at lz4.c:1481 and lz4.h:191"
    source: "benchmarks/agent-intents-2026-10-19/06-def.txt"
  - claim: "Calibrated confidence is open as issue #1786"
    source: "https://github.com/xerj-org/xerj/issues/1786"
links_out:
  - "local-code-search-mcp-fewer-tokens-than-grep"
  - "ask-questions-about-a-github-repo-without-cloning"
  - "batch-lookup-cve-ghsa-ids-offline"
  - "hybrid-search-quality-measured-ndcg"
  - "rag-without-vector-database"
  - "/compare/xerj-vs-vector-database"
faq:
  - q: "Is there a retrieval tool that says not found?"
    a: "xerj does for exact lookups. `xerj ask --def` on a missing symbol printed `no match in lz4` and exited 1, and a question no corpus covers printed `no hub corpus matches` and exited 1."
  - q: "How do I get cited answers with file and line?"
    a: "Use `xerj ask --def` for a symbol or `xerj search` for a passage. Every hit prints path:line, and `xerj ask` adds the corpus, the pinned commit date and the licence."
  - q: "Will it abstain when unsure of the answer?"
    a: "Only when nothing matches. An off-topic free-text question to a corpus still returned four lz4 passages with exit 0. Calibrated confidence and an abstain flag on answers are issue #1786, open."
  - q: "Do search results come with a confidence score?"
    a: "They come with a BM25 score, such as 17.13 for the zstd definition; ranking is lexical by default. It is a ranking score, not a probability, and it is not comparable across queries."
  - q: "Are the citations version-pinned?"
    a: "For hub corpora, yes: the JSON carries `pin_sha`, `pin_date` and `age_days`. A full trace-to-evidence report is issue #1789, open."
  - q: "Can an agent tell the outcomes apart without reading the text?"
    a: "Yes, by exit code: 0 answered, 1 no match, 2 unknown corpus, 3 a pack failed verification, 4 more than one corpus matches. The MCP tools return the same answers as text."
---

**TL;DR** — For retrieval that says not found and cites file and line, `xerj ask` answers with file:line, the pinned commit and the licence, and exits 1 when a symbol or corpus is missing. It does not yet abstain on weak free-text matches: calibrated confidence is issue #1786. Measured 2026-10-19 on v1.0.0-rc.97.

## Run it

Three questions to the same tool, from a HOME where the lz4 pack was already cached.

An exact symbol answers with two file:line citations:

```sh
xerj ask --corpus lz4 --def LZ4_compress_default
```

```text
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/lib/lz4.c:1481
int LZ4_compress_default(const char* src, char* dst, int srcSize, int dstCapacity)
  approach-only licence (mixed: BSD-2 (lib) / GPL-2.0+ (rest)): read, do not copy
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/lib/lz4.h:191
LZ4LIB_API int LZ4_compress_default(const char* src, char* dst, int srcSize, int dstCapacity);
  approach-only licence (mixed: BSD-2 (lib) / GPL-2.0+ (rest)): read, do not copy
answered from: local pack
```

A symbol that does not exist says so and exits 1:

```sh
xerj ask --corpus lz4 --def LZ4_DOES_NOT_EXIST_XYZ; echo "exit $?"
```

```text
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)
no match in lz4 (pin 2026-10-02)
answered from: local pack
exit 1
```

A question no corpus covers also exits 1, and names the nearest corpora instead of guessing:

```sh
xerj ask "what is the capital of australia"; echo "exit $?"
```

```text
no hub corpus matches; see xerj.org/llms.txt
closest corpora: snappy, age, astro; re-run with --corpus <slug> to ask one of them
exit 1
```

## Where it does not abstain yet

Force an off-topic question onto one corpus and xerj returns its best passages anyway, with exit 0:

```sh
xerj ask --corpus lz4 "kubernetes pod eviction policy"
```

```text
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/SECURITY.md:0
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/doc/lz4_Frame_format.md:0
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/programs/lz4io.c:594
lz4  pin 2026-10-02 (18 days old)  mixed: BSD-2 (lib) / GPL-2.0+ (rest)  lz4/lib/lz4frame.c:638
```

None of these answers the question. The two Markdown files are cited as line 0; a fix for prose line numbers in `xerj code` merged to main after rc.97 (#1908) and is not in a release yet. Until #1786 lands, an agent should treat a free-text hit as a lead to read, not as an answer.

## Measured on 2026-10-19

| Question | Output | Exit | Wall time |
| --- | --- | --- | --- |
| `--def LZ4_compress_default` | `lz4.c:1481`, `lz4.h:191` | 0 | 20 ms |
| `--def ZSTD_CLEVEL_DEFAULT` (JSON) | `zstd.h:134`, value 3, score 17.13 | 0 | 20 ms |
| `--def LZ4_DOES_NOT_EXIST_XYZ` | no match | 1 | 20 ms, cached pack |
| "what is the capital of australia" | no hub corpus matches | 1 | 14 ms |
| `--corpus lz4` "kubernetes pod eviction policy" | 4 unrelated passages | 0 | 485 ms, first run |

## How it compares

| Tool | Cites file:line | Says not found | Calibrated confidence | Pinned version |
| --- | --- | --- | --- | --- |
| `xerj ask --def` | yes | yes, exit 1 | no, issue #1786 | yes, for hub corpora |
| `xerj ask` free text | yes | only when nothing matches | no, issue #1786 | yes, for hub corpora |
| grep / ripgrep | yes | yes, exit 1 | n/a, exact match | your checkout |
| Vector search top-k | if you stored it | no, always returns k | no, similarity only | if you stored it |
| Chat model, no retrieval | no | rarely | no | no |

## When not to use xerj

- You need a calibrated probability that an answer is right. xerj prints a BM25 ranking score, and search is lexical by default.
- You need line numbers for prose files today. rc.97 cited Markdown files as line 0 in this run.
- The question needs reasoning across many files, not a passage. Retrieval finds the passage; it does not write the answer.
- You need the citation trail as a report. That is issue #1789, open.
