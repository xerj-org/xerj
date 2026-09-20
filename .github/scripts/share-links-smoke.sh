#!/usr/bin/env bash
# CI gate for share links (`xerj share`, `/_share`, the guest page).
#
# Boots THROWAWAY nodes on private ports and drives, in order:
#   A. engine/crates/xerj-api/tests/live/share_security_live.py against a node
#      with authentication on AND `logging.access_log = true` — what a guest
#      key can and cannot reach, and that no share id reaches the node's log;
#   B. the `xerj share` CLI against that node: create / --list / --revoke /
#      --json, the folder-not-indexed error, the wrong-key error (index AND
#      folder argument), a --url with no scheme, and --tunnel with a stub
#      `cloudflared` — the Cloudflare notice, and its last lines when it dies;
#   C. xerj-ux/test/share-guest-flow.e2e.mjs — the guest page in a real
#      headless Chrome (claim, search, read, XSS probe, sign-out), on a fresh
#      node with the access log on;
#   D. the same security test against a node that declares loopback a trusted
#      proxy (the `--tunnel` + trusted_proxies configuration);
#   E. the refusal: `xerj share` against an --insecure node exits 1 and says why;
#   F. the headline path: `xerj brain <folder>` boots a node and indexes notes
#      and email, its output carries a pasteable `xerj share` hint, and
#      `xerj share <folder>` resolves the folder to that brain and its indices.
#
# Not covered here, because it needs the public internet and a third party:
# `xerj share --tunnel` actually opening a cloudflared quick tunnel. The
# hostname parser and the fallbacks are unit-tested in xerj-server.
#
# Env overrides:
#   XERJ_BIN                     xerj binary (default <repo>/engine/target/release/xerj)
#   XERJ_SHARE_ROOT              scratch root (default a fresh mktemp -d)
#   XERJ_SHARE_PORT              first of THREE consecutive ports (default 9510); the
#                                nodes run one after another on the same three
#   XERJ_SHARE_REQUIRE_BROWSER   1 = a missing Chrome is a failure (CI), else phase C is skipped
#   CHROME_BIN                   Chrome/Chromium binary for phase C
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
XERJ_BIN="${XERJ_BIN:-$REPO/engine/target/release/xerj}"
ROOT="${XERJ_SHARE_ROOT:-$(mktemp -d)}"
PORT="${XERJ_SHARE_PORT:-9510}"
LIVE="$REPO/engine/crates/xerj-api/tests/live/share_security_live.py"
E2E="$REPO/xerj-ux/test/share-guest-flow.e2e.mjs"

