# §12.2 — Binaries only (Corpus Hub v2 release gate)

_Last updated: 2026-10-19. Source of truth: `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`
§12.1–12.2. On any conflict, the implementation spec wins over this checklist._

## What §12.2 requires

The hub hosts **only binaries**: the signed `*.hubpack.tar.zst` (+ `.sig`) pack
releases and the single signed `catalogue.json` (+ `.sig`). The catalogue is the
only JSON served, and it is an index of packs, not corpus data. Everything that
hands a user data to index themselves is retired:

1. **GitHub Releases** — the old lane-B JSON "record pack" releases
   (`rust-vulns`, `agent-session-trajectories`) are deleted once their v2
   hubpacks verify. This is what `retire_v1_releases.sh` (next to this file)
   does.
2. **`pack-publish.yml`** — stops publishing the JSON record packs and publishes
   the two corpora as hubpacks instead (handled by PR #1736).
3. **Hub pages + `llms.txt`** — stop linking per-corpus manifest JSON and stop
   printing `xerj corpus add --from <url>` as the consumer path. The registry
   manifests under `tools/xerj-code/hub/` stay in the repo as *build inputs*
   for `build-pack`; they are not a consumer download.

## Release inventory snapshot (2026-10-19)

737 releases total:

| Class | Tag shape | Assets | Count | §12.2 |
|---|---|---|---|---|
| v2 hubpacks | `pack-<slug>-<8hex-pin>[-rN]` | `<slug>-<pin>.hubpack.tar.zst` (+ `.sig`) | 610 | **keep** |
| v1 JSON record packs | `pack-<slug>-YYYY-MM-DD[-rN]`, slug ∈ {`rust-vulns`, `agent-session-trajectories`} | `*-pack.zip`, `*-freshness.json`, `*-SHA256SUMS`(`.sig`) | 31 | **retire** |
| engine binaries | `v1.0.0-rc.N` | release tarballs/installers (16–20) | 96 | out of scope |

The retire set is **19** `pack-rust-vulns-*` + **12**
`pack-agent-session-trajectories-*` = **31** releases (one of the 31,
`pack-rust-vulns-2026-09-28`, is a draft — the script deletes by release **id**,
so drafts are handled).

### Why the discriminator is safe

The v2 hubpacks for these two corpora will reuse the **same slug prefix**
(`pack-rust-vulns-<pin>`), so the slug alone cannot tell the lanes apart. Two
independent signals do, and the script requires **both**:

- **Tag shape** — v1 is date-suffixed (`YYYY-MM-DD`); v2 is an 8-hex content pin.
- **Assets** — v1 carries `*-pack.zip` / `*-SHA256SUMS` and **no**
  `*.hubpack.tar.zst`; v2 carries `*.hubpack.tar.zst`.

A pre-delete assertion re-checks every selected release against the v2 pin shape
and the hubpack-asset test and aborts the whole run on a single mismatch.

## Dependency: PR #1736 must land first

`retire_v1_releases.sh --execute` **refuses** (exit 3) until a pin-based v2
hubpack release exists for **both** record slugs (published, unless
`--allow-draft-replacement`). As of 2026-10-19 that guard is **UNMET**: zero
`pack-rust-vulns-<pin>` / `pack-agent-session-trajectories-<pin>` hubpack
releases exist yet. #1736 (`feat/record-hubpacks`) adds the `hubpack` job that
builds them; it publishes only once the `RECORD_HUBPACKS` variable says so.
Retiring the live JSON packs before their binary replacements are live would
strand those two corpora.

**Durability precondition:** retiring the 31 releases is only permanent once
`pack-publish.yml` no longer publishes the JSON lane — otherwise the daily cron
re-creates a new dated JSON release. With #1736 merged, the JSON jobs are gated
on the repository variable `RECORD_HUBPACKS`: `publish` publishes the hubpacks
beside the JSON packs, `only` skips the JSON jobs. Set it to `only` (or remove
the `schedule:` from `pack-publish.yml`) before, or together with, the retire.

## hub.xerj.org today

Every probed API path (`/api/v1/catalogue.json`, `…/catalogue.json.sig`,
`…/search`, per-corpus manifest paths) returns an **HTTP 200 serving the landing
HTML** (same 263 KB body) — a catch-all, not a real catalogue. So the signed
`catalogue.json` is **not yet served** at its documented path. "Binaries-only"
is complete when:

- `GET https://hub.xerj.org/api/v1/catalogue.json` returns the signed catalogue
  JSON (not HTML) and `…/catalogue.json.sig` its signature;
- no hub page or `llms.txt` links a per-corpus manifest `.json` or prints
  `xerj corpus add --from <url>` as *the* way to get a hub corpus;
- the only hosted downloads are `*.hubpack.tar.zst` (+ `.sig`) and the catalogue.

(HTTP 200 is not existence — verify the body is JSON, per the llms.txt study.)

## The checklist

Preconditions
- [ ] #1736 merged; `pack-publish.yml` publishes the two corpora as hubpacks and
      **no longer** publishes the JSON record packs.
      Verify: `gh pr view 1736 --json state` → `MERGED`;
      `gh variable get RECORD_HUBPACKS` → `only` (the JSON jobs are skipped),
      or the `schedule:` block is gone from `pack-publish.yml`.
- [ ] A published `pack-rust-vulns-<pin>` **and**
      `pack-agent-session-trajectories-<pin>` hubpack release exists.
      Verify: `retire_v1_releases.sh` prints the guard as satisfied.

Retire the GitHub releases
- [ ] Dry run: `tools/hub-packs/retire_v1_releases.sh` — review the 31 listed.
      Expected: `DRY RUN — nothing was deleted. 31 release(s) would be retired.`
- [ ] Execute: `tools/hub-packs/retire_v1_releases.sh --execute`
      (optionally `--cleanup-tags`). Expected: `retired=31 already-gone=0 failed=0`.
- [ ] Re-run the dry run. Expected: `0 release(s) would be retired`
      (idempotent — proves binaries-only on the releases side).

Consumer surfaces (may be separate PRs / owners — this script does not touch them)
- [ ] `landing/llms.txt` (line ~54) and `landing/llms-full.txt` (lines ~1625)
      stop presenting `xerj corpus add --from ./<slug>.json` as the hub path
      (draft PR #1410, into `main`).
- [ ] Hub pages (`tools/corpus-hub-site/gen.py`, deployed to hub.xerj.org on
      push to `corpus-hub`) print `xerj hub pull <slug>` and the one line
      instead of manifest JSON links (draft PR #1407, into `corpus-hub`).
- [ ] `landing/docs/corpus-hub.html`, `landing/demo/index.html`,
      `landing/blog/the-corpus-hub.html` stop linking per-corpus manifest JSON.
- [ ] `hub.xerj.org/api/v1/catalogue.json` serves signed JSON (not the HTML 200).

Whole-hub gate: §12.4 of the implementation spec governs the final release.
