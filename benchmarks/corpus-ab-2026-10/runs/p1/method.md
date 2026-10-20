# Method

Static reading of the attack-facing surface (`router`/`auth`/`authz`/`share`/`es_compat`,
`xerj-wasm`, `xerj-autoindex`, `xerj-storage`, `xerj-console-api`) with parallel
subagent sweeps per area, then dynamic confirmation of the top candidates against
the prebuilt release binary on scratch ports/data dirs.

Dynamically reproduced:

- the `force_synthetic_source` stack overflow (node abort at 50k dots),
- the `PUT _mapping` stack overflow,
- the pipeline-DAG expansion (49+ GB RSS, host-threatening),
- the suggest edit-distance amplification (85 s for 1 MB x 2000 terms),
- a `.dv` sidecar abort from a single u64 tamper.

The git `--upload-pack` injection was confirmed against local git 2.39.5.

Findings I could not confirm at runtime are marked with static evidence only.

Checked and cleared rather than reported: CORS (restrictive by default), share
claim rate limiting and key hashing, console session signing/CSRF, zip-slip and
XML entity expansion in autoindex, path traversal in SPA serving, and the
painless interpreter's resource limits.
