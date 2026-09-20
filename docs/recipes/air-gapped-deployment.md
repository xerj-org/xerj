# Air-gapped deployment: run XERJ without runtime internet

This is a short, Linux x86_64-musl procedure. It stages one operator-approved
release, installs the service as an unprivileged account, and keeps the node on
loopback. The base path is lexical and needs no model files. Neural embedding is
an optional, separately staged path.

## Runtime boundary

- The default embedding mode is **lexical feature hashing**. It needs no model,
  no embedding service, and no network connection.
- Neural mode is optional. Without `embedding.local_model_dir`, the first
  operation that needs an embedding downloads `config.json`, `tokenizer.json`,
  and `model.safetensors` for `sentence-transformers/all-MiniLM-L6-v2` from the
  Hugging Face Hub. With that directory set, XERJ reads those files locally.
- `proxy` calls only the configured OpenAI-compatible
  `embedding.default_endpoint`. The experimental `onnx-experimental` backend
  needs an ONNX-enabled build and explicit local assets; neither is the base
  release path.

The running node makes no telemetry, update-check, or license-activation call.
Release-download analytics are a property of the connected website/GitHub
staging surface, not of a running XERJ process. Getting the software at all
means downloading a release archive and its checksum from GitHub, which is why
section 1 below runs on a connected machine: an offline operator must stage
those two files before entering the enclave.

## Defaults and boundaries

