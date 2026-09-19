---
title: "What a XERJ share-link guest can and cannot reach"
h1: "If I share one index with a link and a passcode, what can that person actually reach on my node?"
description: "A share-link guest key reads the shared index and nothing else: no other index, no _cat or _cluster, no writes, no scroll or PIT. Each refusal is a live test."
slug: "what-a-share-link-guest-can-reach"
cluster: "Sharing: read-only guest access"
question: "If I give someone a read-only share link to one search index, can they see my other indices or anything else on the server?"
intent: "informational"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt. On a throwaway XERJ node with authentication on, create a share with POST /_share, claim it with POST /_share/claim and a JSON body of {id, passcode}, then use the returned api_key to call /_cat/indices, /_cluster/health, a search on another index, a write to the shared index and a search with ?scroll=1m, and report the status code of each call."
commands:
  - cmd: "xerj share casefile --label 'for the auditor' --max-claims 1"
    note: "Create a share over one index. Needs the node's admin key, read from the data directory."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9200/_share/claim' -H 'content-type: application/json' -d '{\"id\":\"0123456789abcdef0123456789abcdef\",\"passcode\":\"abcd-2345\"}'"
    note: "What the guest page does: exchange the share id and the passcode for a read-only key. Both travel in the body, so the id is not in the request line an access log records. This id and passcode are placeholders and return 404."
  - cmd: "xerj share --revoke 8a1e546d4ee8"
    note: "End a share by the handle that the list prints. Every key it minted is invalidated."
links_out:
  - "share-folder-read-only-search-without-uploading"
  - "share-local-search-index-through-a-tunnel"
  - "private-agent-memory-namespaces"
  - "/docs/security"
evidence:
  - claim: "The guest route allow-list: _search, _count, _msearch, _mget, _mapping, _field_caps, GET _doc, and the graph ego and overview routes."
    source: "engine/crates/xerj-api/src/authz.rs"
  - claim: "The live test makes 237 checks against a node with authentication on and the access log on (225 without the native-listener section); all passed on 2026-09-19."
    source: "docs/usecases/share-links/VERIFICATION.md"
  - claim: "A share id is 128 bits and is stored as a SHA-256 digest; the passcode is stored as an Argon2id hash in a file with mode 0600."
    source: "engine/crates/xerj-api/src/share.rs"
  - claim: "Unknown share ids are limited to 10 a minute and 100 an hour per source address; a known share to 10 a minute and 30 an hour."
    source: "engine/crates/xerj-api/src/share.rs"
  - claim: "The guest page is served with default-src 'none' and frame-ancestors 'none'."
    source: "engine/crates/xerj-console-api/src/spa.rs"
faq:
  - q: "If I give someone a read-only share link to one search index, can they see my other indices or anything else on the server?"
    a: "No. The guest key is granted `read` on the shared index only, and a route allow-list closes everything that describes the node. Another index is a 403 by name, by alias and from inside a request body."
  - q: "Can a guest list my indices with _cat/indices?"
    a: "No. `_cat`, `_cluster`, `_nodes`, `_snapshot`, `_tasks` and `_stats` all return 403 to a guest key. An ordinary scoped key sees a filtered view of those; a guest sees none of it."
  - q: "Can a guest write, delete or reindex?"
    a: "No. Index, update, delete, bulk, delete-by-query, reindex, refresh, mapping and settings changes all return 403. The live test also confirms that no guest write landed."
  - q: "Can a guest keep a scroll or a point-in-time open on my machine?"
    a: "No. `scroll`, `_search/scroll`, `_pit` and `_async_search` are refused, and so is a `pit` clause in a search body, so a guest cannot ride a point-in-time that someone else opened."
  - q: "What is stored on disk about a share?"
    a: "A SHA-256 digest of the share id and an Argon2id hash of the passcode, in `shares.json` with mode 0600. No share id, passcode or guest key is written to that file, to the API-key file or to the audit log. The guest page sends the share id in a request body and never in a URL, so the node's access log does not hold it either."
  - q: "What can a guest still do that I should think about?"
    a: "Read every document in the shared index and copy what they read. A share grants whole indices. There is no per-document or per-field restriction to apply."
---

**TL;DR** — A share-link guest gets a key with `read` on the shared index and nothing else. A route allow-list, used for guests only, closes `_cat`, `_cluster`, every other index, all writes, and scroll and point-in-time contexts. Each refusal is a check in a live test that ran 237 checks on 2026-09-19, with 0 failures.

## The key a guest is given

Claiming a share mints an ordinary scoped API key with one role, named `share:` plus the share's handle. It carries `read` on the indices the share names and, when a brain is named, on that brain's edges index. It expires when the share does.

Two things confine that key, and both are enforced by the server. The first is the index grants every scoped key gets. The second is a route allow-list that applies only to keys whose roles are all named `share:`. Being recognised as a guest only ever removes reach.

## What is on the allow-list

| A guest can call | On |
| --- | --- |
| `_search`, `_count`, `_msearch`, `_mget` | the shared indices |
| `GET _doc/{id}` | the shared indices |
| `_mapping`, `_field_caps` | the shared indices |
| `GET /_graph/{brain}/ego` and `/overview` | the shared brain |
| `GET /`, `_security/_authenticate`, health probes | the node |

Nothing else. The allow-list can only refuse: a route it permits still goes through the index decision, so an index the guest was not granted is a 403 on a permitted route.

## What was refused in the live test

The test runs against a real node with authentication on. It seeds one shared index and one private index that holds a marker sentence, then looks for that sentence in every response.

| Attempt | Result |
| --- | --- |
| `_cat/*`, `_cluster/*`, `_nodes`, `_snapshot`, `_tasks`, `_stats`, `_aliases`, the audit log, `/v1/metrics`, the native `/v1/*` router | 403 |
| the private index by name, in a comma list, by `_search` with no index | 403, or an answer that holds shared documents only |
| an alias that points at the private index | 403 |
| `_msearch` and `_mget` bodies that name the private index | refused, no private text returned |
| a `terms` lookup, a `more_like_this` like-document, a stored `percolate` document and a `lookup` runtime field that name the private index | no private text returned |
| `?scroll=1m`, also as `SCROLL` and percent-encoded; `_search/scroll`; `_pit`; `_async_search` | 403 |
| the owner's own point-in-time id and scroll id, presented by the guest | 403 |
| index, update, delete, bulk, delete-by-query, reindex, refresh, close, mapping and settings changes | 403 |
| another brain, writes to the shared brain, `/_memory/*` | 403 |
| `GET /_share`, `POST /_share`, `DELETE /_share/{handle}`, `POST /_security/api_key` | 403 |
| the Console API under `/_xerj-console/api/v1/` | 401: it takes a session cookie, not an API key |

A pattern such as `_all` is not refused. It is expanded over what the key holds, so for a guest it means the shared indices and no more.

## An alias is fixed when the share is made

Sharing an alias pins the concrete indices it points at now. The test shares an alias, re-points it at another index, and confirms the guest still reads the first index and gets a 403 on the second.

## The claim route

`POST /_share/claim` is the one route on the node that hands out a credential without authentication, so it is narrow. It is `POST` only, its body is `{id, passcode}` and at most 4 KiB, it is never cached, and every outcome goes to the audit log with the source address.

The share id is in the body and not in the path. A request path is what an access log, a reverse proxy and a tunnel write down by default; a body is not, unless a proxy is set up to log bodies. The test runs against a node with `logging.access_log = true` and looks in that log for every share id it made. It finds none.

A claim against a known share is charged to that share: 10 a minute and 30 an hour, from anywhere. A claim against an unknown id is charged to the source address: 10 a minute and 100 an hour. The two are not stacked, so a flood of made-up ids cannot lock a real guest out, even when a tunnel makes every client arrive from the same address. The test floods the source bucket with a rotating `X-Forwarded-For` and then claims successfully from the same address.

## What this does not cover

A guest can read every document in the shared index, and its mapping. XERJ has no per-document or per-field restriction. That is also why the `autoindex-catalog` index is never granted: it lists every corpus on the node and could not be filtered. `POST /_share` refuses it by name, in a list and through an alias.

Successful reads are only partly audited. The audit log holds a line for every claim outcome and every `_search`. It also holds a line for every request refused to an authenticated key, such as a guest's attempt to list, create or revoke a share or to mint a key. It holds no line for a successful `_msearch`, `_mget`, `_count`, `_mapping`, `_field_caps` or `GET _doc`. So a guest can read the whole shared index through those without a trace. It holds no line for an unauthenticated request other than a claim.

Through `xerj share --tunnel`, Cloudflare ends TLS and can read the passcode, the guest key, the searches and the documents.

Guest searches are not rate-limited. Authorization in XERJ comes from scoped API keys; roles are stored but not enforced. XERJ is single-node, so the share is only as available as that one host.

The test ran on Linux. macOS and Windows were not run.
