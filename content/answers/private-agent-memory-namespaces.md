---
title: "Give two agents private memory on one laptop"
h1: "How do I give two agents private memory on one laptop?"
description: "XERJ keeps agent memory on local disk and made 0 outbound connections in our capture. Roles are stored but not enforced, so scoped API keys do the work."
slug: "private-agent-memory-namespaces"
cluster: "Agent memory: security"
question: "How do I keep one agent's memory from leaking into another agent's?"
intent: "informational"
published: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start a XERJ node without --insecure, mint a scoped API key, store a memory in one namespace, then try to recall a different namespace with that key and report the exact status code you receive."
commands:
  - cmd: "curl -s -XGET 'http://127.0.0.1:8430/_security/roles'"
    note: "Read the node's own statement about role enforcement. Use the native REST port, not the Elasticsearch-compatible one."
  - cmd: "curl -s -XPOST 'http://127.0.0.1:9430/_memory/definitely-not-mine/_recall' -H 'content-type: application/json' -d '{\"query\":\"anything\",\"k\":3}'"
    note: "Recall a namespace that does not exist and compare the answer with an unauthorized one."
  - cmd: "curl -s -XGET 'http://127.0.0.1:9430/_cat/indices?format=json&bytes=b'"
    note: "List the reserved .xerj-memory-* indices that hold every namespace on this node."
links_out:
  - "coding-agent-memory-across-sessions"
  - "knowledge-graphs-for-agent-memory"
  - "/docs/security"
  - "set-mcp-memory-storage-path"
faq:
  - q: "How do I keep one agent's memory from leaking into another agent's?"
    a: "Give each agent its own namespace and its own scoped API key. A scoped key is confined to its named indices, and authorization runs before the existence check."
  - q: "Can two local agents share a search engine but not memory?"
    a: "Yes. Both can query the same document indices on one node, while each scoped key reaches only its own reserved `.xerj-memory-*` namespace."
  - q: "How do I point an MCP memory server at a folder I choose?"
    a: "Point the node's data directory there. XERJ has no separate memory path: memory lives in the data directory as `.xerj-memory-{namespace}`, and the MCP server stores nothing itself."
  - q: "Does XERJ enforce roles?"
    a: "No. The node itself answers `\"enforced\": false` and warns that every authenticated caller has full superuser access. Full role enforcement is deferred."
  - q: "What authorization does exist?"
    a: "Scoped API keys. XERJ confines a scoped key to its named indices and reserves the `.xerj-memory-*` namespace, which is principal scoping rather than roles."
  - q: "Can an attacker enumerate my namespaces?"
    a: "Not through status codes on a secured node. Authorization runs before the existence check, so an absent brain and an unauthorized brain both answer 403."
  - q: "What does --insecure turn off?"
    a: "TLS and API-key authentication. Any client that reaches the port can then read and delete every index, so use it on a development machine only."
---

**TL;DR** — Agent memory stays on local disk, and our capture recorded 0 non-loopback connections during store and recall. Privacy from other callers is a different question. XERJ stores roles but does not enforce them, so a namespace is only as private as the scoped API key that reaches it.

## What stays local

Every memory is a document in a reserved index inside the node data directory. Our capture watched the node process tree during store and recall and counted 0 distinct non-loopback peers across 31 samples covering 2.27 s.

The node also stayed on loopback. All three listening sockets sat on `127.0.0.1`, and XERJ binds there by default. A node on a network interface with TLS off additionally needs `server.allow_insecure_network_bind = true`.

That sampler polls `/proc/net/tcp` every 50 ms, so it is not a packet capture. Cite the 0 with that limit attached.

## What the node says about roles

XERJ ships 6 seeded roles and does not enforce any of them. The node states this itself on the native REST listener, and the wording is worth quoting in full.

```text
roles are stored but NOT enforced: every authenticated caller has full superuser
access regardless of any role assignment. Full RBAC enforcement is deferred.
```

The response carries `"enforced": false` beside those 6 role definitions. Read that as the operative fact: on a XERJ node, a role assignment changes nothing.

## Ask the right port

The security surface answers on the native REST listener, not on the Elasticsearch-compatible one. In our capture `GET /_security/roles` answered on port 8430 and returned HTTP 404 with an empty body on port 9430.

A reader who checks the wrong port sees a 404 and can conclude that no security surface exists. Use the native port for anything under `_security`.

One related endpoint deserves a warning. The `_has_privileges` route answers true to everything. Do not treat it as an authorization oracle.

## The authorization that does exist

Authorization in XERJ is API-key principal scoping, not roles. A scoped key is confined to its named indices, and the `.xerj-memory-*` namespace is reserved, so a scoped key cannot read another principal's memory namespace.

Two boundaries belong beside that. A key minted without role descriptors gets no brain at all, yet keeps historical reach over ordinary non-reserved indices. There is no SSO of any kind in XERJ: no SAML, no OIDC, no LDAP and no Kerberos.

By design, authorization runs before the existence check. A brain that is not yours and a brain that does not exist both answer 403. No status code tells a caller which brains exist on the node.

## What our capture did not test

The harness ran every node with `--insecure` so that it needed no key. This pass therefore never exercised API-key principal scoping or the reserved-namespace rules.

The visible consequence is in the capture. A recall against a namespace that does not exist returned HTTP 200 with an empty hits list, not the 403 a secured node gives.

```json
{"hits":[],"namespace":"definitely-not-mine"}
```

Do not read that response as the security behavior. That answer comes from a node with authentication turned off. The two-scoped-key denial matrix needs a secured node and a separate run.

## How to make a namespace private

Work through five steps in order.

1. Start the node without `--insecure`, so TLS and API-key authentication stay on.
2. Keep the bind address at `127.0.0.1` unless a remote client truly needs the port.
3. Mint one scoped API key per agent, naming only the indices that agent needs.
4. Give each agent its own memory namespace, because the namespace is the unit of authorization.
5. Back up the data directory yourself, because XERJ is single-node and has no object-store snapshot destination.

## What this capture does not show

This pass measured one single-node process with authentication disabled. The capture shows where the data lives and what the node says about role enforcement. No denied cross-namespace recall exists in it, so no page can claim one until a secured run captures it.
