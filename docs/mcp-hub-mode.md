# Hub-first MCP mode (`xerj mcp` with no node, #2760)

**Status: wired into `xerj mcp`, opt-in.** With `XERJ_HUB_CACHE_DIR` (or
`--hub-cache-dir <DIR>`) set and no node answering at the URL `xerj mcp`
proxies to, `xerj_hub_find`, `xerj_hub_def` and `xerj_hub_ask` answer a hub
question end to end without a node: the signed catalogue and the signed pack are
fetched from the hub once, verified, and kept in a bounded disk cache; the next
call is served from disk. Without that setting nothing changes: the hub tools
run the `xerj ask` flow and never download.

The code is `xerj_common::hub_client_cache` (mode detection, the verified
bounded cache, the node-free answer) and `xerj-mcp`'s `src/hub/first.rs` (the
adapter: configuration, the HTTP fetch, the tool result). The end-to-end test is
`engine/crates/xerj-mcp/tests/hub_no_node_e2e.rs`.

## Turning it on

```json
{
  "mcpServers": {
    "xerj": {
      "command": "/home/you/.local/bin/xerj",
      "args": ["mcp"],
      "env": { "XERJ_HUB_CACHE_DIR": "/home/you/.cache/xerj-mcp-hub" }
    }
  }
}
```

- `XERJ_HUB_CACHE_DIR` / `--hub-cache-dir <DIR>`: the cache directory. The flag
  wins. It must be an absolute path (an MCP host starts the server in a
  directory of its own choosing), and it should be a directory used for nothing
  else: eviction and orphan cleanup act on the `pack-*` files in it. It is not
  `~/.xerj/hub/packs`, where `xerj hub pull` keeps extracted packs; the two never
  share files.
- `XERJ_HUB_URL`: the hub to fetch from, as for `xerj ask`. Default
  `https://hub.xerj.org`. Must be an http or https URL.

A setting that cannot work (a relative path, a non-http hub URL) stops
`xerj mcp` at startup. A build with no hub signing key does not serve the
`xerj_hub_*` tools at all, with or without this setting.

## What runs when

Each hub tool call first probes the node it proxies to
(`McpRuntimeMode::detect`: a TCP connect within 150 ms), so a node started or
stopped mid-session is noticed.

| call | node answers | no node answers |
|---|---|---|
| `xerj_hub_def {slug, symbol}` | `xerj ask` flow, as before | through the cache |
| `xerj_hub_ask {corpus, mode: def\|files}` (or `mode` omitted and the question is a symbol) | `xerj ask` flow | through the cache |
| `xerj_hub_find {question}` | `xerj ask` flow (reads the catalogue `xerj hub sync` cached) | catalogue from the cache, fetched when missing or a day old |
| `xerj_hub_ask {mode: how}` | `xerj ask` flow | `xerj ask` flow: a pack pulled with `xerj hub pull`, else its `xerj hub pull` line |
| `xerj_hub_ask` with no `corpus` | `xerj ask` flow | `xerj ask` flow when a catalogue was synced; otherwise an error that says to call `xerj_hub_find` and name the corpus |
| a corpus already pulled with `xerj hub pull` | `xerj ask` flow | `xerj ask` flow (local packs first) |

The private-index tools (`xerj_search`, `xerj_def`, `xerj_memory_*`, …) are not
touched: they still need a node, start one when auto-start is on, and otherwise
answer with their own "no node is listening" error. With a node running, no
existing behaviour changes, and the hub is not contacted (the test asserts zero
requests).

### How this squares with "a tool call never downloads"

The `xerj ask` flow runs with `no_fetch` and a cached catalogue only, and the
published docs and the `xerj init` skill say so. That rule keeps an agent's turn
from blocking on the network and keeps a tool call from deleting anything behind
the agent's back. Hub-first mode is the one opt-in exception, and it keeps both
reasons:

- every request has a timeout (connect 10 s, catalogue and signature 30 s, a
  pack 60 s), and a read is bounded by the size the signed catalogue states;
- a pack over the 50 MB auto-pull cap (`AUTO_PULL_MAX_BYTES`) is refused with
  the `xerj hub pull <slug>` line, exactly as the CLI's auto-pull refuses it;
- eviction only ever removes what this cache fetched, in the operator's own
  directory, never `~/.xerj/hub/packs`;
- the answer says what it did (below).

Because the default is unchanged, the "never downloads" statements in
`landing/llms.txt`, `landing/docs/corpus-hub.html`, the agent endpoints page and
the `xerj init` skill remain true for a default install. The three tool
descriptions (and so `landing/docs/agents/schemas/mcp-tools.json`) name the
exception. Making hub-first the default is a product decision that reverses
those published statements; it is not part of this change.

## What an answer says