FAILED=0
PIDS=()
phase() { echo; echo "════ $* ════"; }
bad()   { echo "::error::$*"; FAILED=$((FAILED + 1)); }
ok()    { echo "  PASS: $*"; }
cleanup() { for pid in "${PIDS[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null; done; return 0; }
trap cleanup EXIT

# stop <name> — by the PID we recorded, never by pattern; then wait for the port.
stop() {
  local pid; pid="$(cat "$ROOT/$1/server.pid" 2>/dev/null)"
  [ -n "$pid" ] && kill "$pid" 2>/dev/null
  for _ in $(seq 1 60); do
    curl -s -o /dev/null "http://127.0.0.1:$PORT/" 2>/dev/null || return 0
    sleep 0.5
  done
  echo "node $1 did not stop"; return 1
}

[ -x "$XERJ_BIN" ] || { echo "xerj binary not found at $XERJ_BIN"; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required"; exit 1; }

# boot <name> <es_port> <extra [server] toml> <extra cli args…>
# ports: es = $2, rest = $2+1, grpc = $2+2. Auth is ON unless --insecure is passed.
# BOOT_ACCESS_LOG=true turns on `logging.access_log` for that node.
boot() {
  local name="$1" es="$2" extra="$3"; shift 3
  local dir="$ROOT/$name"
  # Something already answering here is NOT our node: the phases below create
  # and delete indices, so never adopt a listener we did not start.
  if curl -s -o /dev/null "http://127.0.0.1:$es/" 2>/dev/null; then
    echo "port $es is already in use — refusing to test against a node this script did not start"
    return 1
  fi
  mkdir -p "$dir/data"
  cat >"$dir/xerj.toml" <<EOF
[server]
es_compat_port = $es
rest_port = $((es + 1))
grpc_port = $((es + 2))
bind_address = "127.0.0.1"
data_dir = "$dir/data"
$extra

[tls]
enabled = false

[logging]
access_log = ${BOOT_ACCESS_LOG:-false}
EOF
  nohup "$XERJ_BIN" --config "$dir/xerj.toml" --data-dir "$dir/data" "$@" >"$dir/server.log" 2>&1 &
  PIDS+=("$!")
  echo "$!" >"$dir/server.pid"
  for _ in $(seq 1 120); do
    # 200 on an open node, 401 on an authenticated one — either means "up".
    code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$es/" 2>/dev/null)"
    { [ "$code" = 200 ] || [ "$code" = 401 ]; } && return 0
    sleep 0.5
  done
  echo "node $name failed to boot on :$es"; tail -20 "$dir/server.log"; return 1
}

# ── A. what a guest key can and cannot reach ───────────────────────────────
phase "A. live security test (auth on, no trusted proxies, access log ON)"
# With the access log OFF (the default) the "no share id in the log" check
# below passes whatever the claim route looks like — which is how the first cut
# shipped a claim path that carried the id. This node logs every request.
BOOT_ACCESS_LOG=true boot a "$PORT" "" || exit 1
A_URL="http://127.0.0.1:$PORT"; A_DATA="$ROOT/a/data"
[ -s "$A_DATA/admin.key" ] || { echo "no admin.key — is auth on?"; exit 1; }
if XERJ_URL="$A_URL" XERJ_NATIVE_URL="http://127.0.0.1:$((PORT + 1))" XERJ_DATA_DIR="$A_DATA" XERJ_SERVER_LOG="$ROOT/a/server.log" python3 "$LIVE"; then ok "share_security_live.py"; else bad "share_security_live.py failed"; fi
# The live test checks the log for the exact ids it created. This is the same
# thing by *shape*, for anything else that ran: a 32-hex id in a logged path.
# (`f`×32 is the live test's probe of the retired id-in-path claim shape.)
grep -q '/_share/claim' "$ROOT/a/server.log" && ok "the node logged its requests (access log on)" || bad "access log is not on — the next check would test nothing"
if grep -E '/_share/[0-9a-f]{32}' "$ROOT/a/server.log" | grep -vq "/_share/$(printf 'f%.0s' $(seq 1 32))"; then bad "a share id was written to the server log"; else ok "no share id in the server log"; fi

# ── B. the CLI ─────────────────────────────────────────────────────────────
phase "B. xerj share CLI"
share() { "$XERJ_BIN" share "$@" --url "$A_URL" --data-dir "$A_DATA" --disable-feedback; }
OUT="$(share casefile --label "for the CI" --expires 30m --max-claims 2 --json 2>"$ROOT/cli.err")"; RC=$?
HANDLE="$(printf '%s' "$OUT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["handle"])' 2>/dev/null)"
LINK="$(printf '%s' "$OUT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["link"])' 2>/dev/null)"
if [ $RC -eq 0 ] && [ -n "$HANDLE" ] && [[ "$LINK" == "$A_URL/_xerj-console/share#"* ]]; then ok "create --json → $HANDLE"; else bad "create --json (rc=$RC): $OUT $(cat "$ROOT/cli.err")"; fi
HUMAN="$(share casefile --label "readable" 2>&1)"
for want in "link:" "passcode:" "expires:" "xerj share --revoke" "read-only" "only this machine can reach"; do
  printf '%s' "$HUMAN" | grep -qF -- "$want" || bad "create output lacks \"$want\": $HUMAN"
