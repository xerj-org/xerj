# rust-vulns — the showcase corpus pack

[![pack publish](https://github.com/xerj-org/xerj/actions/workflows/pack-publish.yml/badge.svg)](https://github.com/xerj-org/xerj/actions/workflows/pack-publish.yml)
[![rust-vulns pack: signed · daily builds](https://img.shields.io/badge/rust--vulns%20pack-signed%20%C2%B7%20daily%20builds-blue)](https://github.com/xerj-org/xerj/releases?q=pack-rust-vulns)

Rust vulnerability advisories, **identity-resolved across sources**: the same
vulnerability that arrives as `RUSTSEC-2021-0003`, `GHSA-43w2-9j62-hq99` and
`CVE-2021-25900` is one record, carrying every field any source knew — and
the curated vulnerable-function paths no export carries.

Built with `xerj corpus build` from this directory's
[recipe.toml](./recipe.toml). Every number below is measured from the actual
build ([pack-stats.json](./pack-stats.json), `provenance: measured`) and a
test pins README against stats so the two cannot drift apart.

| What | Count |
|---|---|
| Source records harvested (envelopes) | 4107 |
| Records after identity resolution | 1950 |
| …merged from both sources | 1221 |
| …canonical id RUSTSEC / GHSA / MAL | 1221 / 711 / 18 |
| Records with affected-function paths | 270 (663 distinct paths) |
| Records withdrawn (kept, flagged `is_withdrawn`) | 124 |
| Records with a CVE alias | 992 |
| Packages covered | 1813 |
| Records JSONL bytes | 10140789 |
| Cold build (network fetch + harvest + pack) | ~34 s |

## Why this pack exists

Each source alone is incomplete:

- **osv.dev's crates.io export** (CC0, 2,856 records) never merges aliases —
  the same vulnerability sits there as separate GHSA and RUSTSEC records
  sharing a CVE — and carries **zero** affected-functions data.
- **RustSec's advisory-db** (CC0, 1,251 native advisories) is the only
  source of the hand-curated `affected.functions` paths, CVSS vectors,
  categories, keywords and full advisory prose — but has no GHSA-only
  records.

The recipe's union-find over `id` + every `aliases` element closes the gap:
4,107 envelopes collapse to 1,950 records. Sibling filings union their
function lists — e.g. one 2026 CVE filed against three `libcrux-*` crates
becomes one record carrying all three crates' function paths.

GHSA is deliberately **not** a direct source: its crates.io records are
already inside the osv.dev export under CC0, and the only fields a direct
`github/advisory-database` clone would add live in `database_specific`,
which the tool never reads (upstream marks it internal-use-only). HackerOne
content is unredistributable under its ToS and is out of scope — the pack
author owns what goes in a pack.

## Build, consume, query

The published pack is a GitHub Release per build day (dated tags
`pack-rust-vulns-YYYY-MM-DD`, newest first:
[releases?q=pack-rust-vulns](https://github.com/xerj-org/xerj/releases?q=pack-rust-vulns)),
built and signed daily by the
[pack publish](https://github.com/xerj-org/xerj/actions/workflows/pack-publish.yml)
workflow. This repository has immutable releases — release tags are
single-use and yesterday's release is never modified — so there is no
one-rolling-URL; each day's release is exactly the bytes that shipped,
pinned by its signature.

```sh
# consume the published, signed pack:
#   1. fetch the zip and signature from the newest pack release, and the
#      public key (the key travels OUT of band — next to the recipe in
#      this repo, never inside the pack)
gh release download -R xerj-org/xerj \
  -p 'rust-vulns-pack.zip' -p 'rust-vulns-SHA256SUMS.sig' \
  "$(gh release list -R xerj-org/xerj -L 100 --json tagName \
     --jq '[.[].tagName | select(startswith("pack-rust-vulns-"))] | .[0]')"
curl -LO https://raw.githubusercontent.com/xerj-org/xerj/main/tools/packs/keys/rust-vulns.pub

#   2. install it with signature verification — refuses the pack whole if
#      the signature does not verify, before anything is indexed
xerj corpus add rust-vulns --from rust-vulns-pack.zip \
  --verify-sig rust-vulns.pub

#   3. index and query
xerj corpus index rust-vulns
xerj code rust-vulns "smallvec insert_many buffer overflow"
```

Or build it yourself from this directory (the recipe is the whole build):

```sh
# build (or incrementally refresh) — fetches both sources, dedups, packs
xerj corpus build rust-vulns --recipe tools/packs/rust-vulns/recipe.toml
# pack lands at $XERJ_CODE_HOME/builds/rust-vulns/pack/rust-vulns/

# consume: install as a reference-coding corpus
xerj corpus add rust-vulns --from ~/.xerj-code/builds/rust-vulns/pack/rust-vulns

# query it — the envelope targets both ES-compat search and `xerj code`
xerj code rust-vulns "smallvec insert_many buffer overflow"
```

The pack ships `suggestions.md` / `mapping.suggested.json` /
`relations.jsonl` — field-type verdicts and join candidates computed from a
sample. Suggestions, not decisions: the index infers the real mapping.

## Fields worth knowing

| Field | What it carries |
|---|---|
| `id` / `aliases` | canonical advisory id and every alias (CVE/GHSA/RUSTSEC) unioned across sources |
| `cve_ids` | derived: CVE references extracted from id + aliases |
| `affected_functions` | RustSec's curated vulnerable-function paths — **unique to this pack** |
| `defs` | affected_functions joined — feeds `xerj code`'s symbol search |
| `patched_versions` / `unaffected_versions` | RustSec version requirements |
| `introduced_versions` / `fixed_versions` | osv.dev range events, unioned |
| `severity` / `severity_scores` | CVSS vectors |
| `categories` / `keywords` | RustSec facets |
| `is_withdrawn` / `informational` | lifecycle flags — withdrawn records stay, flagged |
| `licence` / `sources` / `origin` | provenance per record |

## Signatures and key rotation

`SHA256SUMS.sig` is a raw 64-byte ed25519 signature over the pack's
SHA256SUMS (which covers every pack file). The verifying key lives at
[keys/rust-vulns.pub](../keys/rust-vulns.pub) here and on xerj.org — never
inside the pack, because a key shipped beside its own signature verifies
nothing. Checksums prove the pack arrived intact; the signature proves who
built it. A self-consistent rebuild (valid checksums over tampered records)
passes checksums and fails the signature — that is the attack the signature
exists for, and a test in `harvest/sign.rs` pins it.

v1 uses a single release key. Rotation: generate a new keypair
(`xerj corpus keygen`), set the new `PACK_SIGNING_SEED` secret, commit the
new `.pub` in the same change, and let the next scheduled publish run — the
workflow verifies its own output against the committed `.pub` before
publishing, so a half-rotated key fails the build instead of shipping an
unverifiable pack. Consumers re-fetch the `.pub` when they choose to trust
the new one.

## Honest limits

- **Freshness is whatever the last build fetched.** The scheduled workflow
  rebuilds daily; the newest release's `rust-vulns-freshness.json` carries
  its build date and record count. Between the sources updating and the next
  cron tick, the pack lags by up to a day — a rebuild you run yourself is
  always current.
- **Not a security product.** This is a searchable corpus of advisories for
  agents and humans; it is not a scanner and makes no completeness claim
  beyond "what these two sources carried at build time".
- Withdrawn (124) and informational advisories are kept and flagged, not
  dropped — filtering is the consumer's decision.
- The signature proves origin, not safety: a verified pack is exactly what
  the key holder built — indexing it is still your decision.
