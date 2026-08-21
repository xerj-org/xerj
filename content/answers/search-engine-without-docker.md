---
title: "Is there a single-binary local search engine?"
h1: "Is there a single-binary local search engine I can just run?"
description: "XERJ runs from one native binary with no container and no JVM. A measured capture records 3 linked libraries and a 1.071 s cold start to the first HTTP 200."
slug: "search-engine-without-docker"
cluster: "Operations: installation"
question: "Is there a single-binary local search engine I can just run?"
intent: "cost"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, download the XERJ release binary for this host, start a single-node instance with --insecure on a scratch data directory, poll GET / until it answers 200, and report the linked libraries and the cold-start time you measured."
commands:
  - cmd: "xerj --version"
    note: "Print the binary version before you start a node."
  - cmd: "xerj --insecure --data-dir ./xerj-data"
    note: "Start one single-node process directly on the host, with no container."
  - cmd: "curl -s http://127.0.0.1:9200/"
    note: "Poll the Elasticsearch-compatible port until the node answers 200."
links_out:
  - "/docs/install"
  - "estimate-autoindex-time-before-running"
  - "cheap-low-volume-log-search"
  - "local-embeddings-without-openai-api"
  - "/compare/xerj-vs-elasticsearch-local"
  - "/compare/xerj-vs-meilisearch"
faq:
  - q: "Is there a single-binary local search engine I can just run?"
    a: "XERJ is one native executable. The captured binary links only libgcc_s.so.1, libm.so.6 and libc.so.6, and answered its first HTTP 200 in 1.071 s from process start."
  - q: "Is there a search engine I can run locally without Docker?"
    a: "Yes. The capture ran the executable directly on the host with no container runtime. Docker remains optional packaging."
  - q: "I want local search that works offline, no cloud embeddings. What are my options?"
    a: "Run the binary on the host and index the folder in place. The default embedder is lexical feature hashing and runs in-process, so the default path needs no embedding service."
  - q: "I just want to search logs on my laptop. I don't want Elasticsearch in Docker."
    a: "Yes. Start the binary on a scratch data directory, index the log folder, and query the Elasticsearch-compatible port that the same process serves."
  - q: "Does XERJ need a JVM?"
    a: "No. The captured binary links only libgcc_s.so.1, libm.so.6 and libc.so.6, so no Java runtime is present on the dependency list."
  - q: "How large is the XERJ binary?"
    a: "The captured release build measured 67,174,440 bytes and carries debug symbols. The project's own stripped measurement is 36.06 MiB."
  - q: "Can XERJ run as a multi-node cluster?"
    a: "No. XERJ is single-node: there is no data-plane replication and no failover, so plan for snapshot and restore instead."
---

**TL;DR** — XERJ runs a search engine from one native binary, with no container and no JVM. A first-party capture on Linux x86-64 shows 3 dynamically linked libraries and a 67,174,440-byte executable. Cold start measured 1.071 s to the first HTTP 200 on an empty data directory.

## One executable, no container

XERJ ships as a single native executable that runs directly on the host. The capture in `RUN-F` started the binary with no container runtime and no interpreter in front of it.

`readelf` reports the file type. The command below reads the same header the capture read.

```sh
readelf -h ./xerj
```

The captured header shows `Class: ELF64` and `Type: DYN (Position-Independent Executable file)` on `Advanced Micro Devices X86-64`.

## What the binary links against

The captured binary declares 3 `NEEDED` shared libraries. Every one of them ships with an ordinary glibc Linux system. No Java runtime, no Python runtime, and no container image appears in the list.

| Library | Role |
| --- | --- |
| `libgcc_s.so.1` | Compiler support routines |
| `libm.so.6` | C math functions |
| `libc.so.6` | Standard C library |

Read the dependency list on your own target before you install.

```sh
ldd ./xerj
```

## Cold start and the first query

Cold start measured 1.071 s from process start to the first HTTP 200 on `GET /`, against an empty data directory. The poller sampled every 0.5 s, so the captured value is an upper bound.

For a tighter number, read the node's own `startup complete in Nms` line in its server log.

The first query returns an Elasticsearch-shaped version document.

```json
{
  "cluster_name": "xerj",
  "name": "local",
  "tagline": "You Know, for Search",
  "version": { "number": "8.13.0", "lucene_version": "9.10.0" }
}
```

## Binary size, stated honestly

The captured release build measured 67,174,440 bytes on disk. That build carries debug symbols, so it is larger than the artifact a release strips. The project's own stripped measurement is 36.06 MiB, and the two numbers describe different files rather than a disagreement.

## What single-node means for installation

XERJ is single-node, and that is the only configuration this capture measured. There is no data-plane replication, no failover and no multi-region mode. An install plan must therefore cover snapshot and restore rather than node loss.

## Conditions during the capture

Other workloads used the same host during the capture. The run therefore prints its 1-minute load average next to every timing.
