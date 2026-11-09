#!/usr/bin/env bash
# Behaviour test for landing/get when a download is refused (#2460).
#
# Agents running `curl | sh` behind a proxy, a WAF challenge or GitHub's
# anonymous rate limit get a 4xx from the release download and cannot pass a
# challenge. The installer must then exit non-zero, install nothing, name the
# HTTP status, and print a manual install that works as printed and fails
# closed on a bad digest. This runs the real script under sh against a local
# stand-in for github.com: shims for curl and wget rewrite the github.com URLs
# to it and exec the real tool, so the script's own download code is what runs.
#
# What this does NOT prove: what xerj.org/get itself answers (that is
# Cloudflare configuration, not this script), or the macOS / Windows branches
# (shasum, unzip) of the printed recipe.
#
#   bash .github/scripts/installer-download-failure-test.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
GET="$REPO/landing/get"
command -v python3 >/dev/null || { echo "::error::python3 is required"; exit 1; }
command -v curl >/dev/null || { echo "::error::curl is required"; exit 1; }

W="$(mktemp -d)"
SPID=''
cleanup() { [ -n "$SPID" ] && kill "$SPID" 2>/dev/null; rm -rf "$W"; }
trap cleanup EXIT

FAIL=0
ok()  { echo "ok   - $*"; }
bad() { echo "::error::FAIL - $*"; FAIL=1; }

# The host's target, as landing/get will compute it (Linux runners only).
case "$(uname -m)" in x86_64|amd64) cpu=x86_64 ;; aarch64|arm64) cpu=aarch64 ;; *) echo "SKIP: unsupported CPU"; exit 0 ;; esac
[ "$(uname -s)" = Linux ] || { echo "SKIP: this test runs on Linux"; exit 0; }
TAG=v1.0.0-rc.0
STAGE="xerj-${TAG#v}-${cpu}-unknown-linux-musl"
ASSET="$STAGE.tar.gz"

# A fake release: a shell script that prints a version.
mkdir -p "$W/srv" "$W/stage/$STAGE" "$W/bin" "$W/binw"
printf '#!/bin/sh\necho "xerj %s (test stand-in)"\n' "$TAG" > "$W/stage/$STAGE/xerj"
chmod +x "$W/stage/$STAGE/xerj"
tar -czf "$W/srv/$ASSET" -C "$W/stage" "$STAGE"
(cd "$W/srv" && sha256sum "$ASSET" > "$ASSET.sha256")

cat > "$W/server.py" <<PY
import http.server, os, sys
SRV, TAG = sys.argv[2], "$TAG"
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body=b'', extra=None):
        self.send_response(code)
        self.send_header('content-length', str(len(body)))
        for k, v in (extra or {}).items(): self.send_header(k, v)
        self.end_headers()
        if self.command != 'HEAD': self.wfile.write(body)
    def do_HEAD(self): self.do_GET()
    def do_GET(self):
        mode, _, p = self.path.lstrip('/').partition('/')
        p = '/' + p
        if p.startswith('/api/repos/'):
            if mode in ('latest403', 'api403'): return self.send(403, b'{"message":"API rate limit exceeded"}')
            return self.send(200, ('{"tag_name": "%s"}' % TAG).encode())
        if p.endswith('/releases/latest'):
            if mode == 'latest403': return self.send(403, b'<html>Just a moment...</html>')
            return self.send(302, extra={'location': '/%s/xerj-org/xerj/releases/tag/%s' % (mode, TAG)})
        if '/releases/tag/' in p: return self.send(200, b'release page')
        if '/releases/download/' in p:
            name = p.rsplit('/', 1)[1]
            sha = name.endswith('.sha256')
            if mode == 'asset403' and not sha: return self.send(403, b'<html>Just a moment...</html>')
            if mode == 'sha403' and sha: return self.send(403, b'Forbidden')
            if mode == 'asset404' and not sha: return self.send(404, b'Not Found')
            if mode == 'badsha' and sha: return self.send(200, ('0' * 64 + '  ' + name[:-7] + '\n').encode())
            f = os.path.join(SRV, name)
            if os.path.isfile(f): return self.send(200, open(f, 'rb').read())
        self.send(404, b'Not Found')
s = http.server.ThreadingHTTPServer(('127.0.0.1', 0), H)
open(sys.argv[1], 'w').write(str(s.server_address[1]))
s.serve_forever()
PY
python3 -I "$W/server.py" "$W/port" "$W/srv" & SPID=$!
for _ in $(seq 50); do [ -s "$W/port" ] && break; sleep 0.1; done
PORT="$(cat "$W/port")" || { echo "::error::fake server did not start"; exit 1; }