| Component | Default | Air-gapped meaning |
|---|---|---|
| Embedding | `auto`: lexical unless an endpoint is configured | No model or network for lexical mode |
| Neural model | Off | Set `local_model_dir` before selecting `neural` |
| External proxy | Disabled (`default_endpoint = ""`) | Only the configured endpoint is contacted |
| ONNX | Experimental and not in the standard release | Build the feature and provide local assets explicitly |
| WAL tap | Disabled | It remains inert unless enabled with a target URL; see the durable overlay note below |
| Rerank provider | Inert: no key is configured | The only search-time feature that POSTs document text to a third party. The external proxy and WAL tap rows above also send text off the node when enabled; the neural model download and the cluster transport carry no document text; all of them are off by default. It stays inert with no `rerank.api_key` and no `TYPESAFE_API_KEY` in the service environment, and a search must carry a `rerank` block to trigger it. Set `[rerank] enabled = false` to refuse it outright, whatever the environment holds; `GET /_xerj/rerank` confirms `"enabled": false`. See [RERANK.md](../RERANK.md#every-way-data-leaves-a-xerj-node) for every outbound connection a node can open |
| Object storage backend | Refused: `storage.backend = "s3"` does not start | The `S3Backend` client exists (#966) but nothing on the segment path constructs it, so an air-gapped node opens no bucket connection. If #965 wires it, the endpoint you configure is the only host contacted |
| `xerj autoindex s3://` | Off: only when you pass an `s3://` root | A client, not the node. It contacts the endpoint you name (`--endpoint-url`) and nothing else; a folder root contacts nothing |
| Cluster | Disabled | Single-node startup does not initialize the Raft transport |
| REST / ES-compatible listeners | `127.0.0.1` | The default is loopback-only |
| `autoindex` and MCP clients | Localhost defaults | They connect to `localhost`; they do not create a public listener |
| Runtime telemetry/update/license activation | None | No outbound call is made by the running binary for these purposes |

This is a statement of defaults, not a claim that every browser UI request is
offline. **The bundled Console is three embedded documents — `index.html`,
`login.html`, and `setup.html`, served under `/_xerj-console/` — and each one
carries the same three external Google Fonts link elements (two preconnects and
one stylesheet; the stylesheet may fetch additional font files), so nine in
total.** An operator rewriting that HTML has to patch all three; `login.html`
and `setup.html` are the two a browser reaches first. Blocked requests fall back
to system fonts. The engine and APIs continue to work.

This procedure does not claim that egress was measured here. If your policy
requires that boundary, apply an egress-deny rule and inspect firewall/log
counters during the acceptance checks.

## 1. Stage an approved release on a connected machine

Do this on a machine that is allowed to reach GitHub. Set `TAG` to the
operator-approved `vX.Y.Z` release tag before running; there is no moving
release alias in this procedure. The release workflow names each archive from
the version without the `v` prefix, target triple, and extension. The matching
`.sha256` is a separate per-archive asset.

The following stages the Linux x86_64 musl archive only.

```bash
(
  set -eu

  : "${TAG:?set TAG to an operator-approved vX.Y.Z release tag}"
  if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
    echo "TAG must match vX.Y.Z with optional prerelease/build metadata: $TAG" >&2
    exit 2
  fi
  VERSION="${TAG#v}"
  TARGET="x86_64-unknown-linux-musl"
  EXT="tar.gz"
  STAGE="xerj-${VERSION}-${TARGET}"
  ASSET="${STAGE}.${EXT}"
  BASE="https://github.com/xerj-org/xerj/releases/download/${TAG}"
  OUT="${OUT:-$PWD/xerj-airgap-${VERSION}-${TARGET}}"

  mkdir -p "$OUT/release"
  curl -fL --retry 3 -o "$OUT/release/$ASSET" "$BASE/$ASSET"
  curl -fL --retry 3 -o "$OUT/release/$ASSET.sha256" "$BASE/$ASSET.sha256"

  cd "$OUT/release"
  trap 'rm -rf "$STAGE"' EXIT
  want="$(sha256sum "$ASSET" | cut -d ' ' -f 1)"
  if [[ ! "$want" =~ ^[0-9a-f]{64}$ ]]; then
    echo "could not compute a digest for $ASSET" >&2
    exit 1
  fi
  LC_ALL=C grep -qxF -e "$want  $ASSET" -e "$want *$ASSET" \
    < <(tr -d '\r' < "$ASSET.sha256")

  tar -xzf "$ASSET"
  test -x "$STAGE/xerj"
  "$STAGE/xerj" --version
  echo "staged $ASSET ($want) in $OUT"
)
```

The checksum proves that the archive matches the digest published beside that
archive in the same GitHub release. It is an integrity check, not an
independent signature, provenance statement, or attestation. If your policy
requires signed releases or an external attestation, obtain and verify that
evidence separately; `.sha256` alone does not provide it.

`"$STAGE/xerj" --version` executes the staged binary, so this block wants a
staging host of the same platform as the target — Linux x86_64 for the archive
above. On any other staging host that one line fails and the block stops with
the archive and its `.sha256` already downloaded; either drop the line, or stage
a different target, which means changing `TARGET` here **and** the triple
written into `STAGE` in the enclave block below, because that one is spelled out
rather than derived.

The extracted tree is removed by the `trap` after the `cd` on a clean exit —
success, `set -e`, and a failed digest. An **interrupted** run may or may
not leave the tree behind: whether the trap runs to completion depends on
how and when the interrupt lands, and the honest summary is that you cannot
tell from the outside. So do not use the trap to decide safety. The rule
that always holds: only the archive and its `.sha256` should cross the
airgap, and **if the block did not print its `staged …` line, treat the
staging directory as unverified — delete it, or inspect it, before
transferring anything.** The trap is a convenience for the normal exits,
not a guarantee across interrupts.

Three earlier versions of this paragraph were wrong, and each is worth knowing
because two of them are about where the trap sits and the third is about how
this was measured:

- One ran `rm -rf` as the **last line** of the block and claimed the tree was
  "removed either way". False on exactly the wrong-platform path it was written
  for: `set -e` ended the subshell at the failing line and never reached the
  `rm`. Measured then: exec-format `rc=126`, missing exec bit `rc=1`, non-zero
  exit `rc=7`, tree left behind in all three, removed only on success.
- The next used `trap 'rm -rf "$OUT/release/$STAGE"' EXIT` set **after** the
  `cd`. Trap bodies run at exit in the directory the subshell is in by then, so
  an `OUT` given as a relative path — which the `OUT="${OUT:-…}"` hook above
  invites — resolved to `$OUT/release/$OUT/release/$STAGE`, a path that never
  exists. `rm -rf` returns 0 on a missing path, so it failed silently on
  **every** path, including the successful one. Measured: with `OUT` absolute
  the tree went; with `OUT=stagedir` it survived all four cases.

- The third claimed an `EXIT` trap "does not run on `SIGTERM` or `SIGHUP`, so a
  dropped session leaves the tree". That was measured wrong, but so were two
  attempts to correct it: whether the trap runs on an interrupt turned out to
  depend on details the measurements kept getting different answers on —
  which process was signalled, and whether a controlling terminal was
  attached. The honest conclusion is the one now in the lead: do not assert
  what the trap does under interrupts, and trust the `staged …` line instead.
  Several of this page's earlier wrong sentences trace to a harness that
  signalled or observed the wrong thing, which is the whole reason the recipe
  keys on the printed line and not on the trap.

Hence the form above: the body is `"$STAGE"`, relative to the directory the
block already `cd`-ed into and never leaves, and the `trap` is installed
immediately after that `cd` — before the digest check, not just before `tar`.
That ordering matters, because the digest-mismatch exit is the one this section
exists for: with the trap installed after the check, a stale `$STAGE` left by an
earlier interrupted run survived exactly there, and the next instruction on this
page is to transfer `$OUT`.

**Transfer `$OUT` itself, and land it in the enclave at `/opt/xerj-staging`**,
so that the archive is at `/opt/xerj-staging/release/` — that is the path the
enclave blocks below read, and they are written as fixed paths on purpose, so
that a mistyped variable cannot silently point the install at somewhere else.

Then set the same approved `TAG` in the enclave and verify before extracting:

```bash
(
  set -eu

  : "${TAG:?set TAG to the same operator-approved release tag}"
  VERSION="${TAG#v}"
  STAGE="xerj-${VERSION}-x86_64-unknown-linux-musl"
  ASSET="${STAGE}.tar.gz"

  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT
  cp "/opt/xerj-staging/release/$ASSET" "$work/$ASSET"
  cp "/opt/xerj-staging/release/$ASSET.sha256" "$work/$ASSET.sha256"
  cd "$work"

  want="$(sha256sum "$ASSET" | cut -d ' ' -f 1)"
  if [[ ! "$want" =~ ^[0-9a-f]{64}$ ]]; then
    echo "could not compute a digest for $ASSET" >&2
    exit 1
  fi
  LC_ALL=C grep -qxF -e "$want  $ASSET" -e "$want *$ASSET" \
    < <(tr -d '\r' < "$ASSET.sha256")

  tar -xzf "$ASSET"
  test -x "$STAGE/xerj"

  if ! getent passwd xerj >/dev/null; then
    sudo useradd --system --user-group --home-dir /var/lib/xerj \
      --shell /sbin/nologin xerj
  fi
  sudo install -d -m 0755 /opt/xerj/bin /etc/xerj
  sudo install -d -o xerj -g xerj -m 0750 /var/lib/xerj
  sudo install -m 0755 "$STAGE/xerj" /opt/xerj/bin/xerj
  echo "installed $ASSET ($want)"
)
```

Seven details here are load-bearing, and each replaces something that looked
right and was not. The count has been wrong before: rounds of review added
paragraphs without touching the number above them, so if you add an eighth,
change this sentence in the same commit.

**The digest is computed here and the file is searched for it, not the other way
round.** `sha256sum -c` reads the filename out of the `.sha256` — a file that
crossed the airgap on the same medium as the archive — and reports success for
whatever it happens to name. It skips `#` comment lines silently and exits `0`
when at least one line verified and no *listed* line failed — so a `.sha256`
carrying a valid digest for some other file plus a comment mentioning the
archive passes at `0` while the archive is never hashed. (It does exit `1` if a
line it does read fails; the attack does not need one. Measured: comment plus a
valid line for an unrelated file, `rc=0`; one good line plus one failing line,
`rc=1`.)
Filtering that file first does not fix it either: `awk '$2 == asset'` compares
one whitespace-delimited token, while `sha256sum` takes the whole rest of the
line as the filename, so a line naming `$ASSET decoy` satisfies the filter and
hashes a decoy. Building the expected line in the shell and demanding it with
`grep -qxF` removes the disagreement — there is no field-splitting left for the
two programs to differ about, and the asset name is never interpreted. The two
patterns accept GNU text mode and `-b` binary mode; `tr -d '\r'` accepts a
checksum file written on Windows.

**`set -eu` is inside the subshell.** At the top of the block it kills the
operator's login shell on a bad digest — actively harmful over a serial console,
where the natural recovery is to re-paste without it, which is the fail-open
form: with errexit gone, a failed `grep -qxF` no longer stops `tar -xzf`, and
the block extracts and runs the archive it just refused. Inside, a failure ends
the subshell and the operator keeps their session, so that pressure never
arises. Every block on this page that decides something now has its `set -eu`
inside its own parentheses. The blocks that must leave variables behind for a
later paste — section 4's checks, which set `KEY` and `XERJ`, and the `wal_tap`
block that reads `KEY` — carry no `set -eu` at all. Nothing in them decides
whether anything gets installed, and every command reports its own failure, so
none of them needs errexit armed in the operator's session.

That is the property that matters, and it is the only one stated here on
purpose. Three earlier attempts described these blocks by counting them, then by
listing two command types, then by listing three — each enumeration was wrong in
a new way, the last of them missing a single `export`. A property that has to be
re-derived every time the blocks change is a worse thing to write down than the
invariant they are chosen to satisfy.

**The archive is copied once, then hashed and extracted from that copy.** This
is the defect eight earlier versions of this recipe shared without noticing.
Whatever the digest check looked like, the archive at the staging path was
opened TWICE — once by `sha256sum`, once by `tar` — and anyone who can write
that directory swaps the file between the two opens. The genuine, untouched
`.sha256` then verifies, the block prints the genuine digest, exits `0`, and
installs a backdoor. Reproduced with plain regular files, no FIFOs, on a
correctly configured system. Copying first collapses that to a single read: the
bytes that are hashed are the same bytes that are extracted, because they are
the same file, and it is a file in a directory the attacker cannot reach.

**The block that verifies is the block that installs.** Six versions before that
handed the verified tree to a later section through a path on disk, and every
one was broken through the handoff rather than through the digest. There is no
handoff now, so nothing later has to be trusted and no exit status has to be
checked.

**Everything runs INSIDE the subshell, under `set -eu` — including the `TAG`
guard.** Bash does not terminate an *interactive* shell when a `${var:?}`
expansion fails, so with the guard above the `(` a pasted block printed its
message and carried on: `VERSION` empty, `ASSET` becoming
`xerj--x86_64-unknown-linux-musl.tar.gz`, and inside the subshell that variable
IS set, so `set -u` never fired either. An attacker who can write the staging
directory needs only to leave that name and a matching `.sha256` beside the
approved pair, and the block verifies their file against their digest and
installs it at exit 0. A subshell is not interactive, so the same guard inside
it ends the block.

**`mktemp -d` runs inside it too.** It was outside in an
earlier version, where a failure was not fatal and its status was never checked:
`work` became the empty string, both `cp` lines failed, and `cd ""` returns 0 in
bash without changing directory — so the block hashed and extracted `$ASSET`
from whatever directory the operator happened to be in. If that was the staging
directory, it silently became the double read again, printed the genuine digest
and installed attacker bytes at exit 0.

**The work directory must be writable by you and by nobody else.** `mktemp -d`
uses `TMPDIR`, which is safe when that is sticky and not attacker-writable — the
default `/tmp` on any normal system. It is not safe if `/tmp` has been made
world-writable without the sticky bit, or if `TMPDIR` points somewhere the
attacker can write: renaming a directory entry needs write on the parent, so the
work directory can be swapped however restrictive its own mode is. If your
enclave is unusual here, make a private directory and point `TMPDIR` at it:

```bash
install -d -m 0700 "$HOME/xerj-verify"
export TMPDIR="$HOME/xerj-verify"
```

An earlier version of this page said "set `TMPDIR` to a root-owned directory",
which is worse than no advice. Root ownership was never the problem — the
default `/tmp` is root-owned and is exactly right, because the sticky bit stops
anyone else renaming your work directory. The problem is a directory YOU cannot
write: `mktemp` then fails, and with that failure unchecked the sentence was
itself the trigger for the defect above. The property to aim for is writable by
you and renameable by nobody else.

An attacker who rewrites the archive AND recomputes its `.sha256` still passes,
and always will: that is the one case a checksum cannot answer, and the prose
above already says this is an integrity check and not a signature.

## 2. Configure the base lexical node

The account and directories are created by the block above, alongside the
install; the ownership shown there is an example, so use the equivalent
account-management command on the target Linux image. No model directory is
created for the lexical deployment.

Create `/etc/xerj/xerj.toml` with the complete base configuration. Explicitly
selecting lexical mode and an empty endpoint prevents an accidental proxy or
neural dependency:

```toml
[server]
bind_address = "127.0.0.1"
data_dir = "/var/lib/xerj"
es_compat_port = 9200

[auth]
enabled = true

[embedding]
mode = "lexical"
default_endpoint = ""
default_model = ""
```

Start it as the service user. On first start, the authenticated admin key is
written to `/var/lib/xerj/admin.key` with restrictive permissions.

```bash
sudo -u xerj /opt/xerj/bin/xerj --config /etc/xerj/xerj.toml
```

This runs in the foreground and stops when you log out, so it is a first-start
check rather than a deployment. Section 4 needs the node reachable while you
type into a shell: use a second terminal, or supervise it. The unit file is in
[Operations](../../landing/docs/operations.html); this page deliberately does
not carry a second copy of it to drift out of date. Point its `ExecStart` at
`/opt/xerj/bin/xerj`, which is where the block above installs — that unit is
written for the one-line installer's `/usr/local/bin/xerj`, and a unit copied
across unedited fails to start.

Keep the listener on loopback. A network-facing listener requires the separate
production TLS/auth procedure; this recipe does not teach a cleartext opt-out.

## 3. Optional neural model

Do this only when the enclave needs neural semantics. On the connected staging
machine, download the three files consumed by the built-in loader and checksum
them for the transfer:

```bash
(
  set -eu

  : "${OUT:?set OUT to the staging directory section 1 printed}"
  MODEL="$OUT/model/all-MiniLM-L6-v2"
  mkdir -p "$MODEL"
  for FILE in config.json tokenizer.json model.safetensors; do
    curl -fL --retry 3 -o "$MODEL/$FILE" \
      "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/$FILE"
  done
  cd "$MODEL"
  sha256sum config.json tokenizer.json model.safetensors > ../model.sha256
  echo "staged all-MiniLM-L6-v2 in $MODEL"
)
```

Section 1 runs in a subshell, so `OUT` does not survive it — set `OUT` here to
the path that block printed. Transfer `$OUT/model` with the release, in the same
`/opt/xerj-staging` directory, so that the files land at
`/opt/xerj-staging/model/`. Verify them inside the enclave before installing
anything — the same fail-closed rule as section 1, and the reason the check is a
command here rather than a sentence:

```bash
(
  set -eu

  mwork="$(mktemp -d)"
  trap 'rm -rf "$mwork"' EXIT
  cp /opt/xerj-staging/model/all-MiniLM-L6-v2/{config.json,tokenizer.json,model.safetensors} \
    "$mwork/"
  cp /opt/xerj-staging/model/model.sha256 "$mwork/model.sha256"
  cd "$mwork"

  for f in config.json tokenizer.json model.safetensors; do
    want="$(sha256sum "$f" | cut -d ' ' -f 1)"
    if [[ ! "$want" =~ ^[0-9a-f]{64}$ ]]; then
      echo "could not compute a digest for $f" >&2
      exit 1
    fi
    LC_ALL=C grep -qxF -e "$want  $f" -e "$want *$f" \
      < <(tr -d '\r' < model.sha256)
  done

  sudo install -d -o xerj -g xerj -m 0750 /opt/xerj/models/all-MiniLM-L6-v2
  sudo install -o xerj -g xerj -m 0640 \
    config.json tokenizer.json model.safetensors \
    /opt/xerj/models/all-MiniLM-L6-v2/
  echo "installed all-MiniLM-L6-v2"
)
```

The same shape as section 1, and for the same reason: `model.sha256` crossed the
airgap on the same medium as the files it describes, so `sha256sum -c` reading
filenames out of it would verify whatever it happened to name. Each digest is
computed here and the file is searched for that exact line, and every one of the
three must be found — a transfer that dropped or truncated one fails rather than
installing a partial model — and, as in section 1, the block that verifies is
the block that installs, so a failed check leaves nothing behind for a later
paste to install. Then change only the embedding block:

```toml
[embedding]
mode = "neural"
local_model_dir = "/opt/xerj/models/all-MiniLM-L6-v2"
```

XERJ does not independently pin or verify model checksums; the transferred
digest is an operator supply-chain check, not a XERJ signature. If you only
need lexical search, omit this section and all model assets.

## 4. Authenticate local clients and verify

These checks are intentionally operator-run checks, not a claim that this
documentation was live-tested against a disconnected firewall. They verify the
actual binary and local files in your enclave.

```bash
XERJ=/opt/xerj/bin/xerj
DATA=/var/lib/xerj
KEY="$(sudo cat "$DATA/admin.key")"
export XERJ_API_KEY="$KEY"

"$XERJ" --version
curl -fsS -H "Authorization: ApiKey $KEY" \
  http://127.0.0.1:9200/_cluster/health
```

With the lexical node running, index a small semantic document and query it
through the authenticated ES-compatible surface. This semantic query uses the
local lexical embedder and needs zero model files; assert that it returns
`_id` `1`.

```bash
curl -fsS -X PUT "http://127.0.0.1:9200/offline-demo" \
  -H "Authorization: ApiKey $KEY" -H 'Content-Type: application/json' \
  -d '{"mappings":{"properties":{"body":{"type":"semantic_text"}}}}'
curl -fsS -X POST "http://127.0.0.1:9200/offline-demo/_doc/1?refresh=true" \
  -H "Authorization: ApiKey $KEY" -H 'Content-Type: application/json' \
  -d '{"body":"A local lexical node can answer this without a network service."}'
curl -fsS -X POST "http://127.0.0.1:9200/offline-demo/_search" \
  -H "Authorization: ApiKey $KEY" -H 'Content-Type: application/json' \
  -d '{"query":{"semantic":{"field":"body","query":"local lexical"}}}'
```

Repeat the health and semantic query after restarting the same data directory.
For the stronger boundary check, run the sequence with host egress disabled
and inspect the firewall/log counters; this recipe does not claim that test was
run here.

For MCP, `XERJ_AUTH` and `--auth` are complete Authorization-header values,
including the scheme:

```bash
XERJ_URL=http://127.0.0.1:9200 XERJ_AUTH="ApiKey $KEY" "$XERJ" mcp
"$XERJ" mcp --url http://127.0.0.1:9200 --auth "ApiKey $KEY"
```

For `xerj autoindex`, `XERJ_API_KEY` and `--api-key` take the raw key; the
client adds `Authorization: ApiKey` itself:

```bash
XERJ_API_KEY="$KEY" "$XERJ" autoindex /path/to/folder --url http://127.0.0.1:9200
"$XERJ" autoindex /path/to/folder --url http://127.0.0.1:9200 --api-key "$KEY"
```

## Existing data directories and local clients

The WAL tap and cluster are disabled by default. A runtime `PUT
/_xerj/wal_tap` persists an overlay and can re-enable a tap on restart when a
data directory is reused. Inspect and remove that overlay before an offline run
if the directory is not fresh:

```bash
curl -fsS -H "Authorization: ApiKey $KEY" \
  http://127.0.0.1:9200/_xerj/wal_tap
curl -fsS -X DELETE -H "Authorization: ApiKey $KEY" \
  http://127.0.0.1:9200/_xerj/wal_tap
```

Cluster mode stays off unless explicitly enabled with peers and a shared
`cluster.auth_secret`. `xerj mcp` is a local stdio client that proxies
`XERJ_URL`; `xerj autoindex` walks files visible to the invoking machine. Neither
creates an external listener or silently uploads corpus data.

## What this page does not promise

- An offline release is not a signed or attested software supply chain. Verify
  the archive checksum and apply your organization's release-signing process.
- XERJ does not independently checksum the Hugging Face model files today.
  Stage and verify them according to your organization's model-supply-chain
  policy.
- The bundled Console falls back to system fonts, but its nine external Google
  Fonts link elements — three in each of `index.html`, `login.html`, and
  `setup.html` — remain browser egress attempts unless your deployment removes
  or rewrites all three documents; the stylesheet may fetch additional font
  files.
- This page does not claim a live disconnected-firewall run or measured browser
  egress. The procedure is traced to the config, neural loader, and
  release workflow; execute those checks in your target environment.

Related references:

- [Production deployment](./production-deployment.md) — TLS, API-key auth,
  listener boundaries, health probes, and the broader air-gapped model recipe.
- [Configuration reference](../../landing/docs/config.html) — all TOML keys and
  defaults.
- [Experimental ONNX backend](../EXPERIMENTAL_ONNX.md) — explicit feature/build
  and local asset requirements; not the standard release path.
- [Metrics privacy posture](../../metrics/README.md) — connected distribution
  analytics and what they do not measure.
