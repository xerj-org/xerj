# Share links — verification record

What was run to verify `xerj share`, the `/_share` API and the guest page, on
which build, with which result. Every number cited by the share-link docs and
answers traces to a line on this page. Dates are 2026-09-18 unless stated.

Environment: Linux x86_64, release build of `xerj-server` from branch
`feat/share-links` (rebased on `main` at `4d8dadbf`), `cloudflared` 2026.7.3
from the operator's `~/.local/bin`, Google Chrome (headless) for the browser
flow, Python 3 for the live test. macOS and Windows: **not verified**.

## What the smoke script covers, and what it does not

`.github/scripts/share-links-smoke.sh` boots throwaway nodes on
`127.0.0.1:9510–9512` and runs, in order:

| Phase | What | Where |
|---|---|---|
| A | live security test, authentication on, no trusted proxy | `engine/crates/xerj-api/tests/live/share_security_live.py` |
| B | the CLI: create, `--list`, `--revoke`, `--json`, the error paths, `--tunnel` without `cloudflared` | same script |
| C | the guest page in headless Chrome: claim, search, read, an XSS probe document, sign-out | `xerj-ux/test/share-guest-flow.e2e.mjs` |
| D | the live security test again with `server.trusted_proxies = ["127.0.0.1", "::1"]` | — |
| E | the refusal: `xerj share` against an `--insecure` node | — |
| F | `xerj brain <folder>` → `xerj share <folder>`: the folder resolves to the brain and its indices | — |

CI runs the same script in the `usecase-smoke` job with
`XERJ_SHARE_REQUIRE_BROWSER=1`, so a missing Chrome fails the job rather than
skipping phase C.

**Not covered by the script, and why:** `--tunnel` opening a real
`cloudflared` quick tunnel needs the public internet and a third party. It was
run by hand (below). The hostname parser, the connection-banner detection and
the no-`cloudflared` fallback are unit tests in
`engine/crates/xerj-server/src/share.rs`. The gRPC listener has not been
exercised with a guest key (see docs/SHARING.md).

## Run 1 — first full smoke of this branch on the rebased build

```
A live 201/202 · B CLI 7/8 · C browser 30/31 · D 192/192 · E 2/2 · F 3/3
```

Three failures, each with a distinct cause, all fixed in commit `9c5bfaf0`:

- **A.** The owner-side check that the native listener answers asked
  `GET /v1/indices`, which is POST-only (create) → 405. The native describe
  route is `GET /v1/indices/{name}`. The eight guest refusals on that port
  passed as they were.
- **B.** `share --list | grep -q "$HANDLE"` under `set -o pipefail`. `grep -q`
  exits on its first match; the CLI's next write gets SIGPIPE and the process
  ends with 141 and nothing on stderr (forced deterministically with
  `xerj share --list | true`: exit 141, empty stderr). 0 failures in 40 runs on
  a quiet machine, 1 in 1 in the loaded smoke run. The script now captures
  the output and greps the string. Same change for `--revoke`.
- **C.** The e2e waited for "two result rows" after searching `landlord`, but
  the previous query (`deposit`) had also produced exactly two rows, and those
  stay on screen until the new response lands. The check ran against the stale
  list — its snippet was windowed around "deposit", the previous word. The page
  now stamps `#results` with `data-query` once a list is rendered, and the
  test's `search()` waits for that stamp. The similarity-label logic itself was
  right.

## Run 2 — full smoke on the final build (after the fixes)

Release binary rebuilt after the fixes (`cargo build --release -p xerj-server`,
`Finished release … in 9m 28s`), then `.github/scripts/share-links-smoke.sh`
with `XERJ_SHARE_REQUIRE_BROWSER=1` and Chrome at `/usr/bin/google-chrome`:

```
A. live security test (auth on, no trusted proxies)      passed=202 failed=0
   no share id in the server log                          PASS
B. xerj share CLI                                         8/8
   create --json · create prints link/passcode/expiry/revoke/local-only ·
   --list · --revoke · --list --json reports revoked · folder-not-indexed
   error · wrong-key error · --tunnel without cloudflared (exit 0)
C. guest page, headless Chrome                            passed=31 failed=0
D. live security test (trusted_proxies = loopback)        passed=192 failed=0
E. an open node cannot share                              2/2
F. xerj brain <folder> → xerj share <folder>              3/3
share-links smoke: all phases passed
```

Between run 1 and this run one more test-only change was needed: the first
`waitFor` in the e2e evaluated `document.getElementById('view-claim').hidden`
before the guest page had finished loading (load average 54 at the time) and
the harness treated the resulting exception as fatal. A predicate that throws
now counts as "not yet", and `visible()` is null-safe. No page or server code
changed for that.

The 202 live checks are the 192 of run D plus the native-listener section,
which only runs when `XERJ_NATIVE_URL` is set (phase A sets it; phase D does
not). The counts are fixed by the test files, not by the run.

## ES-compatibility conformance suite

Against an `--insecure` throwaway node on `127.0.0.1:9518`, the release binary
of this branch:

```
es-yaml-runner --url http://127.0.0.1:9518 --dir tests/es-compat-yaml/yaml
1371 passed · 0 failed · 3 skipped · 1374 total
```

Identical to the `main` baseline (1371 / 0 / 3). The share routes are new
paths; nothing on the existing wire surface changed.

## The real quick tunnel, by hand

Node with authentication on at `127.0.0.1:9513`, one index of one document,
one private index. `xerj share tun-notes --expires 30m --max-claims 3 --tunnel
--json`, then every request below through the public hostname with `curl -4`
(this machine resolves `trycloudflare.com` over IPv6 only and cannot route it;
a plain `curl` fails here for that reason alone).

| Step | Result |
|---|---|
| link printed | `https://<random>.trycloudflare.com/_xerj-console/share#<share id>` |
| `GET /_xerj-console/share` through the tunnel | `HTTP/2 200`, `cache-control: no-store, max-age=0`, `content-security-policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`, `referrer-policy: no-referrer`, `x-content-type-options: nosniff`, `x-frame-options: DENY` |
| claim, wrong passcode | `401` |
| claim, right passcode | `200`, `cache-control: no-store, max-age=0`, a key in the body |
| guest `POST /tun-notes/_search` | `200` |
| guest `GET /_cat/indices` | `403` |
| guest `POST /tun-private/_search` | `403` |
| audit log | `share.claim … guest@127.0.0.1 … denied … wrong passcode`, then `… ok … claims_left=2` — the TCP peer, because no trusted proxy was declared |
| SIGINT to `xerj share` | exit 0; "Ctrl-C — closing the tunnel." then "share <handle> revoked — 1 guest key(s) invalidated." |
| `cloudflared` processes | one fewer than before the run (the child was terminated; other tunnels on the machine untouched) |
| guest key after the revoke, on the node directly | `401` |
| `xerj share --list` | the share listed as `revoked`, claims `1/3` |

The tunnel binary was found at `~/.local/bin/cloudflared` (the CLI looks at
`XERJ_CLOUDFLARED`, then `PATH`, then `~/.local/bin`). The run was done once,
by hand; it is not part of CI.

## Unit tests, lint, format

UNIT_PLACEHOLDER

## Wording checks that also gate this page

`python3 scripts/seo/factcheck.py --fail-on error` resolves every
`evidence: source:` in the three share answers; this page is one of those
sources, which is why it carries the numbers rather than the answers.
