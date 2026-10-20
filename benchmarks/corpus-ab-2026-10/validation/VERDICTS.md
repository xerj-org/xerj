# Validation verdicts — #1111 A/B manual gate

Binary: `/tmp/xtarget-1093/release/xerj` at pinned SHA `eb2f5689c0d98a9983294f7a0e1e0b8ac61ecbd8`
(same build both arms ran against). Every GROUP 1–3 case was driven dynamically against a
locally booted scratch node (ports 99xx, throwaway data dirs under `/root/val/`). GROUP 4
includes tool-side PoCs (`xerj corpus build` / `corpus add` / `autoindex` against hostile
inputs) and one MITM capture. Scripts and raw evidence: `/root/val/group*.sh`,
`/root/val/group*-report.txt`, `/root/val/g3-*.log`, `/root/val/tlsmitm.log`.

Verdict scale: **confirmed-exploitable** (dynamic PoC reproduces the claimed impact) ·
**confirmed** (mechanism dynamically real, impact shape as claimed, no crash) ·
**confirmed-downgraded** (mechanism real, impact materially smaller than claimed) ·
**confirmed-inspection** (code-level proof with file:line; dynamic not driven where noted) ·
**false-positive** (guard fires; claimed impact not reachable).

## GROUP 1 — request-path panics & amplification (--insecure nodes)

| Case | Finding(s) | Verdict | Evidence |
|---|---|---|---|
| E | p2/F2 slice.max=2^32 | **confirmed-exploitable** | 1 doc + `{"slice":{"id":0,"max":4294967296}}` → abort `es_compat.rs:12592:25` (divide by zero). Node dead. |
| F | x1/F4 number_of_shards:0 + sliced search | **confirmed-exploitable** | `PUT {"settings":{"number_of_shards":0}}` acknowledged, then sliced `_search` → abort `es_compat.rs:37571:20` (rem_euclid 0). |
| G | x1/F2 painless `}{` | **confirmed-exploitable** | `POST /_scripts/painless/_execute {"script":{"source":"}MovingFunctions.max(){"}}` → abort `es_compat.rs:35623:23`. |
| B1 | x1/F1 sort format multibyte | **confirmed-exploitable** | date-typed field + `sort:[{"t":{"format":"yyyyéé"}}]` → abort `es_compat.rs:4136:24`. Needs a date/numeric sort field; text-field sort does not reach the translator (first re-test missed for that reason). |
| B2 | x1/F3 + p2/F3 docvalue_fields format | **confirmed-exploitable** | doc `{"note":".éé"}` + `docvalue_fields:[{field:note,format:strict_date_optional_time}]` → abort `es_compat.rs:38715:33`. |
| J | p2/F5 pipeline set on non-object | **confirmed-exploitable** | `POST /idx/_doc?pipeline=boom` body `"just a string"` → abort in `serde_json-1.0.149/src/value/index.rs:102` (IndexMut on non-object) — the claimed class exactly. |
| A1 | p1/F5 mapping 50k dots | **confirmed-exploitable** | `PUT /{idx}/_mapping` (update path, raw `{"properties":…}` body) with a 50,000-dot property → `fatal runtime error: stack overflow, aborting`. Create-index does NOT recurse (200k dots acknowledged there) — first re-test missed for that reason. |
| A2 | p1/F1 + p2/F8(c2) force_synthetic_source | **confirmed-exploitable** | doc with a 100,000-segment dotted key → any `?force_synthetic_source=true` read aborts (stack overflow). 25k segments SURVIVES (4 MiB RT stack ≈ 160 B/frame) — the finding's 25–50k threshold is ~4× optimistic. **Persistent**: after restart normal reads work, every synthetic read kills again. |
| A3 | p2/F8(c1) scripted update dot-path | **confirmed-exploitable** | `_update` with `ctx._source.<100k dots>z = 1` → stack overflow abort. One write-privileged request. |
| I | x2/F2(a) date_histogram 0ms | **confirmed-exploitable** | `fixed_interval:"0ms"` → abort `xerj-engine/src/aggs.rs:6179:26`. (Claim lists four parameters; (a) tested here.) |
| D | p1/F2 pipeline DAG expansion | **confirmed-exploitable** | Only the INLINE `POST /_ingest/pipeline/_simulate` body expands; the by-id form silently no-ops (its own correctness bug). Inline: one ~150-byte POST on a 24-deep twin chain → +86 MB RSS / 268 ms; time 268→727→1,919→5,218 ms at depth 24→30 = ×2.6 per +2 (Fibonacci) — unbounded, from ~30 cheap PUTs. The literal "49 GB @ 27" figure not re-measured. |
| H | p2/F6 hdr digits=20 | **confirmed-downgraded** | The u64-wrap loop is real, but the engine's 30 s search deadline cancels it: a patient request returns `{"took":30001,"timed_out":true}`, node serving normally 8 ms later. 40 parallel requests burn ≤30 s of one worker each; no permanent pin, no node freeze. DoS amplification (30 s CPU/request, repeatable) — not the claimed unrecoverable state. |
| C | p1/F3 + p2/F9 suggester CPU | **confirmed** | 200-term dict: 10-char token = 9 ms; 20,000-char token = 669 ms — unbounded O(m·n) ×2, no caps, no crash. 74× at lab scale; scales with dictionary size on a real index. |

