# index-size — design notes and staged plan

Working notes for the on-disk-size effort (epic #1038). The harness itself is
[README.md](./README.md); this file is *why the stages are ordered the way
they are*, the measurements that reshaped them, and the peer anchors. Every
number here traces to a committed `results/*.json` from a harness run or to
the offline A/B scripts described alongside it — nothing is derived.

## Measured starting point

`demo/playbooks/DISK_SIZE_2026-07-09.md`, 100k docs force-merged to one
segment (see README for the level table and the comparability caveat — that
corpus's body generator is not the committed one; structure, not absolute
totals, carries over):

```text
.post 53.5 % (body.post alone 37.7 %) · .seg 19.9 % · .dv 17.5 %
.ids 5.3 % · .meta 2.2 %
```

## Where the waste is (anchors verified against the code)

1. **One global bit width per `DictBitpack` column**
   (`engine/crates/xerj-storage/src/stored_codec.rs`, `encode_dict_bitpack`)
   — a single rare high ordinal inflates every row.
2. **Numeric residuals are decimal JSON text** — an f64 like `0.010127`
   costs 8 ASCII chars instead of a delta-friendly binary form.
3. **`.dv` `NumericColumn` was raw 8 B i64/doc** (`doc_values.rs`) — fixed
   with ZNV2 (#1040): FOR + GCD + 128-doc bit-packed blocks, per-column
   codec chosen by measured zstd size.
4. **Postings block headers carried derivable bytes** (`postings.rs`) —
   fixed with ZPS2 (#1041): derivable `byte_len` dropped, FOR baseline with
   a banded rule, RLE-able streams handled by the existing zstd layer.
5. **ZFM3 records are 24 B/term with a monotonic offset** — never
   delta-encoded; `ttf == df` for single-valued keyword fields duplicates
   8 bytes. (`.meta` is 2.2 % of bytes; measure the post-compression payoff
   before building it.)
6. **The same keyword strings are stored up to three times per segment** —
   FTS term dictionary (`.fst`), `.dv` keyword FST, ZBS2 dict entries.
7. **Zero zstd dictionaries anywhere** — every per-(segment,field) sidecar
   restarts entropy history cold.

## What measurement did to the `.seg` plan (stage 1, ZBS4)

The original stage-1 `.seg` item was dict-stream work: per-block widths,
binary dict entries, typed numeric residuals. Then we dissected an actual
harness segment (`node-zps2c-balanced`, 100k docs, `.seg` = 1,663,405 B)
with a container walker (XAYA section table → Stored 0x04 → per-column
payloads):

| column | bytes | share of `.seg` | codec |
|---|---:|---:|---|
| `body` | 1,400,153 | 84.2 % | RAW_JSON, zstd-3 |
| `__seq_no` | 74,361 | 4.5 % | RAW_JSON (textual consecutive ints) |
| `__id` | 54,437 | 3.3 % | RAW_JSON |
| `doc_id` | 54,437 | 3.3 % | byte-identical to `__id` |
| every DICT column | ≈ 84 KB total | ≈ 5 % | DICT_BITPACK |

The dict streams the plan targeted are ~5 % of the section. The bytes are in
the RAW fallback whale and in two shapes the plan never named: a textual
integer column and a duplicated column. So ZBS4 (#1038) is three codecs
instead:

- **TYPED_INT** — all-integer columns as presence-bitmap + 128-value
  frame-of-reference bit-packed blocks (`[width][zigzag-varint min]`,
  width 0 = constant block — the ZPS2 `.post` block shape) + zigzag-varint
  delta residual. u32 lanes; a block whose spread exceeds them keeps the
  JSON fallback.
- **COPY_OF** — a later column byte-identical to an earlier pass-1 column
  stores `u32 src_col_ix` instead of a payload (digest + full-equality
  verify before referencing).
- **Per-column zstd effort chooser (merge path only)** — each JSON fallback
  column is additionally compressed at level 19 with long-distance matching
  and an explicit window (≤ 16 MB), and the smaller payload wins — the same
  measured-chooser rule as the `.dv` ZNV2 codec.

Offline A/B on the exact column payloads (zstd CLI, pinned versions, files
under `/tmp/sizebench`, scripts not committed):

| column | raw | zstd-3 (today) | best alternative | Δ |
|---|---:|---:|---:|---:|
| `body` (29,813,448 B of JSON) | — | 1,398,216 | **330,545** (z19 + LDM, window 16 MB) | −76.4 % |
| `__seq_no` (588,896 B) | — | 51,963 | **≈ 2.4 KB** (typed FOR) | −95 % |
| `__id`/`doc_id` (1,988,891 B) | — | 54,402 | **4 B** (copy ref, second copy) | −100 % |

Two lessons that shaped the chooser, both measured on those same files:

- **The window knee is sharp, not gradual**: body at z19 default window
  679,452 B; 8 MB window 679,238 B; **16 MB 330,545 B**; 25–27 MB flat.
  The sentence pool repeats at distances just past a small window.
- **High levels regress small-alphabet streams**: `__id` at z3 is 54,402 B
  but at z19 is 143,128 B — *worse*. A global "use 19" knob would grow the
  id columns while shrinking body; only a per-column measured chooser is
  safe. This is why the chooser keeps whichever candidate is smaller
  rather than trusting the level.

CPU: z19+LDM measured ≈ 4.08 s per 30 MB column (one pass, background
merge). Flush never runs it — the level-19 ingest regression
(`engine/reports/2026-04-25T21-50-00_ingest_perf_regression_zstd19.md`) is
the standing reason flush is pinned to zstd-3, and ZBS4's typed/copy codecs
are O(rows) with no zstd at all, so they are safe on the flush path.

Format compatibility: the ZBS4 magic is written **only** when a column
actually uses codec 5/6 or a form-1 RAW payload; every segment expressible
as ZBS2 is byte-identical ZBS2 (flush-parity contract and downgrade blast
radius unchanged). Old readers reject the new codec ids cleanly at
directory parse; ZBS3 present-null wrapping composes unchanged.

## Peer anchors (reference-coding, postings-peers corpus)

Corpus: tantivy @ 6182c6062f4d (MIT). What we took and what we did
differently, per the repo's licence rules (cite `file:line` when adapting;
elasticsearch/sonic are approach-only and were not used here):

- **tantivy docstore** — `src/store/mod.rs:5-14`: docs serialised to a
  buffer, compressed per ~16 KB block (LZ4/Zstd), skip list, whole-block
  decompression per DocId. Confirms block-compressed column/row stores as
  the peer shape; XERJ's stored section stays columnar instead (ZBS2+),
  trading tantivy's simpler row blocks for cross-column structure.
- **default zstd level 3** — `src/store/compressors.rs:82`: tantivy also
  defaults to level 3 (same CPU/size balance on the write path).
- **plain bulk compression** — `src/store/compression_zstd_block.rs:9-33`:
  `compress_to_buffer` with no window/LDM/dict tuning; per-index
  `docstore_compression` level + blocksize as the only knobs
  (`src/index_meta.rs`). I.e. **no peer ships a per-column effort chooser,
  typed-int stored columns, or duplicate-column references** — those are
  XERJ contributions, measured here.
- Parquet's "pick the smallest per-column encoding" philosophy (via the
  `.dv` ZNV2 work, #1040) is the rule the chooser follows.

## Staged plan

- **Stage 0 — measurement** ✅ #1039: this harness + informational CI pass.
- **Stage 1** — header/delta wins inside existing envelopes (merge converts
  old segments):
  - ✅ `.dv` FOR+bitpack (ZNV2, adaptive chooser) — #1040. 100k balanced:
    `.dv` 390,487 → 355,156 B (−9.0 %), total −0.9 %.
  - ✅ postings header slimming + FOR + freq elision (ZPS2) — #1041.
    100k balanced A/B: `.post` −3.4 %, total −1.5 %.
  - 🚧 ZBS4 stored-codec slim (this PR): typed ints, copy-of refs,
    merge-path chooser. Harness A/B in `results/`.
  - ZFM3 offset deltas + `ttf`-elision — measure first: `.meta` is 2.2 %
    and zstd already eats adjacent-term redundancy.
  - `xerj_bytes_written_total` beyond `.seg`.
- **Stage 2 — FTS dictionary reuse**: stored/doc-values keyword columns
  reference FTS term ordinals; deletes copies 2 and 3 of waste item 6.
- **Stage 3 — zstd trained dictionaries** at merge, attached as a new
  bundle family role. Expected to pay on the many-small-files tail, not the
  `body.post` whales; A/B honestly against plain zstd and the level
  ceiling.

## Constraints (bind every stage)

- ES-YAML conformance stays **0 failed**; the runner does `DELETE /_all` —
  private port + throwaway data dir only.
- Flush-parity byte-identity (`XERJ_FLUSH_PARITY=1`) between the two flush
  entry points; cross-dep re-derivation must hold; row hydration keeps
  random access.
- Flush pinned to zstd 3; merge is the sanctioned place for compression
  CPU.
- Honest-claims: no public number without a harness run; CI
  `[profile.ci-test]` numbers never become published cells; the 1.61×-vs-ES
  cell keeps its own basis.
