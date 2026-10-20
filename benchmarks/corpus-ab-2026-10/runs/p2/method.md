I mapped the attack surface from the two routers (which routes exist, which
middleware runs and in what order), then audited each focus area in parallel
with subagents covering es_compat.rs, the xerj-wasm ingest pipeline, the
xerj-storage/xerj-engine persistence readers, and the console/SPA. I then
personally re-derived and dynamically verified every high-severity claim
against a prebuilt release binary, booting fresh nodes per test.

Dynamically confirmed: the authz wildcard-restore bypass (literal 403 vs
wildcard 200, data actually rolled back), the slice.max divide-by-zero
(including end-to-end from an unauthenticated /_share/claim guest key that was
403 on everything else), the docvalue_fields char-boundary panic, the .seg
footer tamper causing a three-restart crash loop, the pipeline non-object
panic, the HDR infinite loop at ~92% of a core, and the snapshot repo path
leak. I also tried to reproduce the reported _clone escalation; it did not
reproduce (index_guard held), so I excluded it. Static-only findings are marked
medium confidence.

Notes on method and limits:

- Release profile is `panic = "abort"` with no overflow-checks and no
  CatchPanicLayer anywhere in the router stack, so every reachable panic is a
  whole-node kill, not a 500. That is what turns several of these from "bug"
  into "critical".
- I used `--insecure` for the panic tests (no auth needed there) and a real
  auth-enabled node with minted keys for the authorization findings, so the
  privilege claims are grounded in actual role_descriptors.
- Areas checked and found clean, so not re-litigated: WAL replay (exemplary —
  length caps, CRC-before-decode, boundary resync), bundle unpack, snapshot
  create/restore path confinement, console SPA XSS (no markup sinks; CSP
  pinned), client_ip trusted-proxy handling, path traversal in index names,
  and the painless script sandbox.