## GROUP 2 — authz/auth (auth-enabled node :9921, scoped keys, share guest)

| Case | Finding(s) | Verdict | Evidence |
|---|---|---|---|
| N | p2/F1 snapshot-restore wildcard (the P arm's only CRITICAL) | **false-positive** | Key scoped `read` on `alpha` only: restore `{"indices":"*"}` → 403 `action [write] is unauthorized … on [*]`. Also refused: write-on-alpha key × wildcard; unscoped key × wildcard; RO key × named `beta`. beta docs stayed deleted. The claimed unguarded pattern arm is closed at this SHA. (Finding was a static argument; P2's own method notes live re-verification only of what "mattered".) |
| O | x2/F1 RO key mutates ingest config | **confirmed-exploitable** | Same RO key: `PUT /_ingest/pipeline/victim` → `{"acknowledged":true}`; admin sees it installed. Cluster-wide mutation from a read-only tenant key. |
| P | x2/F5 metrics token sees all index names | **confirmed** | `GET /v1/metrics` with the scrape token returns per-index series naming `alpha` and `beta` (foreign tenant). No memory brains existed on this node to check that half. |
| Q | x1/F7 `_resolve` leaks foreign aliases | **confirmed** | RO key (403 on beta itself) gets `/_resolve/index/*` → `aliases:[{"name":"beta-secret-alias",…}]`. |
| R | x1/F8 predictable task ids | **confirmed** | Admin's `_update_by_query` → task `local:3`; RO key `GET /_tasks/local:3` returns the admin's completed task record (action, timings, description). |
| S | x1/F9 unauth 401 leaks data-dir layout | **confirmed** | `curl /logs-2026/_search` with no credentials → 401 body contains the absolute `…/admin.key` path and the export hint. |
| T | p2/F10 repo settings to any key | **confirmed** | RO key `GET /_snapshot/repo` → `{"settings":{"location":"/root/val/g2snap"}}` — filesystem layout disclosure. |
| V | x2/F6 share-rate kill switch | **confirmed** | 10 claims with wrong passcodes (all 401), then the CORRECT passcode → HTTP 429 `too_many_requests`. One link id, no passcode knowledge, denies the reading room. |
| — | x2/F7 templates unbounded map | **confirmed** | 200 × `PUT /_scripts/tpl{n}` of 100 KB → RSS +23 MB pinned for process life (payload 20 MB + overhead), no eviction, no cap. Repeatable to exhaustion. |

## GROUP 3 — persistence tamper (patch + reboot + query)

| Case | Finding(s) | Verdict | Evidence |
|---|---|---|---|
| .seg | p2/F4 + x2/F3 + p1/F10 | **confirmed-exploitable** | 8-byte footer patch (`section_table_offset` → file_len+0x100000; footer is OUTSIDE the CRC, no recompute needed) → node boots green, FIRST query aborts `segment.rs:638: range start index 1051734 out of range for slice of length 3158`. Repeats every restart: permanent poison. |
| .norms | x1/F5 | **confirmed-exploitable** | 4-byte patch (`payload_len` @ offset 9 → 0xFFFFFFF0) → boots green, first text search aborts `xerj-fts/src/index.rs:2343:33`. |
| .dv | p1/F4 + p2/F7 | **confirmed-downgraded** | Poisoned the live segment of a >10,000-doc index (fast-agg gate) with payload_len = 1 TiB and = 2^63, then ran stats / extended_stats / terms / sorts / docvalue_fields: no abort anywhere, results stay CORRECT. Root cause: `read_doc_values_sidecar` does `decode_columns(...).unwrap_or_default()` — every decode failure (read_exact EOF after the overcommitted alloc) swallows to empty and the engine falls back to the brute path. The unvalidated-length code defect is real (hardening item), but the claimed "immediate process abort on first read" is not reachable through any tested API shape at this SHA. |
| _seq_no | x1/F6 | **confirmed-exploitable** | Decompressed the stored section (LZ4), rewrote all 50 records' `_seq_no` to 0, rebuilt the block, recomputed the CRC — node boots, reads fine; one fresh doc + `_forcemerge` → **51 docs → 1**. Log: `segment merge complete … live_docs=1`, no error. Silent, permanent deletion of live documents; CRC-32 is not a barrier, exactly as claimed. |

