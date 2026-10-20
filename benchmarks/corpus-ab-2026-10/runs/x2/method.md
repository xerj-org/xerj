# Method

Read the router, authz, auth, share and console sources directly, and fanned out
five parallel subagents over wasm/painless ingest, persistence readers,
console/static serving, the `_search` path, and the secondary APIs. I then
personally re-verified everything that mattered against a live node built from
the prebuilt binary with auth enabled.

A first attempt at minting a scoped key silently produced an unscoped one
(`role_descriptors` must be an object, not an array), which briefly invalidated
the escalation demo until corrected.

Dynamically confirmed:
- the `/_ingest/pipeline` privilege escalation, including cross-tenant
  destruction of a PII-redaction pipeline;
- four distinct `_search` panic-aborts, one issued as an unauthenticated
  share-link guest;
- a `.seg` section-table overwrite with the CRC recomputed, proving the
  checksum is no barrier against this adversary;
- the metrics-token index-name leak.

I dropped two promising leads — a `/_decide` write-back into reserved
namespaces and an `api_key` minting path — after live tests showed their guards
firing.