The text is the usual hub rendering with `warning:` lines:

- `no local xerj node reachable at <url> within 150ms; running in hub-only mode
  via <hub> with local disk cache (private-index tools require starting a local
  node)`;
- on a fetch, `fetched corpus '<slug>' (pin <pin8>) from <hub> into the hub disk
  cache; later calls are served from disk`, and `answered from: hosted hub`;
- on a disk hit, `answered from: local pack`;
- a stale entry served because the refresh failed, a damaged entry that was
  re-fetched, and a truncated result each add a line.

The result also carries `_meta["xerj.org/hub_first"]`:

```json
{
  "runtime_mode": { "mode": "hub_only_fallback", "attempted_url": "…", "hub_url": "…", "notice": "…" },
  "cache_outcome": "fetched_and_cached",
  "freshness": { "state": "fresh", "age_secs": 0, "max_age_secs": 86400 },
  "network_fetches": 2,
  "disk_hits": 0,
  "evicted_slugs": []
}
```

`cache_outcome` is `hit_from_disk` or `fetched_and_cached`. The counts are for
that call: the first call for a corpus makes two fetches (catalogue, pack) and
no disk hits; the second makes none and two disk hits. `structuredContent` is
the `Answer` JSON, as for the `xerj ask` flow, and the opt-in `evidence`
receipt works the same.

`xerj_hub_def` and `xerj_hub_ask` also send the automatic corpus review the
`xerj ask` flow sends (off with `--disable-feedback` /
`XERJ_DISABLE_FEEDBACK=true`). Over `--http`, `xerj_hub_find` is a read-only
tool, but in hub-first mode it can fetch the catalogue into the cache
directory; that is the operator's directory and a bounded, verified file.

## What the cache stores: real hub packs

The hub serves a signed archive, `<slug>-<pin8>.hubpack.tar.zst`, with a
detached ed25519 signature in a sibling `.sig` (`xerj-hub` `pack.rs`).
`CatalogueEntry.pack_sha256` is the sha256 of that archive. The cache keeps the
archive and its signature exactly as downloaded, as
`pack-<slug>-<pin8>.hubpack.tar.zst` and `….sig`, and the signed catalogue
(`<hub>/api/v1/catalogue.json`, the URL `xerj hub sync` uses) as
`catalogue.json` and `catalogue.json.sig`.

A fetch closure returns a `SignedDownload { bytes, signature_hex }`: the bytes
and the contents of the entry's `sig_url`. The library never sees a URL; the
HTTP is in `xerj-mcp`. No `Authorization` header is sent: this is the hub, not
the node. Every download, and every later read from disk, is checked in this
order:

1. the archive's sha256 against the catalogue's `pack_sha256` (and, on a disk
   read, against the digest recorded in the index);
2. the signature over the archive bytes against the hub key
   (`xerj_common::hub::hub_pubkey()`: the compiled-in `HUB_PUBKEY_HEX`, or a
   test `XERJ_HUB_PUBKEY`). With no key every operation refuses with
   `HubModeError::NoHubKey` before anything is fetched;
3. the archive itself: `SHA256SUMS` must cover and match `pack.json`,
   `defs.jsonl` and `files.jsonl`, the manifest must carry a supported
   `format_version`, and its slug and pin must match the catalogue entry.

The archive is read in memory and never extracted. The decompressed stream is
capped at 1 GiB and 100 000 entries (`pack::MAX_UNPACKED_BYTES` and
`pack::MAX_MEMBERS`, shared with the extractor), however well it compresses.
Every disk hit re-verifies and re-reads the archive; the cost grows with the
pack, which the 50 MB fetch cap keeps modest.

The disk budget defaults to `xerj_common::hub::DISK_BUDGET_BYTES`, the same
ceiling `xerj hub pull` prunes the pack cache to. When a new entry would exceed
it, the least recently used entries are evicted (ordered by
`(last_accessed_seq, slug)`) and their files removed. A pack larger than the
whole budget is refused before it is downloaded.

## Refresh rules

- A **fresh** entry is a disk hit; `fetch` is not called.
- A **stale** entry (`now >= fetched_at + ttl`, default 24 h), a **damaged** one
  (unreadable, digest or signature mismatch, bad archive or format) or none at
  all calls `fetch`. A pin rotation is picked up, and a damaged entry repairs
  itself.
- A stale entry that still verifies is served **only** when the fetch itself
  fails, and the answer says so (`refresh_error`, and a warning line). A damaged
  entry is never served.
- Bytes that `fetch` returns but that fail the digest, signature or archive
  checks are an error. They never fall back to an older entry.
- A refreshed catalogue that is signed but **older** than the cached one (its
  `generated_at`) is a rollback and is refused, as `xerj hub sync` refuses it.

## Sharing one cache directory

