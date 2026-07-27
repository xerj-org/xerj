# Reproduce every result

All numbers in this case study are reproducible. Scripts live in
[`../../examples/ast-vuln-graph/`](../../examples/ast-vuln-graph/).

## 0. Prerequisites

```bash
pip install -r ../../examples/ast-vuln-graph/requirements.txt   # tree-sitter grammars
# WordPress core to audit (any recent tag):
git clone --depth 1 https://github.com/WordPress/WordPress.git ./wordpress
# XERJ running on 127.0.0.1:9200 (ES-compatible wire):
xerj --insecure --data-dir ./wpdata &
```

`XERJ=http://127.0.0.1:9200` in the commands below.

## 1. Build the audit substrate (index once)

```bash
cd ../../examples/ast-vuln-graph
python3 wp_audit_index.py   ./wordpress   # -> wpaudit (11,990 fns) + wphooks (1,343)
python3 wp_authz_graph.py   ./wordpress   # -> wpauthz (cap/nonce arg-shapes + call edges)
python3 wp_compose_index.py ./wordpress   # -> wpcompose (sanitizer-sequence fingerprints)
```

Parse rate is 100% with tree-sitter-php (vs ~63% for phply); full build ≈ 3.6s.

## 2. Reproduce each finding

**Unauthenticated attack surface** (only 2 of 1,343 hooks):
```bash
curl -s "$XERJ/wphooks/_search" -H 'Content-Type: application/json' \
  -d '{"query":{"term":{"unauth":true}},"_source":["hook","callback"]}'
```

**SQLi triage** (11,990 fns → 4 candidates; grep hands you 51 whole files):
```bash
curl -s "$XERJ/wpaudit/_search" -H 'Content-Type: application/json' \
  -d '{"query":{"bool":{"filter":[{"term":{"has_source":"true"}},{"term":{"sinks":"sql"}},{"term":{"sanit":"false"}}]}}}'
# NOTE: use "true"/"false" as STRINGS — see the boolean-term engine bug below.
```

**Authorization / IDOR sweep** (AJAX + REST → 0 missing-cap IDOR):
```bash
# classifies wp_ajax_* and every REST *_permissions_check by cap object-scoping.
# (the sweep loads _source and traverses the call graph in Python — see note.)
```
The authz reasoning is in [`wordpress-authz-agentic-audit.md`](../../research/wordpress-authz-agentic-audit.md).

**Check-vs-use IDOR** (finds the revisions shape core guards):
```bash
python3 wp_checkuse_idor.py       # 12 candidates; revisions is the guarded one
```

**File-scope `wp-admin` handlers** (41 action handlers; object-scoped):
```bash
python3 wp_admin_pages.py ./wordpress
```

**SSRF completeness** (reproduces the 169.254 gap independently):
```bash
python3 wp_ssrf_ranges.py         # flags wp_http_validate_url: MISSING 169.254 / 100.64 / IPv6
```

**Sanitizer composition** (one fingerprint query; ~2,700× fewer tokens):
```bash
curl -s "$XERJ/wpcompose/_search" -H 'Content-Type: application/json' \
  -d '{"query":{"bool":{"filter":[{"term":{"compose_danger":"true"}},{"term":{"sink_raw":"true"}}]}},"_source":["func","file","sanitizer_seq"]}'
```

## 3. Verify the SSRF by execution (no PHP needed)

`verify_ssrf.py` (below) ports `wp_http_validate_url` line-for-line and executes
it on live payloads — `169.254.169.254` returns ALLOWED while `127/10/192.168` are
rejected. The port is included in
[`../../research/wordpress-verification-and-xerj-vs-grep.md`](../../research/wordpress-verification-and-xerj-vs-grep.md).

## Notes on trusting XERJ's structured queries (important)

Two engine bugs the audit surfaced — use these workarounds until the fixes land:

1. **`term` on keyword ARRAYS matches only the first element.** The interprocedural
   sweeps therefore load `_source` and traverse the call graph in Python, NOT via
   `{term:{calls:...}}`. Fix + PR:
   [`../../research/xerj-keyword-array-term-fix.md`](../../research/xerj-keyword-array-term-fix.md).
2. **Boolean `term` matches as a string** — use `{"term":{"has_source":"true"}}`,
   not `true`. Same PR series.

`_source` scans and full-text (`match`) are unaffected and complete; the sound
conclusions in this case study were all computed over `_source`, so they hold.

## Token accounting

The ~26,000-token total (vs ~5,200,000 to load core once) is the sum of the
per-phase query+read costs measured in the research journals; the composition
audit's ~2,700× and the reachability query's ~2,823× are each reproduced by the
commands above against the live index.
