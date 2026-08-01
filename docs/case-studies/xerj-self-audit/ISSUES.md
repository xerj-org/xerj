# Issues — five-stage instruction/data audit pass

**FILED (2026-07-29), after the three application-layer Highs were fixed on
`feat/rust-self-audit` (`58fc73f`).** Full detail, taint paths, and
live-reproduction transcripts: [FINDINGS-V2.md](FINDINGS-V2.md).

| # | Finding | Severity | Status |
|---|---|---|---|
| [#72](https://github.com/xerj-org/xerj/issues/72) | INJ-01 search-template injection | High | fixed, tracking merge |
| [#73](https://github.com/xerj-org/xerj/issues/73) | F-PATH-02 snapshot location traversal | High | fixed, tracking merge |
| [#74](https://github.com/xerj-org/xerj/issues/74) | S5-1 session-revocation lost update | High | fixed, tracking merge |
| [#75](https://github.com/xerj-org/xerj/issues/75) | DESER-EGRESS-01 unauth Raft transport | High (gated) | fixed, tracking merge — HMAC frame auth + fail-closed config |
| [#76](https://github.com/xerj-org/xerj/issues/76) | Console hardening: S5-4, S5-3, S5-5, AUTHZ-2 | Med/Low/Info | open |

The three Highs were filed publicly only *after* the fix was committed. The
open ones are either operator-gated (DESER-EGRESS-01, needs `cluster.enabled`) or
Medium-and-below, described by class + location + fix without copy-paste exploit
payloads.

---

The original drafts (INJ-01, F-PATH-02) that seeded #72 and #73 follow.

**Shared caveat for both severity labels.** Both routes sit behind
`auth_middleware`, but `crates/xerj-api/src/auth.rs:96` short-circuits to fully
open when `admin_api_key` is empty, and `--insecure` disables auth outright. In
either configuration both issues are **unauthenticated**.

---

## Draft 1 of 2

**Title:** `security: search-template params are spliced into the query JSON unescaped and re-parsed (a param can drop the template's security filter)`

**Labels:** `security`, `es-compat`, `bug`
**Severity:** High
**Reachability:** authenticated-network — app-mediated (the application supplies
the trusted template; the untrusted party supplies only `params`)

### Summary

`render_template` merges untrusted `params` into a trusted query template with a
raw `String::replace` and no escaping, then re-parses the merged string as JSON.
A param value carrying JSON structural tokens is re-interpreted as query
structure, so a param can **delete a security filter the template encoded**.
This defeats the exact contract search templates exist to provide.

### Code

`crates/xerj-api/src/es_compat.rs:23160`

```rust
fn render_template(source: &str, params: &serde_json::Map<String, Value>) -> String {
    let mut result = source.to_string();
    for (key, val) in params {
        let placeholder = format!("{{{{{}}}}}", key);
        let replacement = match val {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        result = result.replace(&placeholder, &replacement);   // no escaping
    }
    result
}
```

Consumed at `es_compat.rs:23205-23206` (`search_template`), `23408`
(`msearch_template_impl`), `23520` (`render_template_api`) — each one
`render_template(...)` then `serde_json::from_str(&rendered)` then
`xerj_query::parse_request`.

Routes: `POST /:index/_search/template` (`router.rs:543`),
`POST /_msearch/template` (`router.rs:545`), `POST /_render/template`
(`router.rs:553`).

### Reproduction

```bash
# application's trusted template encodes an ACL filter
TMPL='{"query":{"bool":{"filter":[{"term":{"visibility":"public"}}]}},"size":{{size}}}'

# benign: 1 hit (public doc only)
curl -s localhost:9200/idx/_search/template -H 'Content-Type: application/json' \
  -d "{\"source\":\"$(echo "$TMPL" | sed 's/"/\\"/g')\",\"params\":{\"size\":50}}"

# injected: 2 hits, including the private doc — the filter is gone
curl -s localhost:9200/idx/_search/template -H 'Content-Type: application/json' \
  -d "{\"source\":\"...\",\"params\":{\"size\":\"50,\\\"query\\\":{\\\"match_all\\\":{}}\"}}"
```

Rendered output is `... "size":50,"query":{"match_all":{}}}` — a duplicate
`query` key; `serde_json` keeps the last, dropping the filter.
`POST /_render/template` with the same params echoes
`{"query":{"match_all":{}},"size":50}`, which shows the rewrite with no index
state at all.

Verified on two independent throwaway servers (ports 9317 and 9319).

### Scope note

The **stored-id** path (`PUT /_scripts/{id}` then `{"id":...}`) double-encodes and
returns `template rendered to invalid JSON` — it breaks rather than executes. The
exploitable vector is inline `source` + untrusted `params`.

### Suggested fix

Stop substituting-then-parsing. Preferred: parse `source` into a
`serde_json::Value` first and substitute **whole leaf `Value`s** by key, so a
param is always exactly one JSON node and can never emit structural tokens.

Acceptable alternative: JSON-escape every `String` replacement, reject params in
non-string positions unless they parse as a standalone JSON scalar, and implement
the Mustache distinction — `{{x}}` escaped, `{{{x}}}` raw.

Add regression tests for: a param containing `"`, a param containing
`,"query":{...}`, a numeric-position param containing `,`, and `{{{x}}}` still
allowing intentional raw injection.

---

## Draft 2 of 2

**Title:** `security: snapshot repository settings.location is unvalidated — snapshots write and copy every index outside data_dir (no path.repo equivalent)`

**Labels:** `security`, `es-compat`, `bug`
**Severity:** High
**Reachability:** authenticated-network

### Summary

`PUT /_snapshot/{repo}` stores `settings.location` verbatim, and
`Engine::create_snapshot` uses it as the **root** of the snapshot path. The
containment check derives its reference point from that same attacker-supplied
string, so it is tautologically satisfied. Result: a caller can make the server
`create_dir_all` + recursively copy **every user index** into any directory the
process can write.

Elasticsearch requires the location to be inside a `path.repo` allowlist in
`elasticsearch.yml` and refuses the registration otherwise. XERJ has no
equivalent — `grep -rnE 'path\.repo|repo_allowlist|snapshot_root' crates`
returns **0 lines**.

### Code

`crates/xerj-engine/src/engine.rs:1562`

```rust
let snap_dir = std::path::Path::new(repo_path).join(name);
let snap_dir = validate_snapshot_path(repo_path, name, &snap_dir)?;
std::fs::create_dir_all(&snap_dir).map_err(EngineError::Io)?;
```

The check, `engine.rs:1877` inside `validate_snapshot_path` — note `repo_canon`
comes from `repo_path`, i.e. from the request:

```rust
let repo_canon = std::path::Path::new(repo_path)
    .canonicalize()
    .unwrap_or_else(|_| std::path::PathBuf::from(repo_path));
if let Ok(snap_canon) = snap_dir.canonicalize() {
    if !snap_canon.starts_with(&repo_canon) { /* refuse */ }
```

Where `repo_path` originates, `crates/xerj-api/src/es_compat.rs:22098`:

```rust
let repo_path = repo_config
    .pointer("/settings/location")
    .and_then(Value::as_str)
    .unwrap_or("/tmp/xerj-snapshots");
```

And the registration that stores it unmodified, `es_compat.rs:22024`
(`put_snapshot_repo`) — it validates the repo **name** only, then
`state.engine.snapshot_repos.insert(repo, body);`.

Three filters exist (`validate_snapshot_repo_name` `es_compat.rs:21991`,
`validate_snapshot_name` `es_compat.rs:22004`, `validate_snapshot_path`
`engine.rs:1852`) and **all three guard names, none guards the location.**

### Reproduction

```bash
curl -XPUT localhost:9200/_snapshot/evil \
  -H 'Content-Type: application/json' \
  -d '{"type":"fs","settings":{"location":"/tmp/OUTSIDE_DATADIR"}}'
curl -XPUT localhost:9200/_snapshot/evil/s1 -d '{}'
# -> {"accepted":true, ... "state":"SUCCESS"}
find /tmp/OUTSIDE_DATADIR
```

Observed on disk, entirely outside `data_dir`:

```
/s1/manifest.json
/s1/myindex/{schema.json,snapshot.json,xerj_meta.json}
/s1/myindex/wal/{s0..s15}
/s1/myindex/segments/<uuid>.{seg,sidx,ids,dv}
/s1/myindex/segments/<uuid>.secret.{fst,post,meta,norms}
```

`"location":"../../../../tmp/pwn"` works identically: `canonicalize()` either
resolves the traversal (and `repo_canon` becomes the escaped path) or fails and
the code falls back to `PathBuf::from(repo_path)` verbatim — in **both** branches
`repo_canon` is the attacker's root.

### Impact

1. Arbitrary recursive directory creation and file write anywhere the process uid
   can write (docroot, log dir, watched/import dir).
2. Exfiltration of every index's raw stored fields, doc values and postings to a
   path the attacker can read by other means.
3. Unbounded disk fill on any mounted filesystem.

### Suggested fix

1. Add an operator allowlist to `Config` — e.g.
   `snapshot.allowed_repo_roots: Vec<PathBuf>`, **default empty = snapshots
   disabled**, mirroring ES's `path.repo` posture.
2. Validate `settings.location` in `put_snapshot_repo` at **registration** time:
   require absolute, `canonicalize()`, require `starts_with` a configured root,
   else reject the registration.
3. Change `validate_snapshot_path` to take its containment root from **config**,
   never from `repo_path`. A check whose reference point comes from the request is
   not a check.
4. Apply the same root on the read side (`Engine::restore_snapshot`,
   `engine.rs:1654`, which reads `manifest.json` from the same chosen directory).

Regression tests: absolute location outside every configured root -> rejected at
registration; `..` location -> rejected; empty allowlist -> snapshot API returns a
clear "snapshots not configured" error; a location inside a configured root ->
still works.