# Shims: send https://github.com/... to http://127.0.0.1:PORT/$MODE/... (and
# api.github.com to .../$MODE/api/..., so no run touches the network) and
# exec the real tool. MODE picks the server's behaviour for that run.
for tool in curl wget; do
  real="$(command -v "$tool" || true)"
  [ -n "$real" ] || continue
  cat > "$W/bin/$tool" <<SH
#!/bin/sh
n=\$#; i=0
while [ \$i -lt \$n ]; do
  a=\$1; shift
  case "\$a" in
    https://github.com/*)     a="http://127.0.0.1:$PORT/\$MODE/\${a#https://github.com/}" ;;
    https://api.github.com/*) a="http://127.0.0.1:$PORT/\$MODE/api/\${a#https://api.github.com/}" ;;
  esac
  set -- "\$@" "\$a"; i=\$((i+1))
done
exec "$real" "\$@"
SH
  chmod +x "$W/bin/$tool"
done
# A PATH with wget but no curl, for the wget branch of the script.
if [ -x "$W/bin/wget" ]; then
  cp "$W/bin/wget" "$W/binw/wget"
  for t in sh tar gzip uname mktemp awk sed tr head tail sha256sum rm cp mv mkdir chmod find cat cut; do
    ln -s "$(command -v "$t")" "$W/binw/$t"
  done
fi

# run_get MODE BINDIR [VAR=value ...] -> sets OUT, RC, INSTALLED
run_get() {
  local h mode="$1" path="$2"; shift 2
  h="$(mktemp -d "$W/home.XXXX")"
  OUT="$(cd "$h" && env -i HOME="$h" PATH="$path" MODE="$mode" "$@" sh "$GET" 2>&1)"; RC=$?
  INSTALLED=no; [ -e "$h/.local/bin/xerj" ] && INSTALLED=yes
}
# recipe_of: the by-hand install printed in $OUT
recipe_of() { printf '%s\n' "$OUT" | sed -n '/^ *base=/,/--version$/p' | sed 's/^ *//'; }
has() { case "$OUT" in *"$1"*) return 0 ;; esac; return 1; }
PATHC="$W/bin:/usr/bin:/bin"

# 1. Control: the normal install still works.
run_get ok "$PATHC"
if [ $RC = 0 ] && [ $INSTALLED = yes ] && has "sha256 verified"; then ok "normal install"; else bad "normal install (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi

# 2. Binary download refused with 403.
run_get asset403 "$PATHC"
if [ $RC != 0 ] && [ $INSTALLED = no ] && has "failed (HTTP 403)" && has "refused this client" \
   && has "gh release download $TAG -R xerj-org/xerj -p $ASSET -p $ASSET.sha256"; then
  ok "403 on the binary: non-zero, nothing installed, status and fallback printed"
else bad "403 on the binary (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi
RECIPE="$(recipe_of)"
[ -n "$RECIPE" ] || bad "no manual install recipe in the 403 output"

# 3. Checksum download refused with 403: same treatment, still fail closed.
run_get sha403 "$PATHC"
if [ $RC != 0 ] && [ $INSTALLED = no ] && has "$ASSET.sha256" && has "failed (HTTP 403)" && has "base="; then
  ok "403 on the checksum: non-zero, nothing installed, status and fallback printed"
else bad "403 on the checksum (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi

# 3b. The latest-release lookup refused (redirect and API both 403).
run_get latest403 "$PATHC"
if [ $RC != 0 ] && [ $INSTALLED = no ] && has "releases/latest: HTTP 403" && has "api.github.com: HTTP 403" \
   && has "refused this client" && has "gh release view -R xerj-org/xerj"; then
  ok "403 on the release lookup: status and fallback printed"
else bad "403 on the release lookup (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi

# 3c. XERJ_INSTALL_DIR is carried into the recipe (absolute, quoted).
run_get asset403 "$PATHC" "XERJ_INSTALL_DIR=/opt/it's here/bin"
if has "d='/opt/it'\\''s here/bin'"; then ok "recipe installs to XERJ_INSTALL_DIR"; else bad "recipe ignores XERJ_INSTALL_DIR"; echo "$OUT"; fi
run_get asset403 "$PATHC" "XERJ_INSTALL_DIR=rel dir"
IRECIPE="$(recipe_of)"
case "$IRECIPE" in *"d='$W/home."*"/rel dir'"*) ok "relative XERJ_INSTALL_DIR made absolute" ;; *) bad "relative XERJ_INSTALL_DIR: $IRECIPE" ;; esac