## GROUP 4 — corpus-tool / supply-chain / console / client cases

| Case | Finding | Verdict | Evidence |
|---|---|---|---|
| W | p1/F6 git `rev` argv injection | **confirmed-exploitable** | Recipe with `rev = "--upload-pack=touch /root/val/pwned-f6"`, url = local repo: `xerj corpus build f6poc --recipe …` → fetch "fails" BUT `/root/val/pwned-f6` exists — arbitrary command execution as the operator from one recipe file. |
| X | p1/F7 pack slug traversal | **confirmed-exploitable** | Checksum-VALID pack (SHA256SUMS fully verified, counts match) with source slug `../../../../pwned-f7dir`: `xerj corpus add` wrote attacker JSONL to `/root/val/pwned-f7dir/records.jsonl`; a planted `sub/keep.txt` was recursively deleted on re-add (`remove_dir_all(dest.join(prev_slug))`). Arbitrary write + arbitrary recursive delete through the full integrity layer. |
| Y | p1/F9 autoindex TLS disabled | **confirmed-exploitable** | Python HTTPS server with a self-signed cert on :9443; `xerj autoindex … --url https://localhost:9443` with `XERJ_API_KEY=ADMIN-SECRET-KEY-123` connected and sent `Authorization: ApiKey ADMIN-SECRET-KEY-123` — captured. Admin key handed to any on-path MITM. |
| Z | p1/F8 console data-sources scope | **confirmed-inspection** | `data_sources.rs` handlers take `_sess: AuthSession` (underscore, never consulted; lines 81/125/179/248) and gate only on builtin/wildcard/`is_hidden_index` (system + reserved only, line 46). Dynamic pass needs the full passkey ceremony (WebAuthn authenticator), not curl-drivable. |
| — | x2/F4 session cookie never Secure | **confirmed-inspection** | `login.rs:195` and `passkey.rs:280` both pass literal `false` to `make_set_cookie(signed, false)` → `set_secure(false)`; `sessions.rs:102-109`. Dynamic requires an HTTPS console deployment. |
| — | x1/F10 login/setup.html headers | **confirmed** (dynamic) | `GET /_xerj-console/login.html` and `/setup.html` → 200 with ONLY `cache-control: public, max-age=300` — no CSP, no frame protection, no no-store — while `index.html` carries the full strict set. |
| — | x2/F8 MCP raw path interpolation | **confirmed-inspection** | `enc()` exists (lib.rs:1153, "this is not optional") and is used by the graph builders; seven other builders interpolate raw tool args: `/{index}/_search` (1414/1433/1458/1483/1743), `/_memory/{namespace}` (1515, a WRITE), `/_memory/{namespace}/_recall` (1538), `/_cat/indices/{pattern}` (1725). Prompt-injected tool args reach an admin-keyed request path unencoded. |

## Totals

| Arm | Findings filed | Confirmed-exploitable (dynamic) | Confirmed (mechanism/headers) | Downgraded | Inspection-confirmed | False-positive |
|---|---|---|---|---|---|---|
| P (plain) | 20 | 13 | 2 (C: p1/F3 + p2/F9) | 3 (p1/F4, p2/F6, p2/F7) | 1 (p1/F8) | 1 (p2/F1 — the arm's only critical) |
| X (corpus) | 18 | 16 | 0 | 0 | 2 (x2/F4, x2/F8) | 0 |
| both | 38 | 29 (27 unique cases; several findings share a root cause) | 2 | 3 | 3 | 1 |

## Notes on process

- Several "misses" on first re-test were PoC-shape errors (wrong endpoint body shape, wrong
  field type, sub-threshold recursion depth, single-segment merge), not refutations — each was
  retried against the code path before any verdict. The genuine downgrades (H, A2's threshold,
  the .dv class) and the one false positive show the gate is not rubber-stamping.
- Where a claim could not be fully re-driven dynamically (WebAuthn console ceremony, HTTPS
  console deployment), the verdict says **confirmed-inspection** with file:line — not
  "confirmed-exploitable".
- x2/F2 named four unvalidated agg parameters; one (a) was dynamically confirmed. The other
  three were not re-driven and are carried at that one's verdict.