Each agent session starts its own process, and several can share a cache
directory. Every read-modify-write of `index.json`, and every eviction, runs
under the advisory file lock `cache.lock` (`xerj_common::flock`, the same lock
the automatic node start and `xerj hub pull` take). `index.json` is re-read
inside the lock, so one process never overwrites another's update, and an
eviction never deletes a pack another process just indexed. The lock is held
only for index work and file reads, never across a download. An operation that
cannot get it within 10 s fails with `HubModeError::CacheLock`. Inside one
`xerj mcp` process, hub-first calls run one at a time, so two concurrent first
calls for a corpus download it once; two processes can still each download it.

An `index.json` that cannot be read is rebuilt and the pack files it orphaned
are removed. One written by a newer build is an error: it is not ours to
discard.

## What a pack can answer without a node

`answer_from_pack` answers from `defs.jsonl` and `files.jsonl`:

- **Definition lookup** (`AnswerMode::Def`): symbols equal to the query first,
  then those containing it, case-insensitively. An empty query is
  `HubModeError::EmptyQuery`. `xerj_hub_def` returns 5 hits by default (`k` up
  to 50).
- **File list** (`AnswerMode::Files`): per-file licence; a non-empty query keeps
  the paths that contain it. `xerj_hub_ask {mode: files}` passes the question
  as that path fragment when it is one word, and lists the first 10 files when
  it is a sentence; the answer says which it did.
- **Passage question** (`AnswerMode::How`) is refused with
  `HubModeError::PassageSearchNeedsNode`. The passages live in the pack's
  `snapshot/`, which only a node restores. The tool does not call the library
  for these: it runs the `xerj ask` flow, which answers from a pulled pack.

Every answer carries at most `max_hits` hits (default 50, at least 1). When more
matched, `Answer.truncated` is set and a warning says how many.

## Loud errors

Nothing here returns an empty result in place of a failure. As tool errors
(`isError: true`, no `structuredContent`): `UnknownCorpus` (the text adds
"call xerj_hub_find to see which corpora the hub serves"), `InvalidCorpusSlug`,
`UnsupportedFormatVersion` (catalogue, pack manifest, cache index),
`ExcludedCorpusLicence`, `IntegrityMismatch` (digest, rollback), `SignatureInvalid`,
`InvalidPack`, `EntryExceedsCacheBudget`, `NoHubKey`, `EmptyQuery`, `CacheLock`,
and `HubFetch` (the hub cannot be reached, answers an HTTP error, serves
something that is not a catalogue, or a pack is over the 50 MB cap). A corpus
marked `LicenceClass::ApproachOnly` attaches a `licence_warning` to every hit.
A known corpus that simply has no definition of the symbol is an answer with no
hits, as in the `xerj ask` flow, not an error.

## Acceptance criteria of #2760

`engine/crates/xerj-mcp/tests/hub_no_node_e2e.rs` runs the real tool dispatch
against a local HTTP server that stands in for the hub (signed catalogue, packs
written by `xerj_hub::pack::write_pack`, a throwaway hub key) and a port nothing
listens on:

1. **No node: starts, lists the hub tools, answers.** `tools/list` includes the
   three tools and touches no network; `xerj_hub_def` returns the definition
   with `cache_outcome: fetched_and_cached`, two fetches, and the hub server
   counted one catalogue and one pack request.
2. **Second call from disk, measured.** The same corpus answers with
   `hit_from_disk`, zero fetches, two disk hits, and the hub's request counts
   unchanged. `xerj_hub_ask` (def and files) and `xerj_hub_find` are then
   answered from the same cache. With the hub shut down, a further call is still
   answered; with an empty cache and the hub down, the error names the hub.
3. **With a node, as today.** With a listener on the proxied URL the call runs
   the `xerj ask` flow (there is no synced catalogue in the test, so it says to
   run `xerj hub sync`), carries no `_meta`, and the hub receives no request.
   The existing `tests/hub_tools.rs` (real node, local pack) is unchanged.
4. **Loud errors.** An unknown corpus, a pack with `format_version` 2, a
   catalogue with `format_version` 3, and a pack whose bytes are not the ones
   the catalogue pins are `isError: true`, and a refused pack leaves nothing in
   the cache.

## Not here

- **Default-on.** See above: it reverses a published contract.
- **Passage search with no node.** A pack archive holds no passages; restoring
  `snapshot/` into a node is `xerj hub pull`.
- **`HubFirstRouter::list_tools` and `route_tool`.** `xerj mcp` does not call
  them: it serves one `tools/list` in every mode and decides per hub tool in
  `src/hub/first.rs`. They remain in the library, tested, for a caller that
  wants the per-mode view.
- **A hosted query route.** This build has no hosted client, as for `xerj ask`.