# 4. 404 means the file is missing: no 'refused' wording, no recipe that would 404 too.
run_get asset404 "$PATHC"
if [ $RC != 0 ] && [ $INSTALLED = no ] && has "failed (HTTP 404)" && has "no such file" && ! has "refused" && ! has "base="; then
  ok "404 on the binary: reported as missing"
else bad "404 on the binary (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi

# 5. The wget branch reports the status too.
if [ -x "$W/binw/wget" ]; then
  run_get asset403 "$W/binw"
  # shellcheck disable=SC2016  # literal recipe text, not an expansion
  if [ $RC != 0 ] && [ $INSTALLED = no ] && has "failed (HTTP 403)" && has 'wget -q "$base/$a"' && ! has "curl -fsSLO"; then
    ok "403 via wget: status and a wget recipe printed"
  else bad "403 via wget (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi
  WRECIPE="$(recipe_of)"
  run_get api403 "$W/binw"
  if [ $RC != 0 ] && [ $INSTALLED = no ] && has "api.github.com: HTTP 403" && has "refused this client" && has "XERJ_VERSION=vX.Y.Z"; then
    ok "403 on the release lookup via wget (api.github.com): status and fallback printed"
  else bad "403 on the release lookup via wget (rc=$RC installed=$INSTALLED)"; echo "$OUT"; fi
else
  echo "SKIP: wget not installed"
fi

# 6. The printed recipe works as printed, and fails closed on a bad digest,
#    also when its exit status is discarded.
run_recipe() {  # MODE SUFFIX [RECIPE PATH] -> RRC, RINSTALLED, ROUT
  local h; h="$(mktemp -d "$W/home.XXXX")"
  ROUT="$(cd "$h" && env -i HOME="$h" TMPDIR="$h" PATH="${4:-$PATHC}" MODE="$1" sh -c "${3:-$RECIPE}$2" 2>&1)"; RRC=$?
  RINSTALLED=no; [ -e "$h/.local/bin/xerj" ] && RINSTALLED=yes
}
if [ -n "$RECIPE" ]; then
  run_recipe ok ''
  if [ $RRC = 0 ] && [ $RINSTALLED = yes ] && [ "$ROUT" = "xerj $TAG (test stand-in)" ]; then ok "printed recipe installs"; else bad "printed recipe (rc=$RRC installed=$RINSTALLED): $ROUT"; fi
  run_recipe badsha ''
  if [ $RRC != 0 ] && [ $RINSTALLED = no ]; then ok "printed recipe refuses a bad digest"; else bad "recipe with bad digest (rc=$RRC installed=$RINSTALLED)"; fi
  run_recipe badsha '; true'
  if [ $RINSTALLED = no ]; then ok "bad digest installs nothing with the exit status discarded"; else bad "bad digest installed with exit status discarded"; fi
fi
if [ -n "${WRECIPE:-}" ]; then
  run_recipe ok '' "$WRECIPE" "$W/binw"
  if [ $RRC = 0 ] && [ $RINSTALLED = yes ] && [ "$ROUT" = "xerj $TAG (test stand-in)" ]; then ok "wget recipe installs on a wget-only PATH"; else bad "wget recipe (rc=$RRC installed=$RINSTALLED): $ROUT"; fi
  run_recipe badsha '' "$WRECIPE" "$W/binw"
  if [ $RRC != 0 ] && [ $RINSTALLED = no ]; then ok "wget recipe refuses a bad digest"; else bad "wget recipe with bad digest (rc=$RRC installed=$RINSTALLED)"; fi
fi
if [ -n "${IRECIPE:-}" ]; then
  h="$(printf '%s\n' "$IRECIPE" | sed -n "s|.*d='\(.*\)/rel dir'.*|\1|p")"
  ROUT="$(cd "$W" && env -i HOME="$h" TMPDIR="$h" PATH="$PATHC" MODE=ok sh -c "$IRECIPE" 2>&1)"; RRC=$?
  if [ $RRC = 0 ] && [ -x "$h/rel dir/xerj" ]; then ok "recipe installs into the XERJ_INSTALL_DIR it names"; else bad "XERJ_INSTALL_DIR recipe (rc=$RRC): $ROUT"; fi
fi

echo "──"
if [ "$FAIL" = 0 ]; then echo "INSTALLER DOWNLOAD-FAILURE TEST PASSED"; else echo "INSTALLER DOWNLOAD-FAILURE TEST FAILED"; exit 1; fi
