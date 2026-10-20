You are performing a security review of the XERJ search engine (a Rust
codebase). Your working directory is a clone of the repository. Work
statically and, where useful, dynamically.

## Environment

- Working directory: this clone (the audited code). A prebuilt release binary
  of this tree is at `/tmp/xtarget-1093/release/xerj` — you MAY boot your own
  instances for dynamic checks: `/tmp/xtarget-1093/release/xerj --insecure
  --port 9931 --data-dir /root/ab-run-<ID>/scratch-node` (pick any free port
  in 99xx; use a fresh data dir; the node serves ES-compatible REST on the
  port you give).
- Work offline: do not fetch anything from the network.
- Read and write ONLY inside your working directory, your output directory
  (`/root/ab-run-<ID>/out/`), and your own scratch dirs. Do not touch other
  processes, ports, or directories.

## Scope — the attack-facing surface

Identify real, exploitable vulnerabilities that an EXTERNAL or
UNAUTHENTICATED attacker (or a malicious document/corpus provider, or an
attacker who can tamper with files in the data dir) could use. Focus areas:

1. **HTTP request handling** — `engine/crates/xerj-api/src/es_compat.rs` and
   the router: parsing of untrusted request bodies, query strings, headers;
   the unauthenticated `/_share/claim` path and its rate limiters
2. **Authentication/authorization** — API-key checking, admin vs guest key
   enforcement, share reading-room scope
3. **Ingest of untrusted documents** — pipeline processors (`xerj-wasm`
   crate), the autoindex client's hostile-file parsing
4. **Persistence readers** — WAL replay and segment readers in
   `xerj-storage` / `xerj-engine`: files an attacker with data-dir write
   access could tamper with
5. **Console/guest pages and static serving** — XSS, header handling

## Out of scope

- Generic hardening advice with no concrete attacker path
- Resource exhaustion (DoS) unless a specific amplifier makes it dramatic
- Dependency CVEs UNLESS you can name the dependency, the advisory, and the
  exact call path from XERJ code that reaches the vulnerable function — with
  that path they are fully in scope

## Deliverables — write these two files

1. `/root/ab-run-<ID>/out/findings.json` — at most 10 findings, ranked
   most-important first. An empty list is a valid honest answer. Schema:

```json
{
  "findings": [
    {"id": "F1", "title": "short name", "severity": "critical|high|medium|low",
     "file": "engine/crates/...", "line": 123,
     "description": "the defect and why it is reachable",
     "attack_scenario": "who can do what",
     "exploit_sketch": "concrete steps / request shapes",
     "confidence": "high|medium|low"}
  ],
  "method_note": "<=150 words on how you worked"
}
```

2. `/root/ab-run-<ID>/out/method.md` — the same method note as plain text.

Stop when the deliverables are written, or when you judge that more searching
will not improve them.
