# Production deployment: TLS, auth, air-gapped models, and migration

**Use case:** Every other recipe boots XERJ with `--insecure` — no TLS, no
auth — because that is the fastest way to try something. This one shows the
opposite: the hardened posture you actually run in production. It covers the
secure quickstart (TLS + API-key auth), the health-probe and gRPC posture, how
to stage the neural embedder on an air-gapped host, the interim path for
copying data out of a live Elasticsearch, and the single-node-vs-cluster
production stance.

Everything below was verified against a live XERJ binary. The exact `curl`
status codes and log lines quoted are real captured output from that run.

> **Honesty first.** XERJ secures *some* things in the engine (TLS in transit,
> API-key auth, a set of resource circuit-breakers) and deliberately delegates
> others (encryption at rest, RBAC, SSO, tamper-evident audit) to your
> surrounding environment. This recipe is explicit about which is which — see
> [What XERJ does and does not secure](#2-what-xerj-does-and-does-not-secure-itself).

---

## 1. Secure quickstart — TLS + auth in one config file

XERJ is configured by a single TOML file passed with `--config`. The secure
posture is two blocks: `[auth]` (on by default) and `[tls]`.

Create `xerj.toml`:

```toml
[server]
es_compat_port = 9200          # Elasticsearch-wire clients point here
data_dir       = "/var/lib/xerj"
bind_address   = "0.0.0.0"

[auth]
enabled = true                 # this is already the default
# admin_api_key = ""           # leave empty → auto-generated on first run

[tls]
enabled   = true
cert_path = "/var/lib/xerj/xerj.crt"
key_path  = "/var/lib/xerj/xerj.key"
```

Boot it:

```bash
xerj --config /etc/xerj/xerj.toml
```

On first start the admin key is printed **once** (illustrative — yours is a
random 64-hex-character value):

```
╔══════════════════════════════════════════════════╗
║  First-run: admin API key auto-generated         ║
║                                                  ║
║  <64 random hex chars>                           ║
║                                                  ║
║  Keep this secret. Written to:                   ║
║  /var/lib/xerj/admin.key                         ║
╚══════════════════════════════════════════════════╝
```

…followed by the deployment-posture banner (verbatim from a real TLS+auth boot;
listener lines shown, the Data-dir/Console lines between them elided with `…`):

```
 Native REST  :8080 [TLS ]
 ES-compat    :9200 [TLS ]
 gRPC         :8081 [h2c]
 …
 ┌─ Deployment posture (see PATH_TO_100_PCT_v0.6.0_to_v1.0.md) ──
 │ ✓  TLS:    in-process rustls termination active (REST + ES-compat)
 │           (self-signed by default — supply a CA cert for production)
 │ ✓  Auth:   single API-key (no RBAC; per-doc / per-field controls roadmap v0.9)
 │ ⚠  Audit:  request tracing only — tamper-evident WORM audit log v0.9
 │ ⚠  Encryption-at-rest: not engine-level — use OS FDE or S3 SSE for now
 └────────────────────────────────────────────────────────────────
```

That banner prints on every start and cannot be suppressed — it is a running
statement of what the engine does and does not cover. The `[TLS ]` tags on the
REST/ES lines and `[h2c]` on gRPC reflect the actual transport of each listener.
The `v0.9` labels are the engine's internal roadmap milestone for those
delegated controls (encryption-at-rest, RBAC, WORM audit); none of them is in
this build — provide them at the layers described in
[section 2](#2-what-xerj-does-and-does-not-secure-itself).

### How auth behaves

* **On by default.** `[auth] enabled` defaults to `true`. Leave `admin_api_key`
  empty and XERJ generates a 64-hex-character key on first run, writes it to
  `<data_dir>/admin.key` with `0600` permissions, and prints it once. Or set
  `admin_api_key` yourself (then no file is written — protect the config file
  with `chmod 600`).
* **One key, full access.** Every request must carry the key; there is **no
  RBAC** — any authenticated caller can read and write every index. Both header
  schemes work:

  ```bash
  curl -k -H "Authorization: ApiKey  $KEY"  https://localhost:9200/_cluster/health   # 200
  curl -k -H "Authorization: Bearer  $KEY"  https://localhost:9200/_cluster/health   # 200
  curl -k                                    https://localhost:9200/_cluster/health   # 401
  curl -k -H "Authorization: ApiKey  wrong"  https://localhost:9200/_cluster/health   # 401
  ```

* **Per-client keys.** `POST /_security/api_key` mints additional keys
  (presented as `ApiKey <base64(id:secret)>`) that can carry an expiry and be
  invalidated. They authenticate as the same superuser — the `role_descriptors`
  in the request are accepted but not enforced.

### How TLS behaves

XERJ terminates TLS **in-process** with rustls on the REST and ES-compat
listeners — there is no separate proxy required just to get HTTPS. Two things
are worth knowing:

1. **You must name `cert_path` and `key_path` when `tls.enabled = true`.**
   Config validation rejects a TLS block with empty paths:

   ```
   Error: config error: tls.cert_path is required when tls.enabled = true
   ```

2. **Self-signed on first run, if the named files don't exist yet.** With TLS
   enabled and the cert/key files absent, XERJ generates a self-signed
   certificate for `localhost` at `<data_dir>/xerj.crt` and
   `<data_dir>/xerj.key` (the private key `0600`) and logs a warning:

   ```
   INFO  generating self-signed TLS certificate for localhost
   WARN  self-signed certificate generated — replace with a real cert for production
   ```

   This is fine for a smoke test but a self-signed `localhost` cert is **not**
   production TLS.

**For production, supply a real certificate.** Put a CA-signed (or internal-CA)
PEM cert and key at the paths you named. XERJ loads them on boot and — if TLS
is enabled but the cert or key is missing or unparseable — **fails loud** and
aborts startup rather than silently downgrading your HTTPS port to cleartext.
A cleartext request to a TLS port simply fails to connect; the port never
answers in plaintext.

> `--insecure` (or `-k`) is the escape hatch every other recipe uses: it forces
> **both** `tls.enabled = false` **and** `auth.enabled = false`. Never pass it
> in production. There is no separate flag that enables TLS — TLS is turned on
> only through the config file.

### Health probes: exempt from auth, *not* from TLS

`/health/live` and `/health/ready` are exempt from authentication so a
Kubernetes kubelet or a Docker `HEALTHCHECK` can probe a hardened node without
credentials — otherwise the moment you set `auth.enabled = true` every probe
gets a 401 and the pod crashloops. Verified against a live auth-on node:

```bash
curl -k https://localhost:9200/health/ready     # 200  (no key needed)
curl -k https://localhost:9200/health/live      # 200  (no key needed)
curl -k https://localhost:9200/_cluster/health  # 401  (every data route still needs the key)
```

The exemption is auth-only. The probe endpoints are still served over whatever
transport the listener uses, so **when TLS is on, probes must speak HTTPS.**
The shipped `Dockerfile` HEALTHCHECK is:

```dockerfile
HEALTHCHECK … CMD curl -fsS http://127.0.0.1:9200/health/ready || exit 1
```

That `http://` probe works as-is for the default (TLS-off, proxy-terminated)
container. If you enable in-process TLS, change the probe to
`https://127.0.0.1:9200/health/ready` and add `-k` for a self-signed cert, or
your container is marked unhealthy forever. Same for a Kubernetes probe: set
`scheme: HTTPS` on the `httpGet`.

### gRPC posture

The gRPC `XerjSearch` service (default `:8081`) always speaks **plaintext
h2c** — the in-process rustls termination covers the REST and ES-compat
listeners only, and tonic is built without its TLS feature. gRPC is **not**
unauthenticated, though: every RPC runs through the same API-key check as the
HTTP surface (credential in the `authorization` metadata, `ApiKey <key>` or
`Bearer <key>`). With `auth.enabled = true`, an unauthenticated or wrong-key
RPC returns `UNAUTHENTICATED`.

Because the gRPC port carries cleartext frames, **terminate TLS in front of
`:8081` at a reverse proxy** (or keep it on a trusted network / mesh) if
clients reach it over an untrusted link. If you don't use gRPC, don't expose
the port.

---

## 2. What XERJ does and does not secure itself

Run XERJ **secure by deployment**: the engine covers transport auth and
transport encryption; your platform provides the rest.

**Engine-level in this build:**

| Control | Status |
|---|---|
| TLS in transit (REST + ES-compat), in-process rustls | ✅ `tls.enabled = true` |
| API-key authentication (single admin key; created keys with expiry/invalidate) | ✅ `auth.enabled = true` |
| gRPC auth (same key, plaintext h2c transport) | ✅ |
| Body-size / `from+size` / `_mget` / agg-bucket caps (`[limits]`); fixed query-nesting depth cap | ✅ |
| Memtable + RSS + disk-flood circuit breakers (429 before OOM/ENOSPC) | ✅ (`[limits]`) |
| Restrictive CORS by default (no cross-origin reads unless allow-listed) | ✅ (`[cors]`) |
| Secrets written `0600` (admin key, auto-generated TLS key) | ✅ |

**Delegated to your environment (not engine-level in this build):**

| Control | Do it with |
|---|---|
| Encryption at rest (segments, WAL are cleartext on disk) | OS FDE (dm-crypt/LUKS, BitLocker), ZFS native encryption, encrypted EBS/PD/Azure Disk, or S3 SSE-KMS |
| RBAC / per-index / field- & document-level security | API gateway (Kong, Apigee, Cloudflare Access) in front of XERJ |
| OAuth / OIDC / SAML / SSO | An OIDC reverse proxy (e.g. oauth2-proxy) |
| mTLS / client-cert validation | Terminate/validate at the proxy |
| Tamper-evident (WORM, hash-chained) audit log | Ship structured logs to an external SIEM (ELK, Splunk, Datadog, Loki) |
| Per-key rate-limiting / rotation / TTL beyond created-key expiry | Reverse proxy / API gateway |

The [deployment-security model](../../engine/reports/SECURITY_DEPLOYMENT_MODEL.md)
report has the full threat model, reference architectures, and compliance
status for these delegated controls. (Note: that report predates the
in-process TLS described here — its "TLS is plain TCP / terminate at a proxy"
lines are the *option*, not the only path; enabling `[tls]` gives you HTTPS
directly. Everything it says about encryption-at-rest, RBAC, and audit is
current.)

A reference single-node topology:

```
Client ──TLS──> [XERJ :9200, tls.enabled=true] ──> [LUKS/EBS-encrypted data_dir]
                     └── or terminate TLS at nginx/Envoy/ALB, run XERJ with
                         tls.enabled=false behind it (keep auth.enabled=true for
                         defense-in-depth, or enforce auth at the gateway)
                     └── RBAC / OIDC / rate-limits enforced at an API gateway if you need them
```

Either enable in-process TLS **or** terminate at a proxy — you don't need both.
Note `--insecure` disables auth *as well as* TLS, so behind a proxy prefer
`tls.enabled = false` with `auth.enabled = true` rather than `--insecure` if you
want XERJ to keep checking the API key.

---

## 3. Air-gapped neural embedder pre-seed

The standard `xerj` binary ships the built-in **neural** embedder — a real
in-process BERT sentence encoder (default `all-MiniLM-L6-v2`, 384-dim, ~90 MB),
no Python, no external service. Select it per node with `[embedding] mode =
"neural"` (or `--embed-mode neural`, or `XERJ_EMBED_MODE=neural`).

On a connected host the model downloads itself **once, on first use**, from the
HuggingFace Hub and is cached for every later start. On an **air-gapped** host
there is no Hub to reach, so stage the model files yourself.

### Step 1 — download the three files on a connected machine

`candle` (XERJ's inference runtime) needs exactly three files, and the weights
**must be safetensors** — it cannot read PyTorch `.bin` weights:

```
config.json
tokenizer.json
model.safetensors
```

Pull them from the model repo (`sentence-transformers/all-MiniLM-L6-v2`) — via
`huggingface-cli download`, `git lfs`, or plain `curl` against
`https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/<file>`.
Verify you have the safetensors file, not `pytorch_model.bin`:

```bash
ls staged-model/
# config.json  model.safetensors  tokenizer.json
```

### Step 2 — copy the folder into the enclave and point XERJ at it

Transfer `staged-model/` to the air-gapped host, then set `local_model_dir`:

```toml
[embedding]
mode            = "neural"
local_model_dir = "/opt/xerj/models/all-MiniLM-L6-v2"   # holds the 3 files above
# neural_model  = "sentence-transformers/all-MiniLM-L6-v2"  # only affects the Hub path
# model_cache_dir = "/opt/xerj/hf-cache"                    # Hub cache dir (unused when local_model_dir is set)
```

When `local_model_dir` is set it takes precedence over the Hub — XERJ makes
**no outbound network call** for the model. Selecting the neural backend logs a
line when the semantic index is built:

```
INFO  embedding backend: built-in neural BERT (loads on first use)
```

The weights themselves load lazily, from your staged directory, on the first
`semantic_text` ingest (not at boot). A correct staging just works — the local
path is silent on success and makes no network call. A **missing or wrong file
fails loud** with the offending path, which is your signal to fix the staging:

```
local model dir /opt/xerj/models/... is missing tokenizer.json
… has no model.safetensors (candle requires safetensors, not pytorch_model.bin)
```

(The `neural embedder: model ready` log line you may have seen elsewhere is the
*Hub-download* path's completion message; the offline `local_model_dir` path
does not print it.)

> **Slim builds.** A `--no-default-features` build omits the neural backend;
> `mode = "neural"` on such a binary logs a warning and falls back to the
> lexical embedder. Use the standard release binary for neural. (The lexical
> default is honest lexical feature-hashing, **not** semantic understanding — see
> the [semantic search recipe](./semantic-search-rag.md) for what each backend
> buys you.)

---

## 4. Interim ES → XERJ migration: scroll + bulk

XERJ speaks the Elasticsearch wire protocol, so for most apps migration is
["change the URL, not your app"](./migrate-from-elasticsearch.md). That covers
pointing a *live client* at XERJ. To **copy existing data out of a running
Elasticsearch**, use the scroll + bulk pattern below.

**Reindex-from-remote is intentionally not supported** — XERJ has no
`reindex.remote.whitelist`, so `POST /_reindex` with a `source.remote` block
**fails loud** instead of silently doing the wrong thing (verified):

```bash
curl -k -H "Authorization: ApiKey $KEY" -X POST https://xerj:9200/_reindex -d '{
  "source": { "remote": { "host": "http://old-es:9200" }, "index": "src" },
  "dest":   { "index": "dst" } }'
# HTTP 400
# {"error":{"type":"illegal_argument_exception",
#   "reason":"[old-es:9200] not whitelisted in reindex.remote.whitelist
#             (reindex from remote is not supported by this XERJ version)"}, "status":400}
```

The working path reads from the source ES with the **scroll** API and writes to
XERJ with **`_bulk`** — both are standard ES calls, and XERJ implements both
ends (a within-XERJ `POST /_reindex` with a local `source.index` also works and
is the right tool once data is already in XERJ).

```bash
#!/usr/bin/env bash
# migrate_scroll_bulk.sh — copy one index from a live Elasticsearch into XERJ.
# stdlib only: bash + curl + python3 (for NDJSON assembly).
set -euo pipefail

SRC=${SRC:-http://old-es:9200}            # source Elasticsearch
SRC_AUTH=${SRC_AUTH:-}                     # simple source auth, e.g. -u elastic:changeme
                                          # (for an ApiKey header with spaces, hardcode it in src() below)
DST=${DST:-https://xerj:9200}             # target XERJ
DST_KEY=${DST_KEY:?set DST_KEY to the XERJ admin key}
INDEX=${INDEX:?set INDEX to the index name}
PAGE=${PAGE:-1000}

src() { curl -s $SRC_AUTH "$@"; }
dst() { curl -sk -H "Authorization: ApiKey $DST_KEY" "$@"; }

# 1. Open a scroll on the source (sorted by _doc = fastest, stable).
resp=$(src -H 'Content-Type: application/json' \
  "$SRC/$INDEX/_search?scroll=5m" \
  -d "{\"size\":$PAGE,\"sort\":[\"_doc\"],\"query\":{\"match_all\":{}}}")

sid=$(printf '%s' "$resp" | python3 -c 'import sys,json;print(json.load(sys.stdin)["_scroll_id"])')
total=0

while :; do
  # Turn this page's hits into an NDJSON bulk body for XERJ.
  ndjson=$(printf '%s' "$resp" | python3 -c '
import sys, json
d = json.load(sys.stdin)
hits = d["hits"]["hits"]
out = []
for h in hits:
    out.append(json.dumps({"index": {"_index": h["_index"], "_id": h["_id"]}}))
    out.append(json.dumps(h["_source"]))
sys.stdout.write("\n".join(out) + ("\n" if out else ""))
')
  n=$(printf '%s' "$resp" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["hits"]["hits"]))')
  [ "$n" -eq 0 ] && break

  # Bulk-load the page into XERJ.
  dst -H 'Content-Type: application/x-ndjson' \
      -X POST "$DST/_bulk" --data-binary "$ndjson" >/dev/null
  total=$((total + n))
  echo "copied $total docs…"

  # Advance the scroll.
  resp=$(src -H 'Content-Type: application/json' -X POST "$SRC/_search/scroll" \
    -d "{\"scroll\":\"5m\",\"scroll_id\":\"$sid\"}")
  sid=$(printf '%s' "$resp" | python3 -c 'import sys,json;print(json.load(sys.stdin)["_scroll_id"])')
done

# 3. Release the scroll on the source and refresh the target.
src -H 'Content-Type: application/json' -X DELETE "$SRC/_search/scroll" \
    -d "{\"scroll_id\":[\"$sid\"]}" >/dev/null
dst -X POST "$DST/$INDEX/_refresh" >/dev/null
echo "done: $total docs → $DST/$INDEX"
```

Notes for a real migration:

* **Create the mapping first** if you rely on specific field types — copy it
  from the source (`GET $SRC/$INDEX/_mapping`) and `PUT` it on XERJ before the
  bulk load, otherwise XERJ infers types from the first documents.
* **Idempotent & resumable.** Each page carries the source `_id`, so re-running
  overwrites rather than duplicates. For huge indices, checkpoint the last
  `_id`/offset and use `search_after` keyset paging instead of a long-lived
  scroll.
* **XERJ's scroll works too.** The same three calls (`_search?scroll=`,
  `POST /_search/scroll`, `DELETE /_search/scroll`) are implemented on XERJ, so
  you can point this script at a XERJ source for XERJ→XERJ copies as well.

---

## 5. Single-node vs. cluster: the production posture

**Run single-node in production.** That is the supported, hardened topology,
and it's the default (`[cluster] enabled = false` → no Raft, no inter-node
transport). Scale it *vertically* — XERJ's ingest and search pipelines
parallelize across cores (`[engine]` shards/workers) — and get durability from
the WAL + segment flush plus filesystem-level backups/snapshots.

**Multi-node cluster mode is experimental — do not use it for production over
an untrusted network.** When you set `[cluster] enabled = true`, XERJ starts a
Raft state machine and a TCP transport on `cluster.port` (default `9300`) for
inter-node consensus and search:

```toml
[cluster]
enabled = true
port    = 9300
peers   = ["n2=10.0.0.2:9300", "n3=10.0.0.3:9300"]
tick_ms = 50
```

That inter-node transport is **plaintext and unauthenticated** — it is a plain
TCP listener with no TLS and no API-key check on the Raft/search messages. The
API-key auth and in-process TLS described above protect the *client-facing*
listeners (REST/ES/gRPC); they do **not** extend to `cluster.port`. Anyone who
can reach `:9300` can participate in the cluster protocol. So if you evaluate
cluster mode, keep every `cluster.port` on a fully trusted, isolated network
segment, and treat the feature as experimental — cross-cluster replication and
disaster recovery are out of scope. For production HA today, mirror via
snapshot/restore on a schedule, or dual-write to two independent single nodes
at the application layer.

---

## Verification

Every status code, banner line, and log message in this recipe is real output
captured from a live `xerj` binary booted with `--config` and a TLS+auth TOML:
HTTPS readiness `200`, auth `401`→`200` with `ApiKey`/`Bearer`, health-probe
exemption under auth, cleartext-against-TLS refusal, `0600` key permissions,
remote-`_reindex` `400`, local reindex success, and a boot that parses the
`[embedding]`/`[cluster]` blocks shown here. The `migrate_scroll_bulk.sh` script
above was run verbatim between two nodes (`PAGE=2` over a 5-doc index → three
scroll pages) and copied all five documents, `_id`s and sources intact, into a
target index that did not previously exist — exit `0`. Reproduce with your own
config; the flows are deterministic.

---

*Verified end-to-end against a live XERJ (merged `main` binary). Claims are
scoped to what this build does today — controls listed as delegated are not
engine-level and must be provided by your deployment environment.*