done
ok "create prints link, passcode, expiry, how to revoke, and that the link is local-only"
# Capture, then grep: `xerj share … | grep -q` is a trap under pipefail —
# grep -q exits on its first match, the CLI's next write gets SIGPIPE (exit
# 141, nothing on stderr), and the pipeline fails with the handle right there
# in the output. Seen once in CI-shaped runs under load.
LISTED="$(share --list 2>&1)"
printf '%s' "$LISTED" | grep -q "$HANDLE" && ok "--list shows the share" || bad "--list does not show $HANDLE: $LISTED"
REVOKED="$(share --revoke "$HANDLE" 2>&1)"
printf '%s' "$REVOKED" | grep -q "revoked" && ok "--revoke" || bad "--revoke $HANDLE: $REVOKED"
share --list --json | python3 -c "import json,sys; s=[x for x in json.load(sys.stdin)['shares'] if x['handle']=='$HANDLE']; sys.exit(0 if s and s[0]['status']=='revoked' else 1)" \
  && ok "--list --json reports it revoked" || bad "revoked share not reported as revoked"
mkdir -p "$ROOT/never-indexed"
ERR="$(share "$ROOT/never-indexed" 2>&1)"; RC=$?
{ [ $RC -eq 1 ] && printf '%s' "$ERR" | grep -q "has not been indexed"; } && ok "a folder nobody indexed is an error that says so" || bad "folder error (rc=$RC): $ERR"
ERR="$("$XERJ_BIN" share casefile --url "$A_URL" --api-key not-the-admin-key --disable-feedback 2>&1)"; RC=$?
{ [ $RC -eq 1 ] && printf '%s' "$ERR" | grep -qi "admin key"; } && ok "a wrong key is refused with the reason" || bad "wrong-key error (rc=$RC): $ERR"
# The same wrong key with a FOLDER argument used to be reported as "has not
# been indexed — run xerj brain first": the brain lookup read its 401 as "no
# such brain".
ERR="$("$XERJ_BIN" share "$ROOT/never-indexed" --url "$A_URL" --api-key not-the-admin-key --disable-feedback 2>&1)"; RC=$?
{ [ $RC -eq 1 ] && printf '%s' "$ERR" | grep -qi "admin key" && ! printf '%s' "$ERR" | grep -q "has not been indexed"; } \
  && ok "a wrong key with a folder argument is a key error, not \"not indexed\"" || bad "wrong-key + folder (rc=$RC): $ERR"
ERR="$("$XERJ_BIN" share casefile --url "127.0.0.1:$PORT" --data-dir "$A_DATA" --disable-feedback 2>&1)"; RC=$?
{ [ $RC -eq 2 ] && printf '%s' "$ERR" | grep -qF "http://127.0.0.1:$PORT"; } && ok "--url without a scheme is a usage error that says what to type" || bad "--url without scheme (rc=$RC): $ERR"
ERR="$(share autoindex-catalog 2>&1)"; RC=$?
{ [ $RC -eq 1 ] && printf '%s' "$ERR" | grep -q "every corpus"; } && ok "the autoindex catalog cannot be shared" || bad "autoindex-catalog (rc=$RC): $ERR"
OUT="$(share casefile --public-url http://plain.example 2>&1)"
printf '%s' "$OUT" | grep -q "plain http" && ok "--public-url http:// warns that the passcode and key travel unencrypted" || bad "--public-url http:// printed no warning: $OUT"

# `--tunnel` without cloudflared: install steps and the local link, not a failure.
OUT="$(XERJ_CLOUDFLARED=/nonexistent/cloudflared share casefile --tunnel 2>&1)"; RC=$?
{ [ $RC -eq 0 ] && printf '%s' "$OUT" | grep -q "install it" && printf '%s' "$OUT" | grep -qF "$A_URL/_xerj-console/share#"; } \
  && ok "--tunnel without cloudflared prints install steps and the local link (exit 0)" || bad "--tunnel fallback (rc=$RC): $OUT"

