---
title: "Let an agent look up endpoints in an API spec"
h1: "My agent needs to look up endpoints in a big OpenAPI spec and the SDK markdown. What's the right way?"
description: "Index the spec file itself. A captured run found route names, operation descriptions and operationIds in both the JSON and the YAML serialization."
slug: "search-openapi-spec-for-agent"
cluster: "Files and formats"
question: "My agent needs to look up endpoints in a big OpenAPI spec. What's the right way?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a local XERJ node, put an OpenAPI document on disk as both .json and .yaml, run `xerj autoindex ./api-specs --url http://127.0.0.1:9200 --prefix oa --progress plain`, then POST a match on body for a route name and for an operationId, GET /oa-docs/_mapping, and report the ax_format of every hit plus the complete field list the spec produced."
commands:
  - cmd: "xerj autoindex ./api-specs --url http://127.0.0.1:9200 --prefix oa --progress plain"
    note: "Index a folder of OpenAPI documents from local disk."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/oa-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match\":{\"body\":\"quokka-orders\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\",\"ax_locator\"],\"track_total_hits\":true}'"
    note: "Find a route name across every serialization of the spec."
  - cmd: "curl -s -XPOST http://127.0.0.1:9200/oa-*/_search -H 'content-type: application/json' -d '{\"query\":{\"match_phrase\":{\"body\":\"Retrieve quokka orders by region\"}},\"size\":10,\"_source\":[\"ax_path\",\"ax_format\",\"ax_locator\"],\"track_total_hits\":true}'"
    note: "Find an operation description written in the spec summary."
  - cmd: "curl -s -XGET http://127.0.0.1:9200/oa-docs/_mapping"
    note: "Read the complete field list, which shows that extraction is format-based."
links_out:
  - "search-yaml-xml-config-repository"
  - "code-search-mcp-for-claude-code"
  - "catalog-files-with-autoindex-map"
faq:
  - q: "My agent needs to look up endpoints in a big OpenAPI spec. What's the right way?"
    a: "Put the spec on disk, run `xerj autoindex` on its folder, and have the agent send a `match` on `body`. The captured run made route names, descriptions and operationIds queryable with 1 command."
  - q: "How do I make an OpenAPI spec searchable for my coding agent?"
    a: "Index the folder that holds it and expose the index to the agent. Extraction is format-based, so the agent queries the document text rather than a typed route object."
  - q: "How do I search a spec and the docs folder together?"
    a: "Keep them in one folder and index it once. The spec and the SDK markdown become documents in the same run, so one index pattern reaches both and each hit carries `ax_path` and `ax_format`."
  - q: "Does XERJ parse OpenAPI as a specification?"
    a: "No. The whole document produced only `body` and `title` beyond the provenance fields. Extraction is format-based, so a path is text and not a typed route object."
  - q: "Can I search for an operationId?"
    a: "Yes. A `match` on `body` for `getOrderCheckpointJournal` returned 2 hits, 1 per serialization. The operationId is ordinary text inside the document."
  - q: "Do I need a separate parser for path parameters?"
    a: "Yes, if you want typed routes. XERJ returns the matching document and its `ax_path`, and your agent parses the spec itself for parameter structure."
  - q: "Can I count routes with an aggregation?"
    a: "Read the hits, not only the buckets. A captured matrix shows `match_phrase` matching 11 documents while its terms aggregation returned 0 buckets."
---

**TL;DR** — XERJ makes an OpenAPI document queryable when `xerj autoindex` reads the file from disk. In a captured run, a route name, a description and an `operationId` each returned 2 hits, 1 per serialization. Extraction is format-based: the whole spec produced only `body` and `title`.

## Index the spec file itself

XERJ treats an OpenAPI document as structured data in its own format. A `.json` spec goes through the JSON family and a `.yaml` spec through the YAML family, and both land in the same index.

```sh
xerj autoindex ./api-specs --url http://127.0.0.1:9200 --prefix oa --progress plain
```

The captured run read 2 files into 1 dataset and 4 documents live in `oa-docs`, with 0 junk files. The 2 files were 1 valid OpenAPI 3.0.3 document, serialized once as JSON and once as YAML by the fixture generator.

## Both serializations answer the same query

Each query below ran once against `/oa-*/_search` and returned 1 hit per serialization. The `ax_format` field on the hit says which file matched.

| query | hits | formats returned |
| --- | --- | --- |
| `match` on `body` for `quokka-orders` | 2 | `json` and `yaml` |
| `match_phrase` on `body` for `Retrieve quokka orders by region` | 2 | `json` and `yaml` |
| `match` on `body` for `getOrderCheckpointJournal` | 2 | `json` and `yaml` |

```sh
curl -s -XPOST 'http://127.0.0.1:9200/oa-*/_search' \
  -H 'content-type: application/json' \
  -d '{"query":{"match":{"body":"quokka-orders"}},"size":10,"_source":["ax_path","ax_format","ax_locator"],"track_total_hits":true}'
```

Keep both serializations on disk only if you want both. Otherwise a single-file query returns 1 hit for the same text.

## Extraction is format-based, not spec-aware

The whole document produced 2 content fields: `body` and `title`. The rest of the mapping is XERJ provenance: `ax_dataset`, `ax_file`, `ax_format`, `ax_locator`, `ax_path`, `ax_paths` and `ax_run`.

There is no `paths` field, no `operationId` field and no per-operation document. A route name matches because the string is in the text, not because XERJ modelled the route.

Plan around that. Use XERJ to find the document and the position, then let your agent parse the spec for parameter types, request bodies and response schemas.

## Read the hits before you trust a bucket count

If you aggregate over spec documents, compare the buckets with the returned hits for that exact query. A terms aggregation is not reliably scoped to the full-text query it travels with.

One captured matrix on a single index makes the risk concrete.

| query type | documents matched | aggregation buckets |
| --- | --- | --- |
| `match_all` | 674 | 331 |
| `exists` | 315 | 315 |
| `term` | 630 | 315 |
| `match_phrase` | 11 | 0 |
| `match` | 11 | 74 |

This is not a universal rule, and the same second pass recorded an aggregation that agreed exactly with its hits. Publish the number you read from the hits.

## What this capture does not show

The spec in this run came from the fixture generator, not from a vendor or a live API. No API server ran, and XERJ fetched nothing over the network. XERJ has no `$ref` resolver and no schema validator.

XERJ runs single-node here, with no replication and no failover. The default embedder in XERJ is lexical feature hashing, so a query and a paraphrase that share no words do not match. Neural embeddings are opt-in through `--embed-mode neural`.

Every number above comes from RUN-B and RUN-G, captured on 2026-08-21. The binary was a `ci-test` profile build, so no wall-clock figure from these runs is published as a performance number.
