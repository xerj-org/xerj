# MCP Hub-Only Mode & Bounded Disk Cache (`#2760`)

`xerj mcp` supports a hub-first stdio runtime mode (`xerj_common::hub_client_cache`) so agent hosts (Claude Code, Cursor, Codex) can launch `xerj mcp` per session with **no running local `xerj` daemon required**, while continuing to use a local `xerj` server whenever one is reachable.

## Runtime Mode Detection (`McpRuntimeMode` & `HubFirstRouter`)

At session startup, `McpRuntimeMode::detect` probes the configured local node URL within a bounded startup budget (`DEFAULT_PROBE_BUDGET_MS = 150ms`):

1. **`LocalNodeReachable { base_url, probe_latency_ms }`**:
   - Private-index tools (`xerj_search`, `xerj_def`, `xerj_map`, `xerj_memory_*`, `xerj_brain_*`, `xerj_plan`, `xerj_status`, …) route to the local node at `base_url` as before.
   - Hub and pack tools (`xerj_hub_find`, `xerj_hub_def`, `xerj_hub_ask`, `xerj_corpus_feedback`, `xerj_field_report`) remain available.
2. **`HubOnlyFallback { attempted_url, hub_url, notice }`**:
   - When no local node responds within the probe budget, `xerj mcp` starts without failing, lists the hub/pack tools as available, and attaches an explicit fallback notice explaining that private-index node tools require a running local node.
   - Calling a private-index tool while in `HubOnlyFallback` returns a loud, typed `HubModeError::LocalNodeRequired` error.

## Bounded LRU Disk Cache (`HubDiskCache`)

`HubDiskCache` caches the signed hub catalogue and verified corpus pack artifacts on disk so repeated queries for the same corpus are served locally with zero network round-trips:

- **Layout**:
  - `index.json`: persistent LRU metadata table (`format_version: 1`), monotonic access clock, per-corpus SHA-256 digests, `pin_sha`, byte sizes, and `fetched_at_unix` timestamps.
  - `catalogue.json`: cached hub catalogue verified against `CATALOGUE_FORMAT_VERSION_MIN..=CATALOGUE_FORMAT_VERSION`.
  - `pack-<slug>.json`: verified corpus pack payload (`CachedCorpusPack`) written atomically via `xerj_common::fsio::write_file_durable`.
- **LRU Byte Budget**:
  - Configured via `max_bytes` (`DEFAULT_MCP_CACHE_MAX_BYTES = 256 MiB`).
  - Before writing a newly fetched corpus pack that would cause `used_bytes() + entry_bytes > max_bytes`, `HubDiskCache` deterministically evicts the least-recently-accessed entries (ordered by `(last_accessed_seq, slug)`) and removes their on-disk pack files.
  - A single corpus entry larger than `max_bytes` is refused loudly with `HubModeError::EntryExceedsCacheBudget` without evicting existing entries.
- **Honest Staleness Reporting (`Freshness`)**:
  - Every cached lookup evaluates `Freshness::evaluate(fetched_at_unix, now_unix, ttl_secs)` (`DEFAULT_CACHE_TTL_SECS = 86400`).
  - When `age_secs > ttl_secs`, the entry is reported as `Freshness::Stale { age_secs, max_age_secs }` and a warning line is appended to `Answer.warnings` and rendered output—never hidden.

## Loud Errors on Unknown Corpus or Format Version

`HubDiskCache` and `HubFirstRouter` never return an empty result on invalid metadata or unknown targets:

- `HubModeError::InvalidCorpusSlug`: rejects empty slugs, slugs over 64 bytes, path traversal (`../`), or non-lowercase-ASCII characters before any disk or network I/O.
- `HubModeError::UnknownCorpus`: rejects any slug not listed in the catalogue, naming the catalogue's `generated_at` timestamp.
- `HubModeError::UnsupportedFormatVersion`: rejects any catalogue outside `CATALOGUE_FORMAT_VERSION_MIN..=CATALOGUE_FORMAT_VERSION` or pack manifest not matching `PACK_FORMAT_VERSION`.
- `HubModeError::ExcludedCorpusLicence`: refuses to fetch or serve any corpus marked `LicenceClass::Excluded`; corpora marked `LicenceClass::ApproachOnly` attach a `licence_warning` to every returned hit.
- `HubModeError::IntegrityMismatch`: rejects any catalogue or pack whose SHA-256 digest or 40-hex `pin_sha` does not match the expected metadata.