# `--tunnel` with a STUB cloudflared: prints a quick-tunnel banner, registers,
# then dies on its own. No network. What the owner must see: that the link goes
# through Cloudflare and what Cloudflare can read — at the moment the link is
# printed, not only in the docs — and, when the tunnel drops, its last lines.
STUB="$ROOT/cloudflared-stub.sh"
cat >"$STUB" <<'STUBEOF'
#!/usr/bin/env bash
echo "2026-01-01T00:00:00Z INF |  https://smoke-stub-tunnel.trycloudflare.com  |" >&2
echo "2026-01-01T00:00:01Z INF Registered tunnel connection connIndex=0" >&2
sleep 2
echo "2026-01-01T00:00:03Z ERR failed to serve tunnel connection error=\"stub: edge went away\"" >&2
exit 1
STUBEOF
chmod +x "$STUB"
OUT="$(XERJ_CLOUDFLARED="$STUB" share casefile --tunnel 2>&1)"; RC=$?
FLAT="$(printf '%s' "$OUT" | tr -s '[:space:]' ' ')"
{ [ $RC -eq 0 ] && printf '%s' "$FLAT" | grep -qF "https://smoke-stub-tunnel.trycloudflare.com/_xerj-console/share#" \
  && printf '%s' "$FLAT" | grep -q "goes through Cloudflare" && printf '%s' "$FLAT" | grep -q "the passcode, the guest's key" \
  && ! printf '%s' "$FLAT" | grep -q "browser reads from this node"; } \
  && ok "--tunnel says the link goes through Cloudflare, and what Cloudflare can read" || bad "--tunnel banner (rc=$RC): $OUT"
{ printf '%s' "$OUT" | grep -q "cloudflared stopped" && printf '%s' "$OUT" | grep -q "stub: edge went away" && printf '%s' "$OUT" | grep -q "revoked"; } \
  && ok "when cloudflared dies on its own: its last lines are shown and the share is revoked" || bad "tunnel death: $OUT"

# ── C. the guest page in a real browser ────────────────────────────────────
phase "C. guest page, headless Chrome (a fresh node, access log ON)"
# Its own node: phase A spent 127.0.0.1's junk-id claim budget on purpose, and
# the page's "this link is not recognised" view needs one unknown-id claim to
# get a 404 rather than a 429. The access log is on so the shape check below
# covers what the page itself requests.
stop a || exit 1
if command -v node >/dev/null; then
  BOOT_ACCESS_LOG=true boot c "$PORT" "" || exit 1
  XERJ_URL="http://127.0.0.1:$PORT" XERJ_DATA_DIR="$ROOT/c/data" node "$E2E"; RC=$?
  case $RC in
    0)  ok "share-guest-flow.e2e.mjs" ;;
    77) echo "  SKIP: no Chrome/Chromium on this machine (set CHROME_BIN)" ;;
    *)  bad "share-guest-flow.e2e.mjs failed (rc=$RC)" ;;
  esac
  if grep -E '/_share/[0-9a-f]{32}' "$ROOT/c/server.log" >/dev/null; then bad "the guest page put a share id in a logged path"; else ok "no share id in the server log after the browser flow"; fi
  stop c || exit 1
else
  [ "${XERJ_SHARE_REQUIRE_BROWSER:-0}" = 1 ] && bad "node is required for the browser test" || echo "  SKIP: node not installed"
fi

# ── D. the tunnel configuration: loopback declared a trusted proxy ─────────
phase "D. live security test (server.trusted_proxies = [\"127.0.0.1\", \"::1\"])"
boot d "$PORT" 'trusted_proxies = ["127.0.0.1", "::1"]' || exit 1
if XERJ_URL="http://127.0.0.1:$PORT" XERJ_DATA_DIR="$ROOT/d/data" XERJ_TRUSTS_LOOPBACK=1 python3 "$LIVE" >"$ROOT/d/live.out" 2>&1; then
  ok "share_security_live.py ($(tail -1 "$ROOT/d/live.out"))"
else
  grep -E "FAIL|passed=" "$ROOT/d/live.out"; bad "share_security_live.py failed behind a trusted proxy"
fi

# ── E. the refusal ─────────────────────────────────────────────────────────
phase "E. an open node cannot share"
stop d || exit 1
boot e "$PORT" "" --insecure || exit 1
E_URL="http://127.0.0.1:$PORT"
curl -s -o /dev/null -XPUT "$E_URL/open-index"
ERR="$("$XERJ_BIN" share open-index --url "$E_URL" --disable-feedback 2>&1)"; RC=$?
{ [ $RC -eq 1 ] && printf '%s' "$ERR" | grep -q "authentication OFF" && printf '%s' "$ERR" | grep -q "restrict nothing"; } \
  && ok "xerj share refuses, exit 1, and says why" || bad "refusal (rc=$RC): $ERR"
