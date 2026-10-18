# Hub backfill pre-flight (§12.1), 2026-10-17

What this is: a prediction, made before the backfill runs, of which corpora
`hub-packs.yml` will fail to build, so that the 40-corpus batches are not spent
on rows that are already known to fail. Nothing was dispatched or published.
The backfill report §12.1 names (`benchmarks/hub-backfill/REPORT.md`, not yet
written) records what the real runs do. Where the two
disagree, `REPORT.md` is right.

## The plan

The planner was run the same way `hub-packs.yml` runs it:
`hub_packs.py plan --registry tools/xerj-code/hub --backlog
tools/xerj-code/hub/backlog/backlog-100.json --published-tags <tags>`. The tags
came from `gh release list` (non-draft `pack-*`: 29 tags, and lz4, snappy and zstd
already have hubpacks). The base was `origin/corpus-hub` @ `414ec00d`. After
rebasing onto `3d93fae9` (#1699, main merged into corpus-hub), the planner was
run again and produced the same 506 slugs.

- Before this PR: **509 due**, which is 13 batches of 40 and 0 planner warnings.
- After this PR: **506 due**. Three dead corpora are withdrawn.

## Verdicts (509 planned rows)

| verdict | count |
|---|---:|
| PASS, pack under the 50 MiB auto-pull cap | 268 |
| PASS, cap uncertain (estimate straddles 50 MiB) | 99 |
| PASS, over the 50 MiB cap (a notice, not a failure) | 117 |
| RISK: may exceed the runner's time or memory | 16 |
| FIXED in this PR (dead pin repinned), then RISK on size | 1 |
| **FAIL**: needs an owner decision | 5 |
| **DELIST**: withdrawn in this PR (repository 404) | 3 |

- **Will pass: 484.**
- **At risk: 17** (16 RISK rows plus ghsa-db).
- **Will fail: 5** (exploit-pocs-2023/2024/2025/2026 and xerj-blogposts).
- **Delisted: 3**.

Licence class (most restrictive across repos, the catalogue builder's rule):
499 adapt-with-attribution, 10 approach-only, **0 Excluded**. No
registry `review.use` is `excluded`, so no corpus leaves the catalogue on licence
grounds. One gap remains: `hub_packs.py plan` never reads the licence class. If a
row is ever marked `excluded`, the planner would still schedule its pack, although
§12.1 says such a corpus gets no pack. Three adapt-with-attribution corpora record
`spdx: NONE-FOUND` (ada-2010, eurlex-core, uk-building-regs: government texts).
Their licence review should name the actual terms before the packs are published.

## Top failure reasons

1. **Pin or repository gone (4 corpora, fixed here).**
   - xerj-org/corpus-ietf-rfc-abstracts, corpus-redos-precedent and
     corpus-rust-vulns-txt return 404 to the GraphQL API, to REST with the owner's
     token, and to `git fetch`. No other repository serves those shas (commit
     search). They are withdrawn the same way #1641 withdrew crypto-misuse-precedent:
     a `withdrawn` reason, `repos: []`, backlog `status: withdrawn` plus a note, and
     the skill table updated.
   - ghsa-db pinned `0af427f6`, which `github/advisory-database` does not have
     ("not our ref"). It is repinned to `4b4ac300e0f6718d5e56e9a410069894c3a22cb7`,
     the commit its own review note names (2026-10-03). This follows the go-vulndb
     precedent in #1641.
   - **Caveat:** ghsa-db's url is the RAW upstream, not the flattened corpus the
     pin described (the repo name is `corpus-ghsa-db-v2`). The tree at 4b4ac300 is
     larger than 57,186 entries (the trees API truncates), and the review note
     records an 11.3 GB failed raw attempt. Expect a timeout or OOM until a flatten
     mirror exists. go-vulndb has the same shape: its pin is upstream golang/vulndb
     (11,224 entries, 35 MB), not the 4.2 MB flatten.
2. **exploit-pocs-2023/2024/2025/2026 cannot build as they stand.**
   - `corpus add` stops on the first repository that does not land ("this corpus
     is INCOMPLETE", exit 1). This was reproduced locally on a 4-repo subset.
   - Across the four corpora, 2,046 pins are not in their repository and 59
     repositories 404.
   - Root cause in 2023-2025: rows share directory names. For example, 31 rows in
     exploit-pocs-2023 are named `CVE-2023-23752`. The recorded sha is one per NAME:
     there are 1,977 distinct shas for 1,978 distinct names. Every collided row
     carries a foreign sha.
   - 2026 uses `owner--name` and has only 45 dead rows, but its 4,612 repos and
     4.3 GiB source would not finish in a 90-minute job.
   - Options for the owner: prune the dead rows with a recorded list, then rename
     and repin the collided rows; or move these corpora to a lane-B record pack.
     Do not let the backfill burn batch 5 on them.
3. **xerj-blogposts is a recipe, not a git manifest.**
   - It has `kind: harvested` and a `recipe`. `corpus add --from` refuses it in
     0.01 s ("cannot read manifest.json"), and so does `build-pack`.
   - Building it as a plain git manifest would index the whole xerj repository
     (skill rule 3). It needs the recipe path or lane B. Either is an owner decision.
4. **Runner limits (16 RISK rows).** These rows are above 300 MiB of
   source or 15k files: `biome`, `dagger`, `dailymed`, `duckdb`, `exploitdb-src`, `freebsd-handbook`, `github-api-description`, `kics`, `openbsd-faq`, `rolldown`, `rspack`, `uk-legislation`, `vuln-fix-commits`, `webgl`, `xerj-columnar`, `xerj-search`.
   - Local measurements on 32 cores: dailymed (18,655 files) took 22 min. The node
     process peaked at 7.4-10.8 GiB RSS on the three builds that succeeded.
   - The runner has 4 vCPU, 16 GB and a 90-minute job ceiling.
   - Peak memory on 4 cores may be lower, because fewer threads run. Time will be
     higher. Treat these rows as the first candidates for a dedicated batch
     (`slugs:`) rather than mixing them into the alphabetical batches.

## Pack size vs the 50 MiB cap

Being over the cap is a `::notice::` in `hub-packs.yml`, not a failure. Clients then
need an explicit `xerj hub pull`. Measured packs are **1.5-5.3x the source bytes**
(index plus defs plus snapshot), so most corpora above about 10 MB of source will
be over the cap:

| corpus | source B | pack B | ratio |
|---|---:|---:|---:|
| lz4 (published) | 1,711,696 | 4,623,814 | 2.70 |
| snappy (published) | 3,377,428 | 11,338,487 | 3.36 |
| zstd (published) | 8,520,472 | 21,038,204 | 2.47 |
| curl-src (local) | 18,450,259 | 97,640,437 | 5.29 |
| dailymed (local) | 28,054,872 | 101,025,250 | 3.60 |
| gitlab-runbooks (local, gitlab.com) | 162,916,404 | 252,843,118 | 1.55 |

The table's size estimates are `source x 1.55` to `source x 5.29`. That is a range
from six packs, not a model.

## Pipeline defect found on the way (fixed here)

`.gitignore` ignores every `*.md` and had no rule that re-included
`benchmarks/hub-backfill/`. The `backfill report` job in `hub-packs.yml` runs
`git add benchmarks/hub-backfill/REPORT.md benchmarks/hub-backfill/rows.jsonl`
under `set -euo pipefail`. Reproduced with this repository's `.gitignore`: that add
exits 1 ("paths are ignored"), so the job fails and REPORT.md is never committed.
With `!benchmarks/hub-backfill/*.md` added, the same script exits 0.

## Method

- **Pins:** one batched GraphQL query per 40 repos covered 14,579 GitHub repos (365
  queries at ≤4 concurrent, 1 point each). Each query asked for
  `repository{diskUsage, licenseInfo, object(oid: sha)}`. The 2 gitlab.com repos
  were checked with the builder's own command, `git fetch --depth 1 origin <sha>`.
  `git ls-remote` was not used to judge pins, because it lists ref tips and cannot
  confirm an arbitrary commit. Spot checks with the real fetch agreed with GraphQL
  in both directions.
- **Size:** the manifest's `bytes` (worktree bytes without `.git`, as `corpus
  add` records them), cross-checked against GitHub `diskUsage`.
- **Builds:** `engine/target/release/xerj hub build-pack <slug> --registry ...
  --out ... --url http://127.0.0.1:<port> --sign-key <throwaway seed>`. Each
  build got one throwaway node, configured with the workflow's
  `snapshot_repo_allowlist`, and a private `XERJ_CODE_HOME`. Seven builds ran:
  curl-src, dailymed and gitlab-runbooks (pass); redos-precedent and ghsa-db at
  the old pins (fail); a 4-repo exploit-pocs-2026 subset (fail); xerj-blogposts
  (fail).
- **Build note:** a node binds `port`, `port+1` and `port+2`. Builders started on
  adjacent ports collide ("Address already in use"), so space them 10 apart. The
  workflow uses one fixed port per VM and is not affected.

## Full table

Batch = the `--max-builds 40` run this row falls in, after this PR (alphabetical, 506 due).

| slug | pin8 | batch | licence | predicted | reason |
|---|---|---:|---|---|---|
| 12-factor-agents | `d20c7283` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 70-238 MiB: builds, but clients never auto-pull it |
| 18f-content-guide | `1b1723d3` | 1 | adapt | PASS | pack est. 0.6-2.2 MiB, under cap |
| a11y-project-content | `27de2268` | 1 | adapt | PASS (cap uncertain) | pack est. 35-118 MiB straddles the cap |
| accord-legal-models | `a23b30a1` | 1 | adapt | PASS | pack est. 10.3-35.2 MiB, under cap |
| act | `4f411281` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 168-575 MiB: builds, but clients never auto-pull it |
| ada-2010 | `d2be9eb3` | 1 | adapt | PASS | pack est. 0.8-2.9 MiB, under cap; licence review: spdx NONE-FOUND on an adapt-with-attribution corpus |
| adalflow | `810de99d` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 64-219 MiB: builds, but clients never auto-pull it |
| adyen-openapi | `45520ee6` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 84-286 MiB: builds, but clients never auto-pull it |
| aeron | `d3499bb1` | 1 | adapt | PASS (cap uncertain) | pack est. 31-107 MiB straddles the cap |
| age | `b74dce4c` | 1 | adapt | PASS | pack est. 1.0-3.5 MiB, under cap |
| agentbench | `d1e4a10d` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 67-228 MiB: builds, but clients never auto-pull it |
| agentops | `f8e907b9` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 258-881 MiB: builds, but clients never auto-pull it |
| agentscope | `812eaecd` | 1 | adapt | PASS (cap uncertain) | pack est. 25-86 MiB straddles the cap |
| agno | `1f2aac51` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 91-310 MiB: builds, but clients never auto-pull it |
| aider | `5dc9490b` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 115-392 MiB: builds, but clients never auto-pull it |
| aiohttp | `70852859` | 1 | adapt | PASS | pack est. 7.9-27.0 MiB, under cap |
| aioquic | `974e9349` | 1 | adapt | PASS | pack est. 1.8-6.1 MiB, under cap |
| alertmanager | `55b27cfb` | 1 | adapt | PASS | pack est. 6.1-20.9 MiB, under cap |
| alex-inclusive-language | `9a57595d` | 1 | adapt | PASS | pack est. 0.7-2.5 MiB, under cap |
| ann-benchmarks | `2e081ad3` | 1 | adapt | PASS | pack est. 4.0-13.7 MiB, under cap |
| annoy | `379f7446` | 1 | adapt | PASS | pack est. 1.2-4.3 MiB, under cap |
| anthropic-cookbook | `9c1e493d` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 322-1099 MiB: builds, but clients never auto-pull it |
| anything-llm | `3e4f9411` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 88-300 MiB: builds, but clients never auto-pull it |
| api-blueprint | `86fc3a12` | 1 | adapt | PASS | pack est. 0.6-2.1 MiB, under cap |
| api-common-protos | `3332dec5` | 1 | adapt | PASS | pack est. 0.5-1.8 MiB, under cap |
| api-linter | `836098d5` | 1 | adapt | PASS | pack est. 4.2-14.4 MiB, under cap |
| arazzo-spec | `79ddb24a` | 1 | adapt | PASS | pack est. 8.1-27.7 MiB, under cap |
| argo-cd | `03a10c74` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 333-1136 MiB: builds, but clients never auto-pull it |
| argo-workflows | `8f57ec73` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 123-421 MiB: builds, but clients never auto-pull it |
| aria-practices | `0f765e4d` | 1 | adapt | PASS | pack est. 13.5-46.1 MiB, under cap |
| arkworks-algebra | `4a19a1c5` | 1 | adapt | PASS | pack est. 4.0-13.5 MiB, under cap |
| arrow-rs | `73595d58` | 1 | adapt | PASS (cap uncertain) | pack est. 31-107 MiB straddles the cap |
| arroyo | `18c203dc` | 1 | adapt | PASS (cap uncertain) | pack est. 32-109 MiB straddles the cap |
| astro | `59b9cc00` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 76-260 MiB: builds, but clients never auto-pull it |
| astro-docs | `f29faa41` | 1 | adapt | PASS (over 50 MiB cap) | pack est. 119-405 MiB: builds, but clients never auto-pull it |
| asyncapi-bindings | `1f418fe7` | 1 | adapt | PASS | pack est. 0.9-3.0 MiB, under cap |
| asyncapi-spec | `1dd65fd2` | 1 | adapt | PASS | pack est. 2.6-9.0 MiB, under cap |
| asyncapi-spec-json-schemas | `00af2905` | 1 | adapt | PASS | pack est. 7.4-25.3 MiB, under cap |
| asyncapi-studio | `f3aa74ec` | 1 | adapt | PASS | pack est. 8.6-29.3 MiB, under cap |
| atomic-agents | `4d443474` | 1 | adapt | PASS (cap uncertain) | pack est. 17-58 MiB straddles the cap |
| attack-flow | `8b6ca28b` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 56-192 MiB: builds, but clients never auto-pull it |
| attack-mappings-explorer | `a85e55af` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 93-318 MiB: builds, but clients never auto-pull it |
| autogen | `027ecf0a` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 71-244 MiB: builds, but clients never auto-pull it |
| autorag | `ccb70c12` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 128-436 MiB: builds, but clients never auto-pull it |
| avro | `28cb08c1` | 2 | adapt | PASS (cap uncertain) | pack est. 26-90 MiB straddles the cap |
| awesome-ai-system-prompts | `74964625` | 2 | adapt | PASS | pack est. 1.8-6.2 MiB, under cap |
| awesome-llm-apps | `4bf51ab7` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 133-454 MiB: builds, but clients never auto-pull it |
| awesome-mcp-servers | `cc7f1f42` | 2 | adapt | PASS | pack est. 3.5-12.1 MiB, under cap |
| awesome-sre | `e91ef69a` | 2 | adapt | PASS | pack est. 0.1-0.5 MiB, under cap |
| axe-core | `7e287a6a` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 69-234 MiB: builds, but clients never auto-pull it |
| axolotl | `e767fe07` | 2 | adapt | PASS (cap uncertain) | pack est. 25-87 MiB straddles the cap |
| badger | `5688f140` | 2 | adapt | PASS | pack est. 2.0-6.9 MiB, under cap |
| bandit-plugins | `68ebe11e` | 2 | adapt | PASS | pack est. 7.4-25.3 MiB, under cap |
| bebop | `03a2a761` | 2 | adapt | PASS | pack est. 11.9-40.5 MiB, under cap |
| biome | `39d7595b` | 2 | adapt | RISK (runner time/memory) | 158 MiB / 32,904 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| blake2 | `ed1974ea` | 2 | adapt | PASS | pack est. 7.8-26.5 MiB, under cap |
| blake3 | `f55849f8` | 2 | adapt | PASS | pack est. 2.7-9.2 MiB, under cap |
| bleve | `6abe3730` | 2 | adapt | PASS | pack est. 13.0-44.4 MiB, under cap |
| blst | `19b285a4` | 2 | adapt | PASS | pack est. 5.7-19.4 MiB, under cap |
| bluge | `57414197` | 2 | adapt | PASS | pack est. 2.8-9.7 MiB, under cap |
| bookkeeper | `773a988d` | 2 | adapt | PASS (cap uncertain) | pack est. 35-120 MiB straddles the cap |
| boringtun | `6dcc889a` | 2 | adapt | PASS | pack est. 0.7-2.4 MiB, under cap |
| borsh-rs | `c9a2ed48` | 2 | adapt | PASS | pack est. 1.5-5.1 MiB, under cap |
| botan | `9fc28d5b` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 71-242 MiB: builds, but clients never auto-pull it |
| boulder | `f877538d` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 61-207 MiB: builds, but clients never auto-pull it |
| box-openapi | `38fa64be` | 2 | adapt | PASS | pack est. 5.8-19.9 MiB, under cap |
| brex-prompt-engineering | `50871be1` | 2 | adapt | PASS | pack est. 0.1-0.4 MiB, under cap |
| brotli | `42a2ed43` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 98-335 MiB: builds, but clients never auto-pull it |
| brpc | `7a2c1d5e` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 62-211 MiB: builds, but clients never auto-pull it |
| bson-rust | `81dcdcae` | 2 | adapt | PASS | pack est. 2.0-6.8 MiB, under cap |
| btrblocks | `c950c3c0` | 2 | adapt | PASS | pack est. 2.1-7.3 MiB, under cap |
| buf | `91f127d3` | 2 | adapt | PASS (cap uncertain) | pack est. 20-69 MiB straddles the cap |
| buildpacks-pack | `99894bcb` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 138-471 MiB: builds, but clients never auto-pull it |
| bulletproofs | `be67b6d5` | 2 | adapt | PASS | pack est. 2.3-8.0 MiB, under cap |
| c-ares | `f4156c12` | 2 | adapt | PASS | pack est. 5.6-19.0 MiB, under cap |
| caddy | `ac834b5d` | 2 | adapt | PASS | pack est. 6.6-22.6 MiB, under cap |
| calcite | `8f27e24a` | 2 | adapt | PASS (over 50 MiB cap) | pack est. 54-183 MiB: builds, but clients never auto-pull it |
| caldera | `cb8d6a81` | 2 | adapt | PASS | pack est. 11.7-39.8 MiB, under cap |
| caldera-stockpile | `996ec41c` | 2 | adapt | PASS (cap uncertain) | pack est. 17-57 MiB straddles the cap |
| caliber-ehr-phenotypes | `f4f614fc` | 2 | adapt | PASS | pack est. 5.8-19.7 MiB, under cap |
| camel | `e5e55256` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 225-767 MiB: builds, but clients never auto-pull it |
| candle | `c68b2499` | 3 | adapt | PASS (cap uncertain) | pack est. 23-79 MiB straddles the cap |
| caniuse | `e951c6cd` | 3 | adapt | PASS (cap uncertain) | pack est. 36-123 MiB straddles the cap |
| cap-examples | `9ef3b4d0` | 3 | adapt | PASS | pack est. 9.3-31.8 MiB, under cap |
| capa-rules | `805f9eac` | 3 | adapt | PASS | pack est. 1.4-4.7 MiB, under cap |
| capnproto | `e866bdba` | 3 | adapt | PASS (cap uncertain) | pack est. 16-53 MiB straddles the cap |
| carbon | `c9b1f791` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 73-249 MiB: builds, but clients never auto-pull it |
| cassandra | `ec2c276b` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 212-724 MiB: builds, but clients never auto-pull it |
| catala-law-lang | `5a41577e` | 3 | adapt | PASS | pack est. 9.2-31.2 MiB, under cap |
| cdc-clinical | `3bd1ba62` | 3 | adapt | PASS | pack est. 1.6-5.6 MiB, under cap |
| cdk8s | `ed80049b` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 86-293 MiB: builds, but clients never auto-pull it |
| cel-spec | `40a3c900` | 3 | adapt | PASS | pack est. 2.1-7.3 MiB, under cap |
| cell-ontology | `b7cb31d4` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 332-1135 MiB: builds, but clients never auto-pull it |
| cereal | `22a1b369` | 3 | adapt | PASS | pack est. 3.3-11.3 MiB, under cap |
| cert-manager | `ef9ac738` | 3 | adapt | PASS (cap uncertain) | pack est. 23-78 MiB straddles the cap |
| certmagic | `cfd20205` | 3 | adapt | PASS | pack est. 1.0-3.3 MiB, under cap |
| certstrap | `f392bdae` | 3 | adapt | PASS | pack est. 0.3-1.1 MiB, under cap |
| cfn-nag | `8b5f03da` | 3 | adapt | PASS | pack est. 2.1-7.1 MiB, under cap |
| cfssl | `ab47f0d9` | 3 | adapt | PASS (cap uncertain) | pack est. 49-168 MiB straddles the cap |
| chamber | `5f93f5f3` | 3 | adapt | PASS | pack est. 0.4-1.4 MiB, under cap |
| chaos-mesh | `780cb80c` | 3 | adapt | PASS (cap uncertain) | pack est. 25-86 MiB straddles the cap |
| chaostoolkit-docs | `d113ae82` | 3 | adapt | PASS | pack est. 2.0-6.9 MiB, under cap |
| chatdoctor | `e77bd7eb` | 3 | adapt | PASS (cap uncertain) | pack est. 42-142 MiB straddles the cap |
| chdb | `a2d579a2` | 3 | adapt | PASS (cap uncertain) | pack est. 28-94 MiB straddles the cap |
| checkov | `456c7e53` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 103-353 MiB: builds, but clients never auto-pull it |
| choosealicense-data | `f717b235` | 3 | adapt | PASS | pack est. 1.7-5.7 MiB, under cap |
| chroma | `5f30478c` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 71-242 MiB: builds, but clients never auto-pull it |
| chronicle-detection-rules | `234e9223` | 3 | adapt | PASS | pack est. 3.7-12.8 MiB, under cap |
| cicero-contract-templates | `89d82efc` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 188-640 MiB: builds, but clients never auto-pull it |
| circl | `91db3e78` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 80-274 MiB: builds, but clients never auto-pull it |
| cisa | `edfa3694` | 3 | adapt | PASS | pack est. 7.4-25.1 MiB, under cap |
| clipboard-apis | `6201fd15` | 3 | adapt | PASS | pack est. 0.3-1.2 MiB, under cap |
| cloud-custodian | `a75de191` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 122-417 MiB: builds, but clients never auto-pull it |
| cloudevents-spec | `2ed3806b` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 51-173 MiB: builds, but clients never auto-pull it |
| cloudnative-pg | `f2a7162e` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 92-313 MiB: builds, but clients never auto-pull it |
| cluster-api | `557ad73a` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 53-181 MiB: builds, but clients never auto-pull it |
| cognita | `8bcaae79` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 79-269 MiB: builds, but clients never auto-pull it |
| commonaccord-legal-docs | `c2e864fd` | 3 | adapt | PASS (cap uncertain) | pack est. 35-120 MiB straddles the cap |
| commonpaper-contracts | `3efceb01` | 3 | adapt | PASS | pack est. 0.1-0.2 MiB, under cap |
| complianceascode-content | `f0f3396f` | 3 | adapt | PASS (over 50 MiB cap) | pack est. 101-345 MiB: builds, but clients never auto-pull it |
| compose-spec | `914ec15d` | 4 | adapt | PASS | pack est. 0.7-2.4 MiB, under cap |
| concourse | `6a706fb8` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 58-196 MiB: builds, but clients never auto-pull it |
| congress-legislators | `d5af3d2d` | 4 | adapt | PASS (cap uncertain) | pack est. 19-65 MiB straddles the cap |
| connect-go | `2b80b7c5` | 4 | adapt | PASS | pack est. 2.8-9.6 MiB, under cap |
| connect-protocol | `088d64e1` | 4 | adapt | PASS | pack est. 4.7-15.9 MiB, under cap |
| containerd | `92f321c8` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 86-292 MiB: builds, but clients never auto-pull it |
| controlflow | `a7ffb12f` | 4 | adapt | PASS | pack est. 4.1-14.0 MiB, under cap |
| conventional-commits | `7d293dc5` | 4 | adapt | PASS | pack est. 2.1-7.1 MiB, under cap |
| coredns | `235e7947` | 4 | adapt | PASS | pack est. 6.2-21.2 MiB, under cap |
| coroot | `56d1b0bd` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 110-377 MiB: builds, but clients never auto-pull it |
| counsel-chat | `e202e19c` | 4 | adapt | PASS (cap uncertain) | pack est. 17-59 MiB straddles the cap |
| courts-db | `c22604df` | 4 | adapt | PASS | pack est. 3.0-10.4 MiB, under cap |
| covid-qa | `fb2eb5e2` | 4 | adapt | PASS (cap uncertain) | pack est. 27-93 MiB straddles the cap |
| cozo | `481af058` | 4 | adapt | PASS | pack est. 8.6-29.3 MiB, under cap |
| cri-o | `617265f3` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 156-533 MiB: builds, but clients never auto-pull it |
| crossplane | `b8d0c527` | 4 | adapt | PASS (cap uncertain) | pack est. 34-115 MiB straddles the cap |
| csp | `b1bad631` | 4 | adapt | PASS | pack est. 6.0-20.6 MiB, under cap |
| css-tree | `f8980154` | 4 | adapt | PASS | pack est. 3.1-10.5 MiB, under cap |
| ctakes | `80e0d8e9` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 67-228 MiB: builds, but clients never auto-pull it |
| cue | `71c80626` | 4 | adapt | PASS (cap uncertain) | pack est. 25-84 MiB straddles the cap |
| curl-src | `39ad9b07` | 4 | adapt | PASS (cap uncertain) | pack est. 27-93 MiB straddles the cap; BUILT locally: 97,640,437 B pack (5.3x source), 26,667 defs, 6m15s, node peak RSS 7.4 GiB |
| curve25519-dalek | `97a020d1` | 4 | adapt | PASS | pack est. 4.0-13.6 MiB, under cap |
| cyclonedx-spec | `ad83dae2` | 4 | adapt | PASS | pack est. 11.6-39.5 MiB, under cap |
| dagger | `96a18ac7` | 4 | adapt | RISK (runner time/memory) | 173 MiB / 16,624 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| dailymed | `21a87b3c` | 4 | adapt | RISK (runner time/memory) | 27 MiB / 18,732 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min; BUILT locally: 101,025,250 B pack (3.6x source), 22m12s (18,655 files), node peak RSS 9.4 GiB |
| datafusion | `45de64cf` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 90-308 MiB: builds, but clients never auto-pull it |
| datasketches-cpp | `9a7226e3` | 4 | adapt | PASS | pack est. 4.2-14.2 MiB, under cap |
| deepeval | `016946f6` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 65-223 MiB: builds, but clients never auto-pull it |
| delta-rs | `ad4964d5` | 4 | adapt | PASS (cap uncertain) | pack est. 24-81 MiB straddles the cap |
| density | `58938943` | 4 | adapt | PASS (cap uncertain) | pack est. 15-52 MiB straddles the cap |
| dgraph | `317411f2` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 101-346 MiB: builds, but clients never auto-pull it |
| digitalocean-openapi | `0dd84700` | 4 | adapt | PASS | pack est. 5.9-20.2 MiB, under cap |
| diskann | `177c03be` | 4 | adapt | PASS (cap uncertain) | pack est. 19-66 MiB straddles the cap |
| dispatch | `dd2837e8` | 4 | adapt | PASS (cap uncertain) | pack est. 20-70 MiB straddles the cap |
| distilabel | `313fac85` | 4 | adapt | PASS (cap uncertain) | pack est. 16-53 MiB straddles the cap |
| dnscrypt-proxy | `ade0c1d6` | 4 | adapt | PASS (cap uncertain) | pack est. 23-79 MiB straddles the cap |
| docassemble | `fea293b7` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 113-385 MiB: builds, but clients never auto-pull it |
| docker-docs | `db384a4b` | 4 | adapt | PASS (over 50 MiB cap) | pack est. 75-257 MiB: builds, but clients never auto-pull it |
| dragonboat | `076c7f64` | 4 | adapt | PASS | pack est. 3.8-13.1 MiB, under cap |
| drpc | `92689768` | 4 | adapt | PASS | pack est. 1.2-4.2 MiB, under cap |
| dspy | `099982b3` | 5 | adapt | PASS (cap uncertain) | pack est. 39-133 MiB straddles the cap |
| duckdb | `2866f801` | 5 | adapt | RISK (runner time/memory) | 246 MiB / 16,173 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| dwyl-english-words | `20f5cc9b` | 5 | adapt | PASS (cap uncertain) | pack est. 29-98 MiB straddles the cap |
| ecfr-title-12 | `4dc0246c` | 5 | adapt | PASS (cap uncertain) | pack est. 38-129 MiB straddles the cap |
| ecfr-title-14 | `0520b312` | 5 | adapt | PASS (cap uncertain) | pack est. 17-59 MiB straddles the cap |
| ecfr-title-26 | `3f3e58c7` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 110-374 MiB: builds, but clients never auto-pull it |
| ecfr-title-29 | `bf0c9b74` | 5 | adapt | PASS (cap uncertain) | pack est. 36-122 MiB straddles the cap |
| ecma262 | `a51d83bd` | 5 | adapt | PASS | pack est. 5.3-18.2 MiB, under cap |
| edliz-zimbabwe-treatment-guidelines | `73b89bdc` | 5 | approach-only | PASS (cap uncertain) | pack est. 15-51 MiB straddles the cap |
| ell | `9d129846` | 5 | adapt | PASS (cap uncertain) | pack est. 37-128 MiB straddles the cap |
| emp-tool | `bd036a9e` | 5 | adapt | PASS (cap uncertain) | pack est. 19-65 MiB straddles the cap |
| enet | `5a9c537f` | 5 | adapt | PASS | pack est. 0.6-1.9 MiB, under cap |
| envoy | `eb32f1f9` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 250-854 MiB: builds, but clients never auto-pull it |
| errata-ai-readability-vale | `20b367bc` | 5 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| esbuild | `f6058f83` | 5 | adapt | PASS | pack est. 14.0-47.7 MiB, under cap |
| eset-malware-ioc | `705ead5a` | 5 | adapt | PASS (cap uncertain) | pack est. 20-68 MiB straddles the cap |
| etcd-docs | `d20f8ae3` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 51-173 MiB: builds, but clients never auto-pull it |
| etcd-src | `64f26db4` | 5 | adapt | PASS (cap uncertain) | pack est. 23-77 MiB straddles the cap |
| etherparse | `70f72bee` | 5 | adapt | PASS | pack est. 5.1-17.6 MiB, under cap |
| eurlex-core | `f407c938` | 5 | adapt | PASS | pack est. 2.9-10.0 MiB, under cap; licence review: spdx NONE-FOUND on an adapt-with-attribution corpus |
| exploit-pocs-2023 | `af9e194d` | 5 | approach-only | FAIL (owner decision) | 2526 repos; 539 pins not in their repo, 3 repos 404. `corpus add` aborts on ANY missing repo (INCOMPLETE, exit 1); 548 rows reuse another row's directory name (31 rows in exploit-pocs-2023 alone are named `CVE-2023-23752`), and the recorded sha is the one per NAME, so collided rows carry a foreign sha; 33,752 MiB source would also exceed the 90-min job |
| exploit-pocs-2024 | `7855e58f` | 5 | approach-only | FAIL (owner decision) | 3019 repos; 640 pins not in their repo, 7 repos 404. `corpus add` aborts on ANY missing repo (INCOMPLETE, exit 1); 658 rows reuse another row's directory name (31 rows in exploit-pocs-2023 alone are named `CVE-2023-23752`), and the recorded sha is the one per NAME, so collided rows carry a foreign sha; 7,797 MiB source would also exceed the 90-min job |
| exploit-pocs-2025 | `374cf8f1` | 5 | approach-only | FAIL (owner decision) | 3908 repos; 863 pins not in their repo, 8 repos 404. `corpus add` aborts on ANY missing repo (INCOMPLETE, exit 1); 881 rows reuse another row's directory name (31 rows in exploit-pocs-2023 alone are named `CVE-2023-23752`), and the recorded sha is the one per NAME, so collided rows carry a foreign sha; 10,070 MiB source would also exceed the 90-min job |
| exploit-pocs-2026 | `160f470e` | 5 | approach-only | FAIL (owner decision) | 4612 repos; 4 pins not in their repo, 41 repos 404. `corpus add` aborts on ANY missing repo (INCOMPLETE, exit 1); 4,354 MiB source would also exceed the 90-min job; BUILT locally on a 4-repo subset (2 ok, 1 sha-missing, 1 repo-404): "2 of 4 repos did not land ... INCOMPLETE", `corpus add` exit 1 |
| exploitdb-src | `12879ca7` | 5 | approach-only | RISK (runner time/memory) | 174 MiB / 47,771 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min; gitlab.com source, fetch verified |
| external-secrets | `4aaa3159` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 57-193 MiB: builds, but clients never auto-pull it |
| eyecite | `b70a146d` | 5 | adapt | PASS | pack est. 1.0-3.5 MiB, under cap |
| fabric | `177b3a9d` | 5 | adapt | PASS (cap uncertain) | pack est. 43-146 MiB straddles the cap |
| faiss | `0db2f308` | 5 | adapt | PASS (cap uncertain) | pack est. 15-53 MiB straddles the cap |
| falco-rules | `21ef6d5f` | 5 | adapt | PASS | pack est. 1.1-3.8 MiB, under cap |
| fastchat | `587d5cfa` | 5 | adapt | PASS (cap uncertain) | pack est. 23-80 MiB straddles the cap |
| fastcrypto | `f8b24b80` | 5 | adapt | PASS | pack est. 5.4-18.3 MiB, under cap |
| fastmcp | `e2fcc412` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 91-311 MiB: builds, but clients never auto-pull it |
| fastrag | `ab3d19d5` | 5 | adapt | PASS (cap uncertain) | pack est. 42-143 MiB straddles the cap |
| fda-food-code | `30a28444` | 5 | adapt | PASS | pack est. 9.8-33.5 MiB, under cap |
| fhir-test-cases | `4e3b2480` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 297-1015 MiB: builds, but clients never auto-pull it |
| flagembedding | `fd1a2bdf` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 85-291 MiB: builds, but clients never auto-pull it |
| flatbuffers | `b8431fbc` | 5 | adapt | PASS (cap uncertain) | pack est. 20-69 MiB straddles the cap |
| flexrag | `2b6e32c3` | 5 | adapt | PASS (cap uncertain) | pack est. 27-94 MiB straddles the cap |
| fluent-bit | `3b6ce5cf` | 5 | adapt | PASS (over 50 MiB cap) | pack est. 256-875 MiB: builds, but clients never auto-pull it |
| fluentd | `358c92bb` | 6 | adapt | PASS | pack est. 6.3-21.7 MiB, under cap |
| flux2 | `a0c663ab` | 6 | adapt | PASS | pack est. 4.9-16.6 MiB, under cap |
| fluxcd-website | `8aa27f0f` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 173-590 MiB: builds, but clients never auto-pull it |
| foodon | `cd735402` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 384-1310 MiB: builds, but clients never auto-pull it |
| fortio | `67ce9fa2` | 6 | adapt | PASS | pack est. 1.4-4.9 MiB, under cap |
| fory | `9cc2a6ac` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 58-196 MiB: builds, but clients never auto-pull it |
| freebsd-handbook | `3aa6b19a` | 6 | adapt | RISK (runner time/memory) | 362 MiB / 12,512 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| garak | `cd87893b` | 6 | adapt | PASS (cap uncertain) | pack est. 17-59 MiB straddles the cap |
| gatekeeper-library | `f8e3b4e9` | 6 | adapt | PASS | pack est. 5.7-19.3 MiB, under cap |
| gateway-api | `0da5a10e` | 6 | adapt | PASS (cap uncertain) | pack est. 28-94 MiB straddles the cap |
| gatling | `d552826b` | 6 | adapt | PASS | pack est. 6.8-23.2 MiB, under cap |
| gdpr-commonaccord | `cd623e0d` | 6 | adapt | PASS | pack est. 1.4-4.7 MiB, under cap |
| gemini-cookbook | `dd5b15da` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 221-755 MiB: builds, but clients never auto-pull it |
| getrandom | `eeb6a3d4` | 6 | adapt | PASS | pack est. 0.3-1.1 MiB, under cap |
| ghsa-db | `0af427f6` | 6 | adapt | FIXED (repinned in this PR), then RISK | old pin 0af427f6 is not in github/advisory-database ("not our ref"); repinned to 4b4ac300 (the commit the review note names; exists, 2026-10-03). The url is the RAW upstream, not the 36,263-record flatten the pin described: tree API truncates past 57,186 entries, an earlier raw attempt hit 11.3 GB; BUILT locally at the old pin: `git fetch 0af427f6... failed`, `corpus add` exit 1 |
| giskard | `48814003` | 6 | adapt | PASS (cap uncertain) | pack est. 16-55 MiB straddles the cap |
| github-api-description | `7bdf5f00` | 6 | adapt | RISK (runner time/memory) | 7,502 MiB / 419 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| github-opensource-guide | `e7ac98e9` | 6 | adapt | PASS (cap uncertain) | pack est. 22-75 MiB straddles the cap |
| github-site-policy | `b9578b54` | 6 | adapt | PASS | pack est. 1.0-3.4 MiB, under cap |
| gitlab-runbooks | `caae4426` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 241-822 MiB: builds, but clients never auto-pull it; gitlab.com source, fetch verified; BUILT locally (gitlab.com source): 252,843,118 B pack (1.55x source), 15m41s, node peak RSS 10.8 GiB |
| glasskube | `b0d877db` | 6 | adapt | PASS | pack est. 2.6-8.9 MiB, under cap |
| gnark | `3ac4de3e` | 6 | adapt | PASS (cap uncertain) | pack est. 27-93 MiB straddles the cap |
| gnostic | `baf14b96` | 6 | adapt | PASS | pack est. 4.0-13.7 MiB, under cap |
| go-jose | `13b7118f` | 6 | adapt | PASS | pack est. 1.3-4.6 MiB, under cap |
| go-stomp | `9eb558af` | 6 | adapt | PASS | pack est. 0.3-1.1 MiB, under cap |
| go-tpm | `e05fb8b1` | 6 | adapt | PASS | pack est. 2.4-8.1 MiB, under cap |
| go-vulndb | `5cd8418c` | 6 | adapt | PASS | pack est. 6.3-21.4 MiB, under cap |
| golang-jwt | `73c870b1` | 6 | adapt | PASS | pack est. 0.4-1.2 MiB, under cap |
| golang-x-crypto | `3b414e3e` | 6 | adapt | PASS | pack est. 8.0-27.5 MiB, under cap |
| good-docs-templates | `de840b37` | 6 | adapt | PASS (cap uncertain) | pack est. 35-120 MiB straddles the cap |
| google-aip | `615875bf` | 6 | adapt | PASS | pack est. 1.2-4.2 MiB, under cap |
| google-devdocs-vale | `9e2483e2` | 6 | adapt | PASS | pack est. 0.1-0.3 MiB, under cap |
| google-eng-practices | `3bb3ec25` | 6 | adapt | PASS | pack est. 0.1-0.5 MiB, under cap |
| google-fuzzing | `734e55f3` | 6 | adapt | PASS | pack est. 2.5-8.5 MiB, under cap |
| google-licenseclassifier | `3cfbab2d` | 6 | adapt | PASS | pack est. 6.7-22.9 MiB, under cap |
| googleapis | `dfaf98d4` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 119-406 MiB: builds, but clients never auto-pull it |
| gopass | `1e831b4c` | 6 | adapt | PASS | pack est. 7.6-26.0 MiB, under cap |
| goreleaser | `e6c4afca` | 6 | adapt | PASS | pack est. 10.4-35.5 MiB, under cap |
| gorilla | `6ea57973` | 6 | adapt | PASS (over 50 MiB cap) | pack est. 277-946 MiB: builds, but clients never auto-pull it |
| gosec-rules | `71577268` | 6 | adapt | PASS | pack est. 2.9-9.8 MiB, under cap |
| goss | `f1a60436` | 7 | adapt | PASS | pack est. 1.2-4.0 MiB, under cap |
| got | `50c4ac61` | 7 | adapt | PASS | pack est. 3.8-13.0 MiB, under cap |
| govuk-design-system | `9bd52fc9` | 7 | adapt | PASS | pack est. 0.6-2.0 MiB, under cap |
| gpt-engineer | `a90fcd54` | 7 | adapt | PASS (cap uncertain) | pack est. 25-85 MiB straddles the cap |
| grafana-alloy | `e5a50dfc` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 59-203 MiB: builds, but clients never auto-pull it |
| graphql-composite-schemas-spec | `e5ce4835` | 7 | adapt | PASS | pack est. 0.6-2.0 MiB, under cap |
| graphql-over-http | `d438b209` | 7 | adapt | PASS | pack est. 0.9-3.1 MiB, under cap |
| graphql-spec | `3f1ef846` | 7 | adapt | PASS | pack est. 7.8-26.6 MiB, under cap |
| graphrag | `5faaaf4f` | 7 | adapt | PASS (cap uncertain) | pack est. 26-90 MiB straddles the cap |
| griptape | `aa95c599` | 7 | adapt | PASS (cap uncertain) | pack est. 31-107 MiB straddles the cap |
| grpc-gateway | `14c1da7c` | 7 | adapt | PASS | pack est. 14.5-49.6 MiB, under cap |
| grpc-go | `de20bed5` | 7 | adapt | PASS (cap uncertain) | pack est. 22-75 MiB straddles the cap |
| grpc-proposals | `6342be72` | 7 | adapt | PASS (cap uncertain) | pack est. 16-53 MiB straddles the cap |
| guardrails | `06d0ff2c` | 7 | adapt | PASS (cap uncertain) | pack est. 23-79 MiB straddles the cap |
| guidance | `21b1d90d` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 57-193 MiB: builds, but clients never auto-pull it |
| h2 | `e64c1662` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 91-309 MiB: builds, but clients never auto-pull it |
| h2o | `d21e7cb5` | 7 | adapt | PASS (cap uncertain) | pack est. 47-160 MiB straddles the cap |
| hacl-star | `504c2987` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 141-481 MiB: builds, but clients never auto-pull it |
| harbor | `23bfa9a1` | 7 | adapt | PASS (cap uncertain) | pack est. 29-99 MiB straddles the cap |
| harper | `6a0f42d1` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 74-251 MiB: builds, but clients never auto-pull it |
| haystack | `441023dc` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 157-536 MiB: builds, but clients never auto-pull it |
| head-qa | `da54ff23` | 7 | adapt | PASS | pack est. 0.1-0.4 MiB, under cap |
| helm | `dc56a44f` | 7 | adapt | PASS | pack est. 6.2-21.1 MiB, under cap |
| hivemq-mqtt-client | `99ab2013` | 7 | adapt | PASS | pack est. 5.2-17.9 MiB, under cap |
| how-they-sre | `284703b1` | 7 | adapt | PASS | pack est. 0.5-1.6 MiB, under cap |
| httparse | `a0fa552e` | 7 | adapt | PASS | pack est. 0.4-1.4 MiB, under cap |
| httpx | `b5addb64` | 7 | adapt | PASS | pack est. 4.9-16.8 MiB, under cap |
| huggingface-hub-docs | `fa1174ae` | 7 | adapt | PASS (cap uncertain) | pack est. 32-108 MiB straddles the cap |
| hyper | `2b454af2` | 7 | adapt | PASS | pack est. 2.2-7.5 MiB, under cap |
| ibm-openapi-validator | `cf6ec487` | 7 | adapt | PASS | pack est. 4.5-15.2 MiB, under cap |
| icd10-who | `bff0a1dc` | 7 | adapt | PASS | pack est. 10.6-36.3 MiB, under cap |
| icd10cm-codes | `98ad49ef` | 7 | adapt | PASS | pack est. 1.1-3.8 MiB, under cap |
| icd9-icd10-hierarchy | `1b03a8c0` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 295-1005 MiB: builds, but clients never auto-pull it |
| iceberg | `8102f4cb` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 92-315 MiB: builds, but clients never auto-pull it |
| ietf-rfc-abstracts | `36298d4d` |  | adapt | DELIST (withdrawn in this PR) | pinned repository returns HTTP 404 (GraphQL NOT_FOUND, REST 404 with the owner token, `git fetch`: repository not found) |
| in-solidarity-bot | `9f069c62` | 7 | adapt | PASS | pack est. 1.2-4.2 MiB, under cap |
| influxdb | `06200ef9` | 7 | adapt | PASS (cap uncertain) | pack est. 40-138 MiB straddles the cap |
| infracost | `ccc476fb` | 7 | adapt | PASS (cap uncertain) | pack est. 24-83 MiB straddles the cap |
| inspect-ai | `81c1668d` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 80-272 MiB: builds, but clients never auto-pull it |
| instructor | `035c77b7` | 7 | adapt | PASS (over 50 MiB cap) | pack est. 114-390 MiB: builds, but clients never auto-pull it |
| ipsum-blocklist | `444b4802` | 7 | adapt | PASS | pack est. 7.0-24.0 MiB, under cap |
| jaeger-docs | `231b67c0` | 8 | adapt | PASS (over 50 MiB cap) | pack est. 51-174 MiB: builds, but clients never auto-pull it |
| joblint | `3b3fbb4e` | 8 | adapt | PASS | pack est. 0.6-2.1 MiB, under cap |
| jose-js | `2a3a5259` | 8 | adapt | PASS | pack est. 2.5-8.4 MiB, under cap |
| jsdom | `a78f9c22` | 8 | adapt | PASS (over 50 MiB cap) | pack est. 70-238 MiB: builds, but clients never auto-pull it |
| json-api-spec | `bde79ceb` | 8 | adapt | PASS | pack est. 3.0-10.4 MiB, under cap |
| json-schema-spec | `4f56a990` | 8 | adapt | PASS | pack est. 1.1-3.8 MiB, under cap |
| json-schema-test-suite | `8568bcc4` | 8 | adapt | PASS | pack est. 5.3-18.2 MiB, under cap |
| jsonformer | `bfad0318` | 8 | adapt | PASS | pack est. 0.5-1.6 MiB, under cap |
| jsonwebtoken-rust | `11f83ab5` | 8 | adapt | PASS | pack est. 0.4-1.4 MiB, under cap |
| jwx | `c4774435` | 8 | adapt | PASS | pack est. 5.1-17.3 MiB, under cap |
| kafka-protocol | `94ac9e02` | 8 | adapt | PASS | pack est. 4.6-15.7 MiB, under cap |
| kafka-src | `d12941eb` | 8 | adapt | PASS (over 50 MiB cap) | pack est. 129-440 MiB: builds, but clients never auto-pull it |
| karma | `38fd3a9f` | 8 | adapt | PASS (cap uncertain) | pack est. 38-128 MiB straddles the cap |
| karpenter | `a4432485` | 8 | adapt | PASS (cap uncertain) | pack est. 27-93 MiB straddles the cap |
| keda | `59815e11` | 8 | adapt | PASS | pack est. 13.9-47.5 MiB, under cap |
| keep-a-changelog | `08d0df5a` | 8 | adapt | PASS | pack est. 8.1-27.5 MiB, under cap |
| keywhiz | `52e77fd9` | 8 | adapt | PASS | pack est. 3.7-12.6 MiB, under cap |
| kics | `bbb3c148` | 8 | adapt | RISK (runner time/memory) | 123 MiB / 17,628 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| knox | `61dd5823` | 8 | adapt | PASS | pack est. 0.7-2.3 MiB, under cap |
| kotaemon | `9ad3e4e4` | 8 | adapt | PASS (cap uncertain) | pack est. 28-95 MiB straddles the cap |
| kratix | `2ed71d24` | 8 | adapt | PASS | pack est. 8.1-27.5 MiB, under cap |
| kryptology | `eef70332` | 8 | adapt | PASS | pack est. 10.8-36.9 MiB, under cap |
| kube-bench | `e278a591` | 8 | adapt | PASS | pack est. 7.2-24.6 MiB, under cap |
| kube-hunter | `bc47f08e` | 8 | adapt | PASS | pack est. 1.1-3.8 MiB, under cap |
| kubernetes-community | `585a4ac6` | 8 | adapt | PASS (over 50 MiB cap) | pack est. 91-311 MiB: builds, but clients never auto-pull it |
| kubescape-regolibrary | `5d5fd8fc` | 8 | adapt | PASS | pack est. 9.7-33.0 MiB, under cap |
| kyverno-policies | `ec478cb5` | 8 | adapt | PASS | pack est. 8.1-27.7 MiB, under cap |
| lambdaworks | `3a87850d` | 8 | adapt | PASS | pack est. 10.4-35.6 MiB, under cap |
| ldnoobw-bad-words | `5faf2ba4` | 8 | adapt | PASS | pack est. 0.1-0.2 MiB, under cap |
| lego | `748e608a` | 8 | adapt | PASS | pack est. 9.9-33.7 MiB, under cap |
| leveldb | `7ee830d0` | 8 | adapt | PASS | pack est. 1.5-5.0 MiB, under cap |
| lexbuild | `8cc0b93b` | 8 | adapt | PASS | pack est. 5.3-18.0 MiB, under cap |
| libcbor | `ab8c72b1` | 8 | adapt | PASS | pack est. 1.7-6.0 MiB, under cap |
| libhydrogen | `264bf429` | 8 | adapt | PASS | pack est. 0.3-0.9 MiB, under cap |
| libopenapi | `c4c4be74` | 8 | adapt | PASS (cap uncertain) | pack est. 41-139 MiB straddles the cap |
| libsodium | `61478971` | 8 | adapt | PASS | pack est. 10.8-36.8 MiB, under cap |
| libuv | `49b1c064` | 8 | adapt | PASS | pack est. 7.0-23.9 MiB, under cap |
| licensee | `9261af40` | 8 | adapt | PASS | pack est. 3.5-11.8 MiB, under cap |
| lightningcss | `ab14f90d` | 8 | adapt | PASS (cap uncertain) | pack est. 17-56 MiB straddles the cap |
| lit | `01dbc667` | 8 | adapt | PASS (cap uncertain) | pack est. 20-68 MiB straddles the cap |
| llhttp | `69936dbc` | 9 | adapt | PASS | pack est. 12.1-41.4 MiB, under cap |
| llm-course | `7abd96e8` | 9 | adapt | PASS | pack est. 1.3-4.4 MiB, under cap |
| lmdb | `700e10f9` | 9 | adapt | PASS | pack est. 1.0-3.5 MiB, under cap |
| lol-html | `608cc4a6` | 9 | adapt | PASS | pack est. 8.8-30.2 MiB, under cap |
| loldrivers | `6f6f5b49` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 178-606 MiB: builds, but clients never auto-pull it |
| markdownlint | `3f1f4793` | 9 | adapt | PASS | pack est. 5.8-19.7 MiB, under cap |
| mashqa | `86e0a7d7` | 9 | adapt | PASS | pack est. 1.0-3.4 MiB, under cap |
| material-components-web | `6ceadb81` | 9 | adapt | PASS (cap uncertain) | pack est. 17-57 MiB straddles the cap |
| material-web | `e3471c04` | 9 | adapt | PASS (cap uncertain) | pack est. 27-93 MiB straddles the cap |
| mbedtls | `de23569a` | 9 | adapt | PASS | pack est. 12.3-41.8 MiB, under cap |
| mdn-browser-compat-data | `85c50b0b` | 9 | adapt | PASS (cap uncertain) | pack est. 44-150 MiB straddles the cap |
| mdn-data | `2c16ea05` | 9 | adapt | PASS | pack est. 1.3-4.5 MiB, under cap |
| medical-abbreviations | `3970d38b` | 9 | adapt | PASS | pack est. 0.3-0.9 MiB, under cap |
| medmcqa | `c59ef14c` | 9 | adapt | PASS | pack est. 1.1-3.9 MiB, under cap |
| medqa-usmle | `27b02f66` | 9 | adapt | PASS | pack est. 0.2-0.5 MiB, under cap |
| medquad | `577bd37b` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 54-185 MiB: builds, but clients never auto-pull it |
| medspacy | `b7bb7d82` | 9 | adapt | PASS | pack est. 8.3-28.2 MiB, under cap |
| microsoft-seal | `31fc3194` | 9 | adapt | PASS | pack est. 6.9-23.5 MiB, under cap |
| microsoft-style-vale | `ea5fa73c` | 9 | adapt | PASS | pack est. 0.2-0.6 MiB, under cap |
| miekg-dns | `2e4905fb` | 9 | adapt | PASS | pack est. 1.4-4.7 MiB, under cap |
| milvus | `b48661d3` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 197-671 MiB: builds, but clients never auto-pull it |
| minify | `eb78925d` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 59-201 MiB: builds, but clients never auto-pull it |
| minisign | `b6de4fe3` | 9 | adapt | PASS | pack est. 0.1-0.5 MiB, under cap |
| misp-galaxy | `b9675983` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 89-305 MiB: builds, but clients never auto-pull it |
| mitre-attck | `ec93deaf` | 9 | adapt | PASS | pack est. 2.8-9.4 MiB, under cap |
| mitre-capec | `c59d35cd` | 9 | adapt | PASS | pack est. 1.2-4.3 MiB, under cap |
| mitre-cwe | `364b6e86` | 9 | adapt | PASS | pack est. 3.2-11.0 MiB, under cap |
| mkcert | `1c1dc4ed` | 9 | adapt | PASS | pack est. 0.1-0.3 MiB, under cap |
| mls-rs | `d4c5b19f` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 96-326 MiB: builds, but clients never auto-pull it |
| monocypher | `e65d2772` | 9 | adapt | PASS | pack est. 2.1-7.1 MiB, under cap |
| mozilla-diversity | `5e612f24` | 9 | adapt | PASS | pack est. 0.5-1.6 MiB, under cap |
| mozilla-l10n-docs | `b190a660` | 9 | adapt | PASS | pack est. 1.5-5.3 MiB, under cap |
| mozilla-l10n-styleguides | `50ad513b` | 9 | adapt | PASS | pack est. 4.2-14.2 MiB, under cap |
| mozilla-ssl-configs | `34d8441d` | 9 | adapt | PASS | pack est. 0.1-0.2 MiB, under cap |
| moztw-styleguides | `37d9832a` | 9 | adapt | PASS | pack est. 4.1-13.9 MiB, under cap |
| msft-api-guidelines | `a7022a29` | 9 | adapt | PASS | pack est. 0.9-3.2 MiB, under cap |
| msgpack-c | `1cdc1b66` | 9 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| mtl-bioinformatics-ner | `df0df02f` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 329-1123 MiB: builds, but clients never auto-pull it |
| mts-dialog-clinical-notes | `3ff08019` | 9 | adapt | PASS | pack est. 12.2-41.7 MiB, under cap |
| mutcd | `b70b9188` | 9 | adapt | PASS (over 50 MiB cap) | pack est. 52-176 MiB: builds, but clients never auto-pull it |
| nats-protocol | `1cb267ca` | 10 | adapt | PASS | pack est. 1.5-5.0 MiB, under cap |
| nats-server | `a44c356c` | 10 | adapt | PASS (cap uncertain) | pack est. 22-74 MiB straddles the cap |
| navrules | `4d535668` | 10 | adapt | PASS | pack est. 0.4-1.4 MiB, under cap |
| nghttp2 | `19d06e62` | 10 | adapt | PASS | pack est. 8.6-29.2 MiB, under cap |
| nginx | `2b5c2b60` | 10 | adapt | PASS | pack est. 12.2-41.6 MiB, under cap |
| nigeria-standard-treatment-guidelines | `d2be90e9` | 10 | adapt | PASS | pack est. 2.6-8.9 MiB, under cap |
| nuclei-templates | `24a43e14` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 68-232 MiB: builds, but clients never auto-pull it |
| oasdiff | `b687d634` | 10 | adapt | PASS | pack est. 9.1-31.0 MiB, under cap |
| openapi-moonwalk | `62520aa3` | 10 | adapt | PASS | pack est. 0.3-0.9 MiB, under cap |
| openapi-overlay-spec | `d4cc4c80` | 10 | adapt | PASS | pack est. 0.5-1.6 MiB, under cap |
| openapi-spec | `447c479c` | 10 | adapt | PASS | pack est. 3.6-12.5 MiB, under cap |
| openbao | `a4f7e569` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 112-383 MiB: builds, but clients never auto-pull it |
| openbsd-faq | `cab1d910` | 10 | adapt | RISK (runner time/memory) | 761 MiB / 13,328 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| openfda | `fdbe5432` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 243-829 MiB: builds, but clients never auto-pull it |
| openlegaldata-oldp | `b1c2de58` | 10 | adapt | PASS (cap uncertain) | pack est. 43-148 MiB straddles the cap |
| opennyai-legal-nlp | `2abee8fc` | 10 | adapt | PASS | pack est. 5.8-19.7 MiB, under cap |
| opensanctions | `d1619f4c` | 10 | adapt | PASS (cap uncertain) | pack est. 44-152 MiB straddles the cap |
| openssl | `7ef69a72` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 205-700 MiB: builds, but clients never auto-pull it |
| osquery-config-examples | `89c2ae5c` | 10 | adapt | PASS | pack est. 0.3-1.1 MiB, under cap |
| oss-attribution-builder | `c8bc98e1` | 10 | adapt | PASS | pack est. 2.0-6.7 MiB, under cap |
| oss-license-texts | `f717b235` | 10 | adapt | PASS | pack est. 1.7-5.7 MiB, under cap |
| oss-review-toolkit | `b0e622b0` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 122-416 MiB: builds, but clients never auto-pull it |
| otel-proto | `b3f75588` | 10 | adapt | PASS | pack est. 7.3-24.9 MiB, under cap |
| owasp-cheatsheets | `ca1bded7` | 10 | adapt | PASS (cap uncertain) | pack est. 36-122 MiB straddles the cap |
| owasp-wstg | `8df290a3` | 10 | adapt | PASS (cap uncertain) | pack est. 23-77 MiB straddles the cap |
| oxc | `da692b86` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 116-397 MiB: builds, but clients never auto-pull it |
| pagerduty-incident-response | `464fc9d3` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 264-902 MiB: builds, but clients never auto-pull it |
| parquet-format | `bf099392` | 10 | adapt | PASS | pack est. 1.4-4.6 MiB, under cap |
| parse5 | `103d9aa6` | 10 | adapt | PASS | pack est. 5.5-18.6 MiB, under cap |
| payloadsallthethings | `3ac27901` | 10 | adapt | PASS (cap uncertain) | pack est. 19-65 MiB straddles the cap |
| pebble | `e0818dd0` | 10 | adapt | PASS (cap uncertain) | pack est. 31-106 MiB straddles the cap |
| plain-language-guidelines | `fd769474` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 100-343 MiB: builds, but clients never auto-pull it |
| polars | `099c2863` | 10 | adapt | PASS (cap uncertain) | pack est. 46-159 MiB straddles the cap |
| postcss | `f1aca7ec` | 10 | adapt | PASS | pack est. 1.1-3.8 MiB, under cap |
| postgres-src | `50d6e533` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 217-740 MiB: builds, but clients never auto-pull it |
| postman-collection | `8801e7f4` | 10 | adapt | PASS | pack est. 2.4-8.3 MiB, under cap |
| preact | `8ff8785b` | 10 | adapt | PASS | pack est. 2.5-8.7 MiB, under cap |
| prism | `2a1b2052` | 10 | adapt | PASS | pack est. 4.1-14.1 MiB, under cap |
| prometheus-src | `7c741e28` | 10 | adapt | PASS (cap uncertain) | pack est. 44-150 MiB straddles the cap |
| prompt-engineering-guide | `57673726` | 10 | adapt | PASS (over 50 MiB cap) | pack est. 118-403 MiB: builds, but clients never auto-pull it |
| promptify | `bc5ed081` | 11 | adapt | PASS | pack est. 3.3-11.3 MiB, under cap |
| proselint | `dbed789c` | 11 | adapt | PASS | pack est. 4.2-14.5 MiB, under cap |
| protobuf | `fb0a6f67` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 153-524 MiB: builds, but clients never auto-pull it |
| protovalidate | `c550962b` | 11 | adapt | PASS | pack est. 5.6-19.1 MiB, under cap |
| pubhealth-fact-checking | `02136d3e` | 11 | adapt | PASS | pack est. 0.3-1.0 MiB, under cap |
| pubmedqa | `1cbae8e9` | 11 | adapt | PASS | pack est. 3.8-13.1 MiB, under cap |
| pysec | `3373aa62` | 11 | adapt | PASS (cap uncertain) | pack est. 21-71 MiB straddles the cap |
| quiche | `b60aff93` | 11 | adapt | PASS (cap uncertain) | pack est. 15-50 MiB straddles the cap |
| quinn | `7f1c6b55` | 11 | adapt | PASS | pack est. 3.7-12.5 MiB, under cap |
| qwik | `09c73b97` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 56-191 MiB: builds, but clients never auto-pull it |
| rabbitmq | `f23f19da` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 94-319 MiB: builds, but clients never auto-pull it |
| raft-rs | `ad13f3d9` | 11 | adapt | PASS | pack est. 1.4-4.8 MiB, under cap |
| react-spectrum | `9b7a06ee` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 291-993 MiB: builds, but clients never auto-pull it |
| redhat-vale-style | `7cfb4abc` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 62-210 MiB: builds, but clients never auto-pull it |
| redoc | `2e30cabf` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 51-175 MiB: builds, but clients never auto-pull it |
| redocly-cli | `7df8cf3c` | 11 | adapt | PASS (cap uncertain) | pack est. 37-127 MiB straddles the cap |
| redos-precedent | `583ffc32` |  | adapt | DELIST (withdrawn in this PR) | pinned repository returns HTTP 404 (GraphQL NOT_FOUND, REST 404 with the owner token, `git fetch`: repository not found); BUILT locally at the old pin: `git fetch ... failed`, `corpus add` exit 1 |
| reporters-db | `7a5b9b06` | 11 | adapt | PASS | pack est. 2.7-9.1 MiB, under cap |
| retext-equality | `192deddf` | 11 | adapt | PASS | pack est. 0.3-0.9 MiB, under cap |
| retext-indefinite-article | `960875ee` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-intensify | `9450bee5` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-passive | `66ca2c56` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-profanities | `2627fb08` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-quotes | `e43bb5f3` | 11 | adapt | PASS | pack est. 0.0-0.2 MiB, under cap |
| retext-readability | `ece96241` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-redundant-acronyms | `e670637a` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-repeated-words | `d6d53fb6` | 11 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| retext-simplify | `c7686ac9` | 11 | adapt | PASS | pack est. 0.0-0.2 MiB, under cap |
| rocksdb | `4cd0450f` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 75-257 MiB: builds, but clients never auto-pull it |
| rolldown | `2d44fc3f` | 11 | adapt | RISK (runner time/memory) | 26 MiB / 15,191 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| rousseau | `e7fb314d` | 11 | adapt | PASS | pack est. 0.2-0.8 MiB, under cap |
| rspack | `7d83e469` | 11 | adapt | RISK (runner time/memory) | 67 MiB / 24,309 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| rust-api-guidelines | `97a0969c` | 11 | adapt | PASS | pack est. 0.2-0.6 MiB, under cap |
| rust-cssparser | `9f3ceddf` | 11 | adapt | PASS | pack est. 2.9-10.0 MiB, under cap |
| rust-url | `00a6ce58` | 11 | adapt | PASS | pack est. 3.7-12.8 MiB, under cap |
| rustcrypto-hashes | `138dc5a0` | 11 | adapt | PASS | pack est. 6.1-20.7 MiB, under cap |
| rustls | `7dcbe4c1` | 11 | adapt | PASS (cap uncertain) | pack est. 19-66 MiB straddles the cap |
| rustsec-advisories | `4bd96f2e` |  | adapt | DELIST (withdrawn in this PR) | pinned repository returns HTTP 404 (GraphQL NOT_FOUND, REST 404 with the owner token, `git fetch`: repository not found) |
| s2n-tls | `b1700000` | 11 | adapt | PASS (cap uncertain) | pack est. 33-113 MiB straddles the cap |
| sass-spec | `57c81dd7` | 11 | adapt | PASS | pack est. 2.7-9.2 MiB, under cap |
| scap-security-guide | `f0f3396f` | 11 | adapt | PASS (over 50 MiB cap) | pack est. 101-345 MiB: builds, but clients never auto-pull it |
| school-of-sre | `22979a6e` | 11 | adapt | PASS (cap uncertain) | pack est. 32-110 MiB straddles the cap |
| scispacy | `eacccd4e` | 12 | adapt | PASS | pack est. 0.9-3.0 MiB, under cap |
| season-of-docs | `ec6161e9` | 12 | adapt | PASS | pack est. 1.2-4.2 MiB, under cap |
| serverless-workflow-spec | `13b6a625` | 12 | adapt | PASS (cap uncertain) | pack est. 23-78 MiB straddles the cap |
| simonw-llm | `05d7ae7d` | 12 | adapt | PASS | pack est. 2.9-9.9 MiB, under cap |
| siren-hypermedia | `c29a8784` | 12 | adapt | PASS | pack est. 0.0-0.1 MiB, under cap |
| smallstep-certificates | `69d3c36f` | 12 | adapt | PASS | pack est. 7.2-24.6 MiB, under cap |
| smithy | `2a61b43f` | 12 | adapt | PASS (cap uncertain) | pack est. 33-111 MiB straddles the cap |
| solid | `25138e1c` | 12 | adapt | PASS | pack est. 2.1-7.1 MiB, under cap |
| sourcegraph-handbook | `5752928b` | 12 | adapt | PASS | pack est. 14.1-48.2 MiB, under cap |
| spdx-spec | `395d06e1` | 12 | adapt | PASS (cap uncertain) | pack est. 27-93 MiB straddles the cap |
| spectral | `62527eec` | 12 | adapt | PASS | pack est. 11.0-37.5 MiB, under cap |
| spectrum-web-components | `be922808` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 66-224 MiB: builds, but clients never auto-pull it |
| splunk-security-content | `c653da7c` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 59-203 MiB: builds, but clients never auto-pull it |
| sqlite-src | `9696acb0` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 167-570 MiB: builds, but clients never auto-pull it |
| standard-readme | `5d18ad4d` | 12 | adapt | PASS | pack est. 0.1-0.4 MiB, under cap |
| standard-webhooks | `7537d2a2` | 12 | adapt | PASS | pack est. 0.5-1.9 MiB, under cap |
| stratus-red-team | `3ff51c44` | 12 | adapt | PASS | pack est. 6.1-20.8 MiB, under cap |
| stripe-openapi | `e73367db` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 291-994 MiB: builds, but clients never auto-pull it |
| style-dictionary | `81ecc052` | 12 | adapt | PASS | pack est. 11.1-38.0 MiB, under cap |
| svelte | `6480a973` | 12 | adapt | PASS | pack est. 10.6-36.3 MiB, under cap |
| swagger-ui | `e89a91f1` | 12 | adapt | PASS (cap uncertain) | pack est. 27-92 MiB straddles the cap |
| symptom-ontology | `7a295d98` | 12 | adapt | PASS | pack est. 4.8-16.4 MiB, under cap |
| synthea | `d9d07a6e` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 189-645 MiB: builds, but clients never auto-pull it |
| textlint | `2bc95477` | 12 | adapt | PASS (cap uncertain) | pack est. 17-58 MiB straddles the cap |
| textlint-ja-technical-writing | `5e73caaf` | 12 | adapt | PASS | pack est. 0.8-2.8 MiB, under cap |
| textlint-rule-preset-ai-writing | `45bb6485` | 12 | adapt | PASS | pack est. 1.4-4.6 MiB, under cap |
| textlint-rule-preset-jtf-style | `2186273a` | 12 | adapt | PASS | pack est. 1.7-5.9 MiB, under cap |
| textlint-rule-stop-words | `674119e6` | 12 | adapt | PASS | pack est. 0.7-2.4 MiB, under cap |
| textlint-rule-terminology | `b866c04b` | 12 | adapt | PASS | pack est. 0.3-1.2 MiB, under cap |
| textstat | `30303f95` | 12 | adapt | PASS | pack est. 0.6-2.1 MiB, under cap |
| thrift | `a81db74f` | 12 | adapt | PASS (cap uncertain) | pack est. 31-104 MiB straddles the cap |
| tikv | `5f7037a4` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 62-213 MiB: builds, but clients never auto-pull it |
| tldr-pages | `87441be0` | 12 | adapt | PASS | pack est. 4.3-14.5 MiB, under cap |
| trivy-checks | `ac856458` | 12 | adapt | PASS | pack est. 4.1-14.0 MiB, under cap |
| twelve-factor | `1385d2c8` | 12 | adapt | PASS (cap uncertain) | pack est. 34-115 MiB straddles the cap |
| uk-building-regs | `d4669402` | 12 | adapt | PASS | pack est. 0.8-2.7 MiB, under cap; licence review: spdx NONE-FOUND on an adapt-with-attribution corpus |
| uk-legislation | `0575db1c` | 12 | adapt | RISK (runner time/memory) | 63 MiB / 21,292 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| unicode-cldr | `cafab85f` | 12 | adapt | PASS | pack est. 6.7-22.9 MiB, under cap |
| unitedstates-citation | `953c03d3` | 12 | adapt | PASS | pack est. 3.8-12.9 MiB, under cap |
| uslm-us-legislation | `4ec9db2f` | 12 | adapt | PASS (over 50 MiB cap) | pack est. 257-879 MiB: builds, but clients never auto-pull it |
| valkey-docs | `4852e820` | 13 | approach-only | PASS | pack est. 4.6-15.8 MiB, under cap |
| valkey-src | `f4cbd1c5` | 13 | adapt | PASS (cap uncertain) | pack est. 35-118 MiB straddles the cap |
| vite | `dce9fe69` | 13 | adapt | PASS (cap uncertain) | pack est. 26-89 MiB straddles the cap |
| vue | `ff350142` | 13 | adapt | PASS | pack est. 9.6-32.7 MiB, under cap |
| vuln-fix-commits | `3f7ce8ce` | 13 | adapt | RISK (runner time/memory) | 384 MiB / 29,272 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| w3c-css | `d06dc03e` | 13 | adapt | PASS (over 50 MiB cap) | pack est. 219-749 MiB: builds, but clients never auto-pull it |
| weaviate | `a920eab8` | 13 | approach-only | PASS (over 50 MiB cap) | pack est. 84-287 MiB: builds, but clients never auto-pull it |
| web-features | `de89ce66` | 13 | adapt | PASS | pack est. 6.2-21.1 MiB, under cap |
| webgl | `714857a2` | 13 | adapt | RISK (runner time/memory) | 84 MiB / 16,008 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| whatwg-dom | `3071e5f2` | 13 | adapt | PASS | pack est. 11.4-38.7 MiB, under cap |
| whatwg-fetch | `e9460d1b` | 13 | adapt | PASS | pack est. 9.5-32.3 MiB, under cap |
| whatwg-html | `86190a4d` | 13 | adapt | PASS (over 50 MiB cap) | pack est. 182-620 MiB: builds, but clients never auto-pull it |
| whatwg-specs | `4d844936` | 13 | adapt | PASS | pack est. 12.1-41.4 MiB, under cap |
| whatwg-streams | `b9ba9f49` | 13 | adapt | PASS | pack est. 11.7-39.9 MiB, under cap |
| whatwg-url | `fde3f74f` | 13 | adapt | PASS | pack est. 3.9-13.2 MiB, under cap |
| whatwg-webidl | `0620cf17` | 13 | adapt | PASS | pack est. 11.1-37.9 MiB, under cap |
| woke-inclusive-lint | `5d52c154` | 13 | adapt | PASS | pack est. 0.5-1.5 MiB, under cap |
| write-good | `6940b034` | 13 | adapt | PASS | pack est. 0.3-0.9 MiB, under cap |
| xerj-blogposts | `a9ad4fc0` | 13 | adapt | FAIL (owner decision) | `kind: harvested` recipe manifest; `corpus add --from` refuses it. Building it as a git manifest would index the whole xerj repo (skill rule 3). Needs the recipe path or lane B; BUILT locally: `corpus add: .../hub: cannot read manifest.json`, exit 2 after 0.01 s |
| xerj-columnar | `31081d9f` | 13 | adapt | RISK (runner time/memory) | 1,140 MiB / 72,754 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| xerj-search | `3a55bc98` | 13 | approach-only | RISK (runner time/memory) | 866 MiB / 58,004 files: over the RISK line (>300 MiB or >15k files); locally 18.7k files took 22 min and 162 MiB peaked at 10.8 GiB node RSS on 32 cores, the runner has 4 vCPU / 16 GB / 90 min |
| xerj-storage | `e449d171` | 13 | adapt | PASS (cap uncertain) | pack est. 41-139 MiB straddles the cap |
| xerj-vector | `74f3e85b` | 13 | adapt | PASS (over 50 MiB cap) | pack est. 65-222 MiB: builds, but clients never auto-pull it |
| xz | `b355a311` | 13 | approach-only | PASS | pack est. 10.4-35.4 MiB, under cap |
| zalando-restful-api-guidelines | `88cdfdb1` | 13 | adapt | PASS | pack est. 0.5-1.7 MiB, under cap |
| zlib-ng | `a7f51090` | 13 | adapt | PASS | pack est. 13.5-46.1 MiB, under cap |
