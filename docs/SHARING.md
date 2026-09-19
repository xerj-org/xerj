# Sharing an indexed folder

`xerj share` gives one person read-only search over one indexed folder: a link,
a passcode and an expiry. The guest opens the link in a browser, types the
passcode, and gets a reading room — a search box, highlighted snippets and a
document view — over that one index and nothing else on the node.

The documents and the index stay on your machine. Nothing is uploaded, synced
or copied to a hosting service, and the guest installs nothing. What does leave
the machine is what the guest asks to see: search results and the documents
they open, sent to their browser. Who else can read that traffic depends on how
the link reaches your node: nobody on a loopback link, whoever is on the network
path for a plain-`http` address, and **Cloudflare** for a `--tunnel` link — see
[`--tunnel`](#--tunnel). The command says which of these applies when it prints
the link.

"Zero-token" in this document means search and reading. The reading room
generates no text, summarises nothing and calls no model.

Every statement below points at the code or the test that makes it true.
Primary sources:

| Area | File |
|---|---|
| `/_share` API, records, passcode, claim limiter | `engine/crates/xerj-api/src/share.rs` |
| Guest route allow-list, PIT refusal | `engine/crates/xerj-api/src/authz.rs` (`guest_route_allowed`, `guest_body_denied`) |
| `xerj share` CLI and the tunnel supervisor | `engine/crates/xerj-server/src/share.rs` |
| Guest page | `xerj-ux/share/` |
| Guest page serving and its header set | `engine/crates/xerj-console-api/src/spa.rs` |
| Live security test | `engine/crates/xerj-api/tests/live/share_security_live.py` |
| Headless-browser test of the guest flow | `xerj-ux/test/share-guest-flow.e2e.mjs` |
| One script that runs all of it | `.github/scripts/share-links-smoke.sh` |

## Quick start

```sh
# 1. index the folder (boots a local node with authentication ON)
xerj brain ~/casefiles

# 2. share it — prints a link, a passcode, the expiry and how to revoke
xerj share ~/casefiles --label "for Dana" --expires 7d

# 3. for someone who is not on this machine: a temporary public address
xerj share ~/casefiles --label "for Dana" --tunnel
```

`xerj brain` prints, in its closing summary, the exact `xerj share` command for
the folder it just indexed, including `--url` and `--data-dir` when they are not
the defaults. Each argument is quoted when it needs to be, so a folder called
`case files` pastes as one argument.

**One open is one browser tab.** The guest's key lives in that tab's
`sessionStorage`. Opening the same link again in the same tab keeps the session
and spends nothing; a closed tab is a spent open. With the default
`--max-claims 1`, a guest who closes the tab cannot come back, however long the
share has left to run. For someone who will read over several days, give a few:
`--max-claims 5`.

Send the link and the passcode **through different channels** — the link by
email, the passcode by phone or a messenger. Either one alone opens nothing.

## The command

```text
xerj share <index|folder> [OPTIONS]   create a share
xerj share --list                     every share on this node
xerj share --revoke <handle>          end one (invalidates its guest keys)
```

| Option | Meaning |
|---|---|
| `<index\|folder>` | An index name (`ax-notes`, or `a,b`), or the folder you gave `xerj brain`. A folder resolves to the brain `xerj brain` built for it and to the indices that brain's meta document lists. |
| `--expires <D>` | Lifetime: `30m`, `24h`, `7d`. Default `24h`, longest `30d`. |
| `--max-claims <N>` | How many times the link can be opened. Default `1`, most `50`. Each claim mints its own guest key. One open is one browser tab (see above). |
| `--passcode <CODE>` | Choose the passcode (6–128 characters). Default: a generated `xxxx-xxxx`. A passcode typed on the command line lands in your shell history; the generated one does not. |
| `--label <TEXT>` | What the guest sees as the title. |
| `--brain <NAME>` | Also share this brain's links. A folder argument sets it for you. |
| `--index` | Treat the argument as an index name even when a folder of that name exists. |
| `--tunnel` | Open a temporary public address with `cloudflared`, print the full guest link, run until Ctrl-C, then close the tunnel **and revoke the share**. The traffic passes through Cloudflare, which can read it; the command says so when it prints the link. |
| `--keep` | With `--tunnel`: leave the share active after Ctrl-C. |
| `--public-url <U>` | The `https://` address this node is already reachable at; the printed link uses it. `http://` is accepted with a printed warning: the passcode and the guest key would cross the network unencrypted. |
| `--url`, `--data-dir`, `--api-key` | The node (scheme included: `http://localhost:9200`), where its `admin.key` is, or the admin key itself (also `XERJ_API_KEY`). Same resolution order as `xerj brain`. |
| `--json` | Machine-readable output. |

Exit codes: `0` ok, `1` refused or failed, `2` usage, `130` interrupted before
the tunnel was up (nothing was shared).

Shares are managed with the node's **admin key** and nothing less. A scoped or
minted key gets `403` on `POST /_share`, `GET /_share` and
`DELETE /_share/{handle}` — otherwise a key could widen its own reach by minting
a share (`share.rs`, `require_superuser`). A wrong or non-admin key is reported
as a key problem whether the argument is an index or a folder.

### It refuses an open node

A node started with `--insecure` (or `auth.enabled = false`) treats every
request as the superuser. A "read-only guest key" on such a node restricts
nothing: the guest, and anyone else who can reach the port, can already read,
change and delete every index without it. `xerj share` therefore refuses, says
why, and exits `1`; the node answers `POST /_share` and the claim route with
`409` (`share.rs`, `open_node_refusal`). Restart the node without `--insecure` —
authentication is on by default and the admin key is written to
`<data_dir>/admin.key`.

### `--tunnel`

`--tunnel` runs your own `cloudflared` as a child process
(`cloudflared tunnel --url http://127.0.0.1:<es port>`), reads the
`https://….trycloudflare.com` hostname from its output, and prints the full
guest link. The tunnel comes up **before** the share is created, so a link is
never printed for an address that is not there. On Ctrl-C (and SIGTERM, and a
closed terminal) the tunnel is terminated and the share is revoked, which
invalidates every guest key it minted.

If `cloudflared` is not installed the command does not fail: it prints the exact
install steps for your platform and the local link. It looks at
`XERJ_CLOUDFLARED`, then `PATH`, then `~/.local/bin`.

Verified on 2026-09-18 against a real quick tunnel: the guest page, a wrong and
a right passcode, a guest search and a refused `_cat` call all went through the
public hostname; after SIGINT the `cloudflared` process was gone, the share
listed as `revoked`, and the guest key answered `401`. That run cannot be
repeated in CI — it needs the public internet and a third party — so CI covers
the hostname parser, the fallbacks and everything that does not need the tunnel
itself.

When `cloudflared` stops on its own, the command revokes the share and prints
`cloudflared`'s last lines, which are the only explanation there is.

Three things to know about quick tunnels:

- **The hostname is random and new on every start.** A link stops working when
  the command stops. That is the intended lifetime, not a fault.
- **Traffic passes through Cloudflare, which terminates TLS.** The corpus is not
  uploaded there. Everything the guest's browser and your node say to
  each other does travel through Cloudflare's network, and the operator of a TLS
  endpoint can read what passes through it: **the passcode, the guest's API key,
  every search and every document the guest opens.** `xerj share --tunnel`
  prints this with the link, `--help` says it under `--tunnel`, and the guest
  page shows a notice on a `trycloudflare.com` hostname before the passcode is
  typed. If that is not acceptable, publish the node at your own hostname behind
  your own TLS and use `--public-url`.
- **Cloudflare offers quick tunnels without an account and without an uptime
  guarantee.** For a standing arrangement use a named tunnel or your own
  reverse proxy with a stable hostname.

`--tunnel` fronts a plain-http node on this machine only. For a node with TLS
on, run your own tunnel or proxy and pass its address as `--public-url`.

## What the guest can and cannot do

The guest's key carries one role, `share:<handle>`: `read` on the indices the
share names, plus `read` on the shared brain's edges index when a brain is
named. Two things confine it, both enforced server-side:

1. **The index grants** — the per-index authorization every scoped key gets, in
   the middleware and again at the engine's index funnel
   (see [SECURITY_MODEL.md](./SECURITY_MODEL.md#the-two-layers)).
2. **A route allow-list that applies to share guests only**
   (`authz.rs`, `guest_route_allowed`). An ordinary scoped key is given a
   *filtered* view of `_cat`, `_cluster/*` and `_nodes` because Kibana needs
   one. A guest was handed one corpus by someone who did not agree to describe
   their node, so for a guest that surface is closed outright.

| A guest can | A guest cannot |
|---|---|
| `_search`, `_count`, `_msearch`, `_mget` on the shared indices | search, count, fetch or map any other index — by name, by pattern, by alias, or by naming it inside a request body |
| `GET _doc/{id}` (and `HEAD`) on the shared indices | write, update, delete, bulk, reindex, refresh, flush, close, or change a mapping or a setting |
| `_mapping` and `_field_caps` on the shared indices | read `_cat/*`, `_cluster/*`, `_nodes`, `_snapshot`, `_tasks`, `_stats`, `_aliases`, index templates, ingest pipelines, `/v1/metrics`, the native `/v1/*` router |
| `GET /_graph/{brain}/ego` and `/overview` for the shared brain | read or write another brain, write to the shared brain, use `/_memory/*` |
| read `GET /`, `_security/_authenticate`, the health probes | list, create or revoke shares; mint an API key; read the audit log |
| | open a scroll, a point-in-time or an async search — nothing that parks state on your machine — or ride one that someone else opened |
| | use the Console API (`/_xerj-console/api/v1/*`), which takes a session cookie and not an API key |

A pattern such as `_all` or `*` is not refused: it is expanded over what the
key **holds**, so for a guest `_all` means "all of yours". It never means more.

The allow-list covers both HTTP listeners (the ES-compatible port and the
native REST port). The **gRPC listener** is outside it: it applies the same
per-index grants. An in-process test drives the real gRPC interceptor and
handlers with a claimed guest key: it reads the shared index, and gets
`PermissionDenied` on every other index, on patterns, on system indices and on
`index` and `delete` (`bulk_index` needs a transport to call; it runs the same
per-item check as `index`), and `Unauthenticated` after a revoke
(`xerj-server/src/grpc.rs`, `a_share_guest_key_reads_its_index_over_grpc_and_nothing_else`).
No test opens a gRPC socket with a guest key. The listener binds to loopback by
default and is not reachable through `--tunnel`, which fronts the ES-compatible
port only.

The `autoindex-catalog` index, which lists every corpus on the node, is **never**
granted. A folder share does not include it, and it cannot be shared by name, in
a list or through an alias: `POST /_share` answers `400` (`share.rs`,
`validate_share_index`). The engine has no document-level security to filter it
with, and the guest page is handed the index name directly.

There is no `GET /{index}/_source/{id}` route on this node (`404` for the admin
key), so it is not on the guest allow-list either; `GET _doc/{id}` returns the
source.

Every row of that table is a check in
`engine/crates/xerj-api/tests/live/share_security_live.py`, which runs against a
real node with authentication on. It includes the indirect routes: a `terms`
lookup, a `more_like_this` like-document, a `percolate` stored document, an
`indexed_shape` and a `lookup` runtime field that each name a private index from
inside an otherwise permitted search; an `_msearch` header and an `_mget` body
that name another index; aliases that point elsewhere; and the owner's own
point-in-time and scroll ids presented by the guest.

### An alias is resolved when the share is made

Naming an alias in `xerj share` (or `POST /_share`) records the **concrete
indices it points at now**. Re-pointing the alias later does not move, or widen,
what a guest already holds. The backing names go through the same validation as
any other, so an alias is not a way to share a system index.

### What can never be shared

A share names concrete indices of yours. Patterns and lists-as-patterns
(`logs-*`), `_all`, every dot-index (`.xerj_sessions`, `.xerj_api_tokens`,
`.xerj_passkeys`, `.xerj_audit`, …), the reserved `.xerj-memory-*` namespace and
the `autoindex-catalog` index are refused with `400`. A brain's links are shared by naming the brain, which
grants its edges index and nothing else in the namespace.

## Threat model

### What a share protects against

| Threat | What stands in the way |
|---|---|
| Someone finds or guesses the link | The share id is 128 bits from the OS CSPRNG. The link alone opens nothing: the passcode is a second factor sent separately. |
| Someone has the link and guesses passcodes | A lockout per share, from anywhere: 10 attempts a minute and 30 an hour (`SHARE_PER_MINUTE`, `SHARE_PER_HOUR`). A generated passcode is 8 symbols from a 31-symbol alphabet — 852,891,037,441 codes, about 39.6 bits. Thirty guesses an hour for the 30-day maximum lifetime is 21,600 guesses. |
| The node's disk is read | `shares.json` (mode `0600`) holds a SHA-256 digest of the share id and an Argon2id hash of the passcode (`m=19456,t=2,p=1` in the records this build writes). No share id, passcode or guest key is written to `shares.json`, to `api_keys.json` or to the audit log. The CLI and the guest page never put a share id in a URL, so the node's own log does not hold one either, also with `logging.access_log = true` (the smoke script turns it on to check exactly that). |
| The link leaks into a log | The share id is never part of a URL the guest page requests. It rides in the URL **fragment**, which a browser does not send to a server, so the page request and every `Referer` are free of it. The page then sends it in the **body** of `POST /_share/claim` — not in the path, which is what an access log, a reverse proxy and a tunnel's edge record — and removes it from the address bar. The browser test asserts the id is in no requested URL and only in the claim body. A proxy configured to log request *bodies* would still record it, together with the passcode. The HTTP API's `DELETE /_share/{handle}` also accepts the full id; a caller who uses it that way puts the id in a request path, and revokes it. `xerj share --revoke` takes only the handle. |
| A proxy or browser caches the credential | Every `/_share` response — success or error, including the `401` and `403` the authentication and authorization layers produce — is `Cache-Control: no-store`. So is every response to a guest key (search results, documents, refusals), and so are the guest page and its assets. |
| A document tries to attack the guest | Document text is other people's email and is treated as hostile. The page places it with `textContent` only — there is no `innerHTML`, `eval` or HTML parsing in `share.js`, HTML email bodies are shown as source text, and URLs in documents are not made into links. Independently of that, the page is served with `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`, so injected markup could neither run nor fetch. `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`. |
| The guest wanders | The route allow-list and the index grants above. |
| A link that outlives its purpose | Expiry (default 24 hours, at most 30 days), a claim limit (default 1), `xerj share --revoke`, and `--tunnel` revoking on exit. A revoke invalidates every key the share minted; the guest's next request is refused. |
| Junk traffic locks the real guest out | See the next section. |

### The claim route and its limiter

`POST /_share/claim` is the only unauthenticated route on the node that hands
out a credential, so it is narrow on purpose: one exact path, `POST` only, a
body of at most 4 KiB, rate-limited, never cached, and every outcome — success,
wrong passcode, exhausted, expired, revoked, unknown, throttled — goes to the
audit log with the source address. Only the *first* refusal in a throttle window is audited, so a
flood cannot turn the audit log into its own denial of service.

There are two throttles, and they are deliberately **not** stacked:

- A claim against a **known** share is charged to that share's window, from
  anywhere. This is the passcode lockout, and it does not depend on knowing who
  is asking.
- A claim against an **unknown** id is charged to the source address's window
  (10 a minute, 100 an hour; IPv6 sources are keyed by their /64).

Behind `--tunnel` — or any reverse proxy on the same host — every request
arrives from `127.0.0.1`. A limiter that charged the source address first would
collapse every guest into one bucket, and ten junk requests a minute from
anyone who had found the hostname would answer every real guest with `429`.
The fix is not to believe `X-Forwarded-For`: a direct client writes that header
itself. It is to stop the collapsed bucket from mattering — a real share id is
governed by its own window, which junk cannot touch, and junk is governed by the
source window, where collapsing every tunnel client into one bucket only makes
the bound tighter. The live test floods the source bucket from the same address
as the guest, with a rotating spoofed `X-Forwarded-For`, and then claims
successfully.

To record each guest's real address in the audit log, declare the proxy the way
the Console already requires:

```toml
[server]
trusted_proxies = ["127.0.0.1", "::1"]
```

Then, and only then, `X-Forwarded-For` is read, right to left, skipping declared
proxies — the same reading as the Console's `client_ip.rs` (#76 S5-4). An
unconfigured node believes nobody. `xerj share --tunnel` tells you which of the
two your node is doing.

One consequence to accept: someone who **has the link** can spend that share's
30 guesses an hour and lock the real guest out for the rest of the hour. They
cannot get in, and they need the 128-bit link to do it. The alternative — no
lockout for holders of the link — is a passcode that can be brute-forced.

### What a share does not protect against

- **The guest.** Read access is read access. A guest can copy, screenshot or
  retype anything they can open, and there is no watermarking or download
  control. Share an index you are content for that person to read in full.
- **No per-document or per-field restriction.** A share grants whole indices.
  The engine has no document-level or field-level security
  ([SECURITY_MODEL.md](./SECURITY_MODEL.md#known-limits)). If a folder holds
  things the guest must not see, index a folder that does not.
- **Link and passcode sent together.** Whoever holds both is the guest, until
  the share expires, is used up, or is revoked.
- **An observer on the path.** A local link is plain `http` on loopback. A link
  to a LAN or public `http://` address is readable by anyone on the path, and
  the command says so. A `--tunnel` link is HTTPS to Cloudflare, which
  terminates it and can read the passcode, the guest key and the documents (see
  above). For end-to-end TLS you control, publish the node behind your own
  certificate.
- **A compromised guest browser.** The guest key lives in that tab's
  `sessionStorage` until the tab closes or the guest signs out.
- **Successful reads are only partly audited.** The audit log records every
  claim outcome, every `_search`, and every request an authenticated key was
  **refused** (`403`) — including a guest's attempt to list, create or revoke a
  share or to mint an API key. It does **not** record a successful `_msearch`,
  `_mget`, `_count`, `_mapping`, `_field_caps` or `GET _doc`: a guest can read
  the whole shared index through those without leaving a line. It records
  nothing for an unauthenticated request (`401`) other than a claim
  ([SECURITY_MODEL.md](./SECURITY_MODEL.md#known-limits)).

XERJ makes no legal claim about any of this. Whether sharing documents this way
satisfies a professional, contractual or regulatory duty is a question for the
people involved and their advisers, not for this document.

## Passkeys, and why guests use a link and a passcode

The Console's own sign-in is a passkey. A passkey is bound to the **hostname**
it was created on — that binding is what makes it phishing-resistant — and a
quick tunnel gets a new random hostname every time it starts. A passkey
enrolled through one tunnel would be unusable through the next, so a guest
cannot be given one over a quick tunnel.

That is why a share is a link plus a passcode. The path to guest passkeys is a
**stable hostname**: a named Cloudflare Tunnel on your own domain (optionally
with Cloudflare Access in front of it), or any reverse proxy with your own
certificate, passed to `xerj share` as `--public-url`. Guest passkeys are not
built; the stable hostname is the prerequisite for building them.

## The guest page

The page is `xerj-ux/share/`, bundled into the binary and served at
`/_xerj-console/share` without a session — the guest has no credential until the
page has exchanged the passcode for one. It is buildless and self-contained: no
third-party script, font, stylesheet or image, and the browser test asserts that
every request it makes goes to the node itself.

1. It reads the share id from `location.hash`.
2. It asks for the passcode and posts `{id, passcode}` to `POST /_share/claim`.
   The id is in the body, never in a URL.
3. On success it stores the session record, in `sessionStorage` (scoped to the
   tab, gone when it closes):

   ```js
   sessionStorage['xerj.share'] = JSON.stringify({api_key, index, brain, expires_at, label})
   ```

   The Console's guest mode reads the same record, which is why its shape is
   fixed. Beside it the page keeps `xerj.share.link`: the share's public
   **handle** (the 12 characters `xerj share --list` shows), which cannot be
   turned back into the link. That is how the page recognises the same link
   opened again in the same tab and keeps the session instead of asking for —
   and spending — another claim. A different link asks for its own passcode, and
   the session the tab holds is replaced only when that claim succeeds. A link
   pasted over another one changes only the fragment; the page listens for that.
4. It learns the mapping and searches: the `hybrid` query (BM25 fused with the
   node's embedder) where the index has a `semantic_text` field, `multi_match`
   over the text fields otherwise. It says which on the page. The default
   embedder is lexical feature hashing, so hybrid here is not "semantic AI": it
   ranks by word and sub-word overlap. A hybrid hit that contains none of the
   query's words is labelled as ranked by vector similarity, and when no listed
   hit contains them the page says "No document contains …" before the list.
5. It shows the expiry, a read-only notice, and a sign-out that clears
   `sessionStorage`. On a `trycloudflare.com` hostname it also shows that
   Cloudflare carries the connection and can read it.

## The HTTP API

| Route | Who | Body / result |
|---|---|---|
| `POST /_share` | admin key | `{index: "a" \| ["a","b"], brain?, expires_in?: "24h", max_claims?: 1, passcode?, label?}` → the record plus `share_id`, `url_path`, `passcode` — the only time either plaintext is shown |
| `GET /_share` | admin key | `{shares: [{handle, label, indices, brain, created_at, expires_at, max_claims, claims, claims_left, status}]}` — no ids, no hashes |
| `DELETE /_share/{handle}` | admin key | `{handle, revoked: true, keys_invalidated}`. The full share id is accepted in place of the handle, but then it is in the request path; the CLI sends only the handle. |
| `POST /_share/claim` | anyone, rate-limited | `{id, passcode}` → `{api_key, index, indices, brain, expires_at, label, claims_left}`; `401` wrong passcode, `404` unknown link, `410` expired / used up / revoked, `429` with `Retry-After`, `409` on an open node. The share id is in the body so that it is not in the request line an access log records. |

`api_key` is ready for `Authorization: ApiKey <api_key>`. `index` is the
comma-joined list, which is a valid multi-index expression for
`/{index}/_search`.

## Verifying it yourself

```sh
(cd engine && cargo build --release -p xerj-server)
bash .github/scripts/share-links-smoke.sh
```

The script boots throwaway nodes on `127.0.0.1:9510–9512` (override with
`XERJ_SHARE_PORT`), and refuses to run against a port that already answers. It
runs the live security test with and without a trusted proxy (the first node
with `logging.access_log = true`, so that "no share id in the node's log" is
checked against a node that logs), the CLI — including `--tunnel` against a stub
`cloudflared` — the headless-browser guest flow (Chrome or Chromium;
`CHROME_BIN` to point at one), the open-node refusal, and the `xerj brain` →
`xerj share <folder>` path. Do not start it on port `10080` or any other port
browsers refuse (`ERR_UNSAFE_PORT`): the browser phase cannot reach the node
there. CI
runs the same script in the `usecase-smoke` job.

The record of what was run on this branch, on which build, with which result —
including the by-hand quick-tunnel run that CI cannot repeat — is
[docs/usecases/share-links/VERIFICATION.md](./usecases/share-links/VERIFICATION.md).