CODE="$(curl -s -o "$ROOT/e/create.json" -w '%{http_code}' -XPOST "$E_URL/_share" -H 'content-type: application/json' -d '{"index":"open-index"}')"
[ "$CODE" = 409 ] && ok "POST /_share on an open node → 409" || bad "POST /_share on an open node → $CODE $(cat "$ROOT/e/create.json")"

# ── F. the headline path: xerj brain <folder> → xerj share <folder> ─────────
phase "F. share the folder you gave xerj brain"
stop e || exit 1
F="$ROOT/f"; mkdir -p "$F/casefiles/notes" "$F/casefiles/mail"
printf '# Lease dispute\n\nThe landlord refused to return the deposit. See [[inspection]].\n' >"$F/casefiles/notes/lease.md"
printf '# Inspection\n\nMould in the bathroom, photographed in March. Related: [[lease]].\n' >"$F/casefiles/notes/inspection.md"
printf 'From: Dana <dana@example.test>\nTo: Owner <owner@example.test>\nSubject: Deposit not returned\nDate: Tue, 03 Mar 2026 10:00:00 +0000\nMessage-ID: <1@example.test>\nContent-Type: text/plain; charset=utf-8\n\nThe deposit of 1200 has still not been returned.\n' >"$F/casefiles/mail/0001.eml"
F_URL="http://localhost:$PORT"
"$XERJ_BIN" brain "$F/casefiles" --url "$F_URL" --data-dir "$F/data" --no-open --disable-feedback >"$F/brain.out" 2>&1; RC=$?
[ -s "$F/data/server.pid" ] && PIDS+=("$(cat "$F/data/server.pid")")
{ [ $RC -eq 0 ] || [ $RC -eq 3 ]; } && ok "xerj brain indexed the folder (exit $RC)" || { bad "xerj brain failed (rc=$RC)"; tail -20 "$F/brain.out"; }
HINT="$(grep -F 'share it: xerj share' "$F/brain.out" | head -1)"
{ printf '%s' "$HINT" | grep -qF -- "--url $F_URL" && printf '%s' "$HINT" | grep -qF -- "--data-dir $F/data"; } \
  && ok "xerj brain prints a pasteable share hint (with --url and --data-dir)" || bad "share hint: $HINT"
OUT="$("$XERJ_BIN" share "$F/casefiles" --url "$F_URL" --data-dir "$F/data" --json --disable-feedback 2>"$F/share.err")"; RC=$?
python3 - "$F_URL" "$OUT" <<'PYEOF' && ok "folder → brain → indices; the guest can search it and walk its links, and nothing else" || bad "folder share (rc=$RC): $(cat "$F/share.err")"
import json, sys, urllib.request, urllib.error
url, created = sys.argv[1], json.loads(sys.argv[2])
assert created["brain"] == "casefiles" and created["indices"], created
def call(method, path, body=None, key=None):
    h = {"content-type": "application/json"}
    if key: h["authorization"] = "ApiKey " + key
    req = urllib.request.Request(url + path, data=None if body is None else json.dumps(body).encode(), method=method, headers=h)
    try:
        r = urllib.request.urlopen(req, timeout=30); return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, {}
s, claim = call("POST", "/_share/claim", {"id": created["share_id"], "passcode": created["passcode"]})
assert s == 200 and claim["brain"] == "casefiles", (s, claim)
key, index = claim["api_key"], claim["index"]
s, r = call("POST", f"/{index}/_search", {"query": {"simple_query_string": {"query": "deposit"}}}, key)
assert s == 200 and r["hits"]["total"]["value"] >= 2, (s, r)
s, r = call("GET", "/_graph/casefiles/overview", None, key)
assert s == 200 and r["edges"]["live"] >= 2 and r["nodes"]["total"] >= 3, (s, r)
for path in ("/autoindex-catalog/_search", "/_cat/indices", "/_share"):
    s, _ = call("GET", path, None, key)
    assert s == 403, (path, s)
PYEOF

echo
if [ "$FAILED" -eq 0 ]; then echo "share-links smoke: all phases passed"; else echo "share-links smoke: $FAILED failure(s)"; fi
exit "$FAILED"
