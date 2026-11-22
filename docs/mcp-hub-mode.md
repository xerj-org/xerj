# Hub client cache library (`xerj_common::hub_client_cache`, #2760)

**Status: an internal library module that nothing calls yet.** No binary uses
it: not `xerj mcp`, not `xerj-server`. #2760 asks for more than this module:
with no node running, `xerj mcp` should start and answer a hub question end to
end. That wiring has not landed, and this module does not claim it. Today
`xerj mcp` serves the `xerj_hub_*` tools through the `xerj ask` flow in
`xerj-hub` (packs already on the machine, the automatic node).

The module is the part of #2760 that can be built and tested without a node:
mode detection, tool routing, and a verified, bounded, cross-process-safe disk
cache for what the hub serves.

## What the cache stores: real hub packs

The hub serves a signed archive, `<slug>-<pin8>.hubpack.tar.zst`, with a
detached ed25519 signature in a sibling `.sig` (`xerj-hub` `pack.rs`).
`CatalogueEntry.pack_sha256` is the sha256 of that archive. The cache keeps the
archive and its signature exactly as downloaded, as
`pack-<slug>-<pin8>.hubpack.tar.zst` and `….sig`, and the signed catalogue as
`catalogue.json` and `catalogue.json.sig`.

A fetch closure returns a `SignedDownload { bytes, signature_hex }`: the bytes
and the contents of the entry's `sig_url`. The cache never sees a URL. Every
download, and every later read from disk, is checked in this order:

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

## Sharing one cache directory

Each agent session starts its own process, and several can share a cache
directory. Every read-modify-write of `index.json`, and every eviction, runs
under the advisory file lock `cache.lock` (`xerj_common::flock`, the same lock
the automatic node start and `xerj hub pull` take). `index.json` is re-read
inside the lock, so one process never overwrites another's update, and an
eviction never deletes a pack another process just indexed. The lock is held
only for index work and file reads, never across a download. An operation that
cannot get it within 10 s fails with `HubModeError::CacheLock`.

An `index.json` that cannot be read is rebuilt and the pack files it orphaned
are removed. One written by a newer build is an error: it is not ours to
discard.

## What a pack can answer without a node

`answer_from_pack` answers from `defs.jsonl` and `files.jsonl`:

- **Definition lookup** (`AnswerMode::Def`): symbols equal to the query first,
  then those containing it, case-insensitively. An empty query is
  `HubModeError::EmptyQuery`.
- **File list** (`AnswerMode::Files`): per-file licence; a non-empty query keeps
  the paths that contain it.
- **Passage question** (`AnswerMode::How`) is refused with
  `HubModeError::PassageSearchNeedsNode`. The passages live in the pack's
  `snapshot/`, which only a node restores. This module does not do that.

Every answer carries at most `max_hits` hits (default 50, at least 1). When more
matched, `Answer.truncated` is set and a warning says how many.

## Routing

`McpRuntimeMode::detect` takes a probe closure for the local node and a budget
(default 150 ms). `HubFirstRouter::route_tool` sends private-index tools to the
node when it is reachable and returns `HubModeError::LocalNodeRequired` when it
is not. The `xerj_hub_*` tools are refused with `HubModeError::NoHubKey` in a
build with no hub key, as `xerj mcp` does not serve them then; they are also
shown as unavailable in `list_tools`.

## Loud errors

Nothing here returns an empty result in place of a failure:
`InvalidCorpusSlug`, `UnknownCorpus`, `UnsupportedFormatVersion` (catalogue,
pack manifest, cache index), `ExcludedCorpusLicence`, `IntegrityMismatch`,
`SignatureInvalid`, `InvalidPack`, `EntryExceedsCacheBudget`, `NoHubKey`,
`EmptyQuery`, `PassageSearchNeedsNode`, `CacheLock`. A corpus marked
`LicenceClass::ApproachOnly` attaches a `licence_warning` to every hit.

## Not here yet

- Fetching: callers pass the fetch closures; no HTTP client is wired in.
- `xerj mcp` starting in hub-only mode and registering these tools.
- Passage search from a pack with no node.
