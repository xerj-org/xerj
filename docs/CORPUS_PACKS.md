# Corpus packs — build, sign, publish, consume

A **corpus pack** is a portable directory of records — advisories, datasets,
any structured data, not source code — built by a declarative *recipe*, with
per-file checksums, licence and provenance on every record, and an optional
**detached ed25519 signature** that proves who built it. Install one with
`xerj corpus add --from <pack>` and it becomes a reference corpus that
`xerj code` and plain ES-compat search both query.

This is the records half of the "corpus hub" roadmap item: the pack carries
*records*, and indexing them still costs each consumer CPU-hours — the
pre-indexed half is tracked separately ([#1030](https://github.com/xerj-org/xerj/issues/1030)).

The tool is **domain-agnostic by design**: what a record is, which fields are
identity, and how duplicates merge all live in the recipe, never in the code.
One recipe indexes advisories, another would index mail or genomes. Licence
terms ride along as record data; the tool does not enforce them — the pack
author owns what goes in a pack.

Every statement below points at the code that makes it true:

| Area | File |
|---|---|
| Pipeline orchestration, `corpus build` CLI | `engine/crates/xerj-autoindex/src/harvest/mod.rs` |
| Recipe parsing + strict validation | `engine/crates/xerj-autoindex/src/harvest/recipe.rs` |
| Source adapters (`dir`, `http-zip`, `http-json`, `git`) | `engine/crates/xerj-autoindex/src/harvest/source.rs` |
| Input-format normalisation (`osv`, `rustsec-md`, `flat`) | `engine/crates/xerj-autoindex/src/harvest/normalize.rs` |
| Content-addressed store (presence = dedup) | `engine/crates/xerj-autoindex/src/harvest/store.rs` |
| Union-find identity resolution | `engine/crates/xerj-autoindex/src/harvest/identity.rs` |
| Deterministic pack emit + checksums | `engine/crates/xerj-autoindex/src/harvest/pack.rs` |
| Mapping/relation suggestions | `engine/crates/xerj-autoindex/src/harvest/suggest.rs` |
| ed25519 keygen / sign / verify | `engine/crates/xerj-autoindex/src/harvest/sign.rs` |
| `corpus add --from <pack>` + `--verify-sig` | `engine/crates/xerj-autoindex/src/xc.rs` |
| Claims pinned to a measured build | `engine/crates/xerj-autoindex/tests/pack_claims.rs` |
| Showcase recipes + stats | `tools/packs/rust-vulns/`, `tools/packs/cisa-kev/` |
| Scheduled build/sign/publish | `.github/workflows/pack-publish.yml` |

## Quick start

```sh
# consume the published, signed rust-vulns pack (see its README for the
# exact curl lines) — verification refuses the pack whole on a bad signature:
xerj corpus add rust-vulns --from rust-vulns-pack.zip --verify-sig rust-vulns.pub
xerj corpus index rust-vulns
xerj code rust-vulns "smallvec insert_many buffer overflow"

# or build the same pack yourself — the recipe is the whole build:
xerj corpus build rust-vulns --recipe tools/packs/rust-vulns/recipe.toml
xerj corpus add rust-vulns --from ~/.xerj-code/builds/rust-vulns/pack/rust-vulns
```

## The recipe

A recipe is a commented TOML file — licence reasoning belongs in comments, and
the recipe ships verbatim inside the pack as provenance. Sections:

- `[[sources]]` — `slug`, `kind` (`dir` | `http-zip` | `http-json` | `git`),
  `url`/`path`, `format` (the normalisation adapter), `licence`, optional
  `glob`, `records_field` (to unwrap a top-level JSON object's array field), and
  watermark. Sources are advisory-listed upstream trees, HTTP zip exports,
  single-URL JSON feeds, or git repos fetched shallow at a pinned ref.
- `[envelope]` — which normalised fields become the universal envelope:
  `id_from`, `title_from`, `body_join`, `defs_from`. `defs` (space-joined
  symbol paths) is what feeds `xerj code`'s definition search.
- `[identity]` — `edges` over field values (`{ field = "id" }`, or
  `{ field = "aliases", each = true }` to union on every array element) and
  `canonical_source_order`, which decides the canonical id when sources
  disagree. Union-find closes alias chains for free: A↔CVE↔B merges A and B
  even with no direct A↔B edge.
- `[merge]` — `precedence` (which source wins a disagreeing scalar; arrays are
  always unioned, never chosen). The merged record's `licence` is the most
  restrictive of its contributing sources, and `sources[]` names them all.
- `[[derived]]` — pure post-merge ops (`regex_extract`, `present`, …) for
  fields like `cve_ids` or `is_withdrawn`.
- `[emit]` — pack shape (`shards`).

Validation is strict: an unknown key is a loud error, not a silent ignore.

The full example, with per-source licence reasoning in comments, is
[`tools/packs/rust-vulns/recipe.toml`](../tools/packs/rust-vulns/recipe.toml).

## Incremental by construction

There is no `--sync` mode to get wrong. Every normalised record is stored at
`store/<slug>.<xxh3-of-canonical-json>.json`; a re-run recomputes the key,
sees the file, and skips. Only genuinely new or changed records are written,
and the pack is re-emitted deterministically (fixed shard count, records
ordered by id, a uniform key set on every record) from whatever the store
holds. `--fresh` discards the build state for a full re-harvest.

Build state lives under `~/.xerj-code/builds/<name>/` (override the root with
`XERJ_CODE_HOME`) — persistent across reboots, never under `/tmp`.

## The pack format

```
pack/<name>/
  manifest.json          # format_version (readers refuse unknown), counts,
                         # sources + watermarks, per-file sha256
  records-*.jsonl        # the records, sharded by hash(id), id-ordered
  recipe.toml            # verbatim, as provenance
  mapping.suggested.json # suggested field mapping — never applied
  suggestions.md         # human-readable suggestions + join candidates
  relations.jsonl        # candidate relations with cardinality
  SHA256SUMS             # sha256 of every file above
  SHA256SUMS.sig         # optional: raw 64-byte ed25519 over SHA256SUMS
```

`corpus add --from <pack>` verifies every checksum before anything is
materialized, and refuses an unknown `format_version` outright — a published
pack outlives the release that built it, so readers gate on the version they
were written for. The suggestions are **suggestions, not decisions**: the
index infers the real mapping at index time, as it does for any folder.

## Signatures

Checksums prove a pack arrived intact. A signature proves **who built it**.
The two answer different attacks, and the difference is pinned by a test: a
self-consistent rebuild (tampered records with honestly rewritten checksums)
passes every checksum and fails the signature — that is the attack the
signature exists for.

```sh
# keygen: writes <prefix>.key (SECRET — the seed) and <prefix>.pub
xerj corpus keygen --out rust-vulns

# sign (CI or wherever the seed lives; the build itself never signs)
xerj corpus sign <pack-dir> --key rust-vulns.key

# verify on the way in — before anything is materialized or indexed
xerj corpus add <name> --from <pack> --verify-sig rust-vulns.pub
```

Design rules, each load-bearing:

- **The public key travels out of band** — committed beside the recipe, listed
  on the project site, never inside the pack. A key shipped beside its own
  signature verifies nothing.
- **Build ≠ publish.** `corpus build` emits; it never signs. The seed exists
  only where signing happens (a CI secret in our workflow), and the workflow
  verifies its own output against the committed `.pub` before publishing — a
  half-rotated key fails the build instead of shipping an unverifiable pack.
- **The signature covers SHA256SUMS**, which covers every pack file; the
  signature itself is deliberately not listed in SUMS (it would be circular).
- **A signature proves origin, not safety.** A verified pack is exactly what
  the key holder built — indexing it is still your decision, and a pack
  remains untrusted input to the same degree an indexed folder is.

The released pack is signed with a single v1 release key; the rotation
procedure is in
[`tools/packs/rust-vulns/README.md`](../tools/packs/rust-vulns/README.md#signatures-and-key-rotation).

## Publishing

The [pack publish workflow](../.github/workflows/pack-publish.yml) rebuilds
and signs `rust-vulns` daily and attaches the zip, the loose checksums, the
signature, and a freshness file to a GitHub Release per build day — dated
tags `pack-rust-vulns-YYYY-MM-DD`, newest first at
[releases?q=pack-rust-vulns](https://github.com/xerj-org/xerj/releases?q=pack-rust-vulns).
This repository has immutable releases (release tags are single-use), which
is why the pack ships one release per day instead of one rolling URL —
each day's release is exactly the bytes that shipped, pinned by its
signature, and the workflow keeps only the newest 7.
Freshness is an operational promise the schedule keeps, not a README claim:
the pack's own 30-day staleness refusal applies to our published corpus
exactly as it does to a user's reference corpora.

Publishing your own pack needs none of this — a pack directory is
self-contained, and `corpus add --from <dir|zip>` is the whole install. The
release machinery exists because *our* pack promises freshness.

## Honest limits

- **Indexing still costs each consumer the same CPU-hours.** The pack
  distributes records, not a built index — the pre-indexed half is
  [#1030](https://github.com/xerj-org/xerj/issues/1030).
- **A pack is only as good as its sources and its recipe.** Identity
  resolution merges what the recipe's edges say to merge; a wrong edge is a
  wrong merge, visibly, in `sources[]`.
- **Not a security product.** The showcase pack is a searchable corpus of
  advisories, not a scanner, and makes no completeness claim beyond "what its
  sources carried at build time".
- **The suggester never mutates records.** Field types, joins and relations
  are hints computed from a sample; the index decides at index time.
