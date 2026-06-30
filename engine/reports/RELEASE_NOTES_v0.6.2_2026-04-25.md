# xerj v0.6.2 — vector durability

**Tag:** `v0.6.2`
**Date:** 2026-04-25
**Theme:** vector-store correctness + bounded resource use
**Plan:** [PATH_TO_100_PCT_v0.6.0_to_v1.0.md](./PATH_TO_100_PCT_v0.6.0_to_v1.0.md)
milestone v0.6.2 — all four deliverables landed.

This release closes the four production-first items from the
v0.6.2 plan: persistent HNSW graph, soft-delete on the graph,
on-demand segment fsck, and a TTL/sweeper for point-in-time
contexts. Together they remove three classes of correctness
problem the v0.6.0 fairness review surfaced.

ES YAML conformance: **1305 / 1329 (98.2 %)** — within the
historical 1302–1306 variance window; no regression.

## What changes for an operator

### HNSW survives restarts (P1)

Pre-v0.6.2 the HNSW graph was rebuilt on every restart by walking
the WAL and re-inserting every vector. For million-vector indices
this was minutes-to-hours. After a flush + crash the graph could
be lost entirely (the WAL was already checkpointed past the
inserts).

Now: the graph + doc-id ↔ node-id map are persisted alongside
segments under `<index_dir>/hnsw/{graph.bin, ids.json}`, written
atomically on every `_flush`. Restart loads the byte-identical
graph in seconds.

Format `XHNS0001` v2, CRC32C-validated. Loader handles v1 files
(empty tombstone set). Any graph load failure is non-fatal — a
warn!() message and fallback to WAL-replay rebuild.

### Deletes propagate into the kNN graph (P2)

Pre-v0.6.2 a deleted document was unfindable via `_get` / `_search`,
but its vector still came back in kNN results forever (the node
and its neighbour edges stayed in the graph). After v0.6.2, the
delete path tombstones the corresponding HNSW node — search skips
it both as a result and as a kNN neighbour candidate. Tombstones
persist across restarts.

Edges are not rewired; the next graph compaction (planned v0.7+)
will. For now an unbounded delete workload slowly degrades recall
by ~one bit-vector lookup per traversal step. If this matters,
re-create the index.

### Segment fsck on demand (P3)

The 2026-04-25 audit said segment integrity was missing. Re-
examination proved the audit wrong — per-section CRC32C is computed
at write and the whole-file CRC is checked at open. What was
missing: the operator-facing **trigger**.

New endpoint:

```
POST /{index}/_admin/segments/fsck
→ 200 OK on a healthy index
→ 500 Internal Server Error on any corrupt section
   (so external monitors fire on hits, not on greens)
```

Response body:
```json
{
  "total_segments_checked": 17,
  "total_sections_checked": 51,
  "corrupt_sections": 0,
  "segments": [
    { "segment_id": "seg-...", "sections": [{"kind":"Stored","ok":true,"error":null}, ...] },
    ...
  ]
}
```

CPU-bound work runs under `tokio::task::block_in_place` so it
doesn't stall a worker. Run it on a cron, after a hardware event,
or when you suspect bit rot.

### Open PITs no longer leak (P4)

Pre-v0.6.2 every `POST /{idx}/_pit?keep_alive=…` inserted a
context that never expired. A client opening 10 000 PITs and
forgetting to close them held that count of contexts forever.

New `Config.pit` (3 settings):

```toml
[pit]
default_keep_alive_secs = 300       # when ?keep_alive= absent
max_keep_alive_secs     = 86400     # hard cap (24 h)
sweep_interval_secs     = 30        # background reaper cadence
```

`open_pit` parses ES-style `1ms`/`5s`/`2m`/`1h`/`7d` durations,
floors at 1 s, caps at `max_keep_alive_secs`. Background sweeper
walks the DashMap on the configured cadence and drops anything
with `expires_at <= now`. `open_pit` also runs an opportunistic
sweep so a tight open-loop self-bounds without waiting for the
next tick.

## Smaller items shipped along the way

* **Three painless-execute hardenings** carried in from v0.5.9
  (input limits, no-eval — covered in v0.6.0 notes).
* **HNSW format v2** with backward-compatible v1 reader (graphs
  written by v0.6.2 are not readable by v0.6.0 / v0.6.1; graphs
  written by older builds are still loadable).
* **9 HNSW unit tests** now pass (was 7) — round-trip + corruption
  rejection + tombstone exclusion + tombstone persistence.

## Out of scope (slipped to v0.7+)

* **Periodic HNSW checkpointing** outside the flush trigger (v0.7).
  Today we save on `_flush` only. A long-running ingest with no
  flush still has WAL durability for vectors but the graph is
  rebuilt-on-restart in that interval.
* **HNSW graph compaction** to remove tombstoned edges and reclaim
  memory (v0.7+).
* **Chaos test matrix** that proves restart-rebuild cost is gone
  end-to-end (v1.0-rc).

These move the delivery score on the AI/vector domain from 65 %
(v0.6.0) toward the v0.7 target of ~80 %; the residual 20 % is
mostly the AI-execution layers (`hybrid` / `semantic` executors,
reranker, agent loop) that v0.7 ships.

## Upgrade notes

No on-disk format changes for segments / WAL / DV — those still
read v0.5.9 / v0.6.0 / v0.6.1 artifacts unchanged.

HNSW graph file format bumped 1 → 2; loader supports both. A
v0.6.2 → v0.6.0 downgrade fails to load a graph written after the
upgrade — operator runs the next ingest to rebuild from WAL.

Operators with custom `Config` should add (or rely on defaults
for):

```toml
[pit]
default_keep_alive_secs = 300
max_keep_alive_secs     = 86400
sweep_interval_secs     = 30
```

If the previous `_admin/segments/fsck` endpoint shape was relied
on by tooling — it didn't exist, so no migration risk.
