# Hub backfill report

Written by `.github/workflows/hub-packs.yml` (the `backfill report` job) after every
publishing run; do not edit by hand. One row per corpus per run; the rows behind it
are in `rows.jsonl`. Spec: docs/CORPUS_HUB_V2_IMPLEMENTATION.md §12.1.

## Current state

247 corpora attempted: 201 published at their latest attempt, 46 not.

### Latest attempt did not publish (fix or delist on purpose)

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `12-factor-agents` | `d20c7283` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `18f-content-guide` | `1b1723d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `a11y-project-content` | `27de2268` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `accord-legal-models` | `a23b30a1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ada-2010` | `d2be9eb3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `adalflow` | `810de99d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agentbench` | `d1e4a10d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agentscope` | `812eaecd` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aider` | `5dc9490b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aioquic` | `974e9349` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `alex-inclusive-language` | `9a57595d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ann-benchmarks` | `2e081ad3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `annoy` | `379f7446` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `anything-llm` | `3e4f9411` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-common-protos` | `3332dec5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-linter` | `836098d5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `arazzo-spec` | `79ddb24a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aria-practices` | `0f765e4d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `astro-docs` | `f29faa41` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-bindings` | `1f418fe7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-spec` | `1dd65fd2` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-spec-json-schemas` | `00af2905` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-studio` | `f3aa74ec` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `atomic-agents` | `4d443474` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-flow` | `8b6ca28b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-mappings-explorer` | `a85e55af` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autogen` | `027ecf0a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autorag` | `ccb70c12` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `avro` | `28cb08c1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-ai-system-prompts` | `74964625` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `axe-core` | `7e287a6a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `axolotl` | `e767fe07` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `biome` | `39d7595b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `googleapis` | `dfaf98d4` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

## Runs (newest first)

### Run [39306424180](https://github.com/xerj-org/xerj/actions/runs/39306424180), 2026-10-19: 26 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `biome` | `39d7595b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `googleapis` | `dfaf98d4` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agno` | `1f2aac51` | 68832624 | 66626 | published `pack-agno-1f2aac51` |  |
| `boulder` | `f877538d` | 35896597 | 155376 | published `pack-boulder-f877538d` |  |
| `cfssl` | `ab47f0d9` | 26318594 | 111353 | published `pack-cfssl-ab47f0d9` |  |
| `chatdoctor` | `e77bd7eb` | 87376030 | 59 | published `pack-chatdoctor-e77bd7eb-r6` |  |
| `containerd` | `92f321c8` | 58403782 | 192996 | published `pack-containerd-92f321c8` |  |
| `cri-o` | `617265f3` | 43852494 | 250869 | published `pack-cri-o-617265f3` |  |
| `dagger` | `96a18ac7` | 97413863 | 156244 | published `pack-dagger-96a18ac7` |  |
| `dnscrypt-proxy` | `ade0c1d6` | 14056698 | 108966 | published `pack-dnscrypt-proxy-ade0c1d6` |  |
| `duckdb` | `2866f801` | 344313384 | 69666 | published `pack-duckdb-2866f801` |  |
| `envoy` | `eb32f1f9` | 133959953 | 100656 | published `pack-envoy-eb32f1f9` |  |
| `fory` | `9cc2a6ac` | 107175916 | 57997 | published `pack-fory-9cc2a6ac` |  |
| `goss` | `f1a60436` | 3264112 | 1269 | published `pack-goss-f1a60436` |  |
| `got` | `50c4ac61` | 5033166 | 1502 | published `pack-got-50c4ac61` |  |
| `govuk-design-system` | `9bd52fc9` | 1534136 | 0 | published `pack-govuk-design-system-9bd52fc9` |  |
| `gpt-engineer` | `a90fcd54` | 4455049 | 542 | published `pack-gpt-engineer-a90fcd54` |  |
| `grafana-alloy` | `e5a50dfc` | 39920111 | 19977 | published `pack-grafana-alloy-e5a50dfc` |  |
| `graphql-composite-schemas-spec` | `e5ce4835` | 1839455 | 0 | published `pack-graphql-composite-schemas-spec-e5ce4835` |  |
| `graphql-over-http` | `d438b209` | 2160384 | 0 | published `pack-graphql-over-http-d438b209` |  |
| `graphql-spec` | `3f1ef846` | 3245525 | 26 | published `pack-graphql-spec-3f1ef846` |  |
| `graphrag` | `5faaaf4f` | 23759050 | 2734 | published `pack-graphrag-5faaaf4f` |  |
| `griptape` | `aa95c599` | 33682281 | 9278 | published `pack-griptape-aa95c599` |  |
| `grpc-gateway` | `14c1da7c` | 26435644 | 15694 | published `pack-grpc-gateway-14c1da7c` |  |
| `grpc-go` | `de20bed5` | 18333374 | 16455 | published `pack-grpc-go-de20bed5` |  |
| `grpc-proposals` | `6342be72` | 12072027 | 0 | published `pack-grpc-proposals-6342be72` |  |
| `guardrails` | `06d0ff2c` | 5077419 | 2517 | published `pack-guardrails-06d0ff2c` |  |
| `guidance` | `21b1d90d` | 4325955 | 1818 | published `pack-guidance-21b1d90d` |  |

### Run [39263621696](https://github.com/xerj-org/xerj/actions/runs/39263621696), 2026-10-19: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792419972-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792419977-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":13,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792419972-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792420015-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792420047-defs/_bulk -> HTTP 429: {"took":1,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792420047-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792420159-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1," |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":14,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792421131-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dagger` | `96a18ac7` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":15,"errors":true,"items":[{"index":{"_index":"xc-dagger-b1792421185-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `dnscrypt-proxy` | `ade0c1d6` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-dnscrypt-proxy-b1792421374-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term" |
| `duckdb` | `2866f801` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-duckdb-b1792421425-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `envoy` | `eb32f1f9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-envoy-b1792421985-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fory` | `9cc2a6ac` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-fory-b1792426139-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `freebsd-handbook` | `3aa6b19a` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | cancelled at `build` | [pin] github-api-description -> 7bdf5f005536 |
| `googleapis` | `dfaf98d4` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `goss` | `f1a60436` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `got` | `50c4ac61` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `govuk-design-system` | `9bd52fc9` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `gpt-engineer` | `a90fcd54` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `grafana-alloy` | `e5a50dfc` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `graphql-composite-schemas-spec` | `e5ce4835` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `graphql-over-http` | `d438b209` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `graphql-spec` | `3f1ef846` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `graphrag` | `5faaaf4f` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `griptape` | `aa95c599` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `grpc-gateway` | `14c1da7c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `grpc-go` | `de20bed5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `grpc-proposals` | `6342be72` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `guardrails` | `06d0ff2c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `guidance` | `21b1d90d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

### Run [39240121636](https://github.com/xerj-org/xerj/actions/runs/39240121636), 2026-10-19: 15 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792407647-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792407656-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792407651-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792407691-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792407727-defs/_bulk -> HTTP 429: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792407727-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":15,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792407843-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792408300-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dagger` | `96a18ac7` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-dagger-b1792408360-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `dnscrypt-proxy` | `ade0c1d6` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-dnscrypt-proxy-b1792408539-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term" |
| `duckdb` | `2866f801` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-duckdb-b1792408579-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `envoy` | `eb32f1f9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-envoy-b1792408789-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fory` | `9cc2a6ac` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-fory-b1792412790-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `googleapis` | `dfaf98d4` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `go-stomp` | `9eb558af` | 1053768 | 494 | published `pack-go-stomp-9eb558af` |  |
| `go-tpm` | `e05fb8b1` | 4822047 | 2978 | published `pack-go-tpm-e05fb8b1` |  |
| `go-vulndb` | `5cd8418c` | 135708681 | 2180 | published `pack-go-vulndb-5cd8418c` |  |
| `golang-jwt` | `73c870b1` | 1026480 | 378 | published `pack-golang-jwt-73c870b1` |  |
| `golang-x-crypto` | `3b414e3e` | 16947365 | 4210 | published `pack-golang-x-crypto-3b414e3e` |  |
| `good-docs-templates` | `de840b37` | 24671402 | 0 | published `pack-good-docs-templates-de840b37` |  |
| `google-aip` | `615875bf` | 4005382 | 0 | published `pack-google-aip-615875bf` |  |
| `google-devdocs-vale` | `9e2483e2` | 315508 | 10 | published `pack-google-devdocs-vale-9e2483e2` |  |
| `google-eng-practices` | `3bb3ec25` | 585516 | 0 | published `pack-google-eng-practices-3bb3ec25` |  |
| `google-fuzzing` | `734e55f3` | 1946491 | 58 | published `pack-google-fuzzing-734e55f3` |  |
| `google-licenseclassifier` | `3cfbab2d` | 14023349 | 774 | published `pack-google-licenseclassifier-3cfbab2d` |  |
| `gopass` | `1e831b4c` | 12964876 | 4004 | published `pack-gopass-1e831b4c` |  |
| `goreleaser` | `e6c4afca` | 8966217 | 4459 | published `pack-goreleaser-e6c4afca` |  |
| `gorilla` | `6ea57973` | 261168885 | 2523 | published `pack-gorilla-6ea57973` |  |
| `gosec-rules` | `71577268` | 6373377 | 1572 | published `pack-gosec-rules-71577268` |  |

### Run [39218075283](https://github.com/xerj-org/xerj/actions/runs/39218075283), 2026-10-19: 16 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792396258-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792396262-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792396255-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792396296-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792396334-defs/_bulk -> HTTP 429: {"took":2,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792396334-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792396440-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792397333-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dagger` | `96a18ac7` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-dagger-b1792397394-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dnscrypt-proxy` | `ade0c1d6` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-dnscrypt-proxy-b1792397562-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term" |
| `duckdb` | `2866f801` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-duckdb-b1792397599-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `envoy` | `eb32f1f9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-envoy-b1792398791-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | failed at `build` | xerj hub build-pack: `corpus index` failed (exit 1) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fory` | `9cc2a6ac` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-fory-b1792401205-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `freebsd-handbook` | `3aa6b19a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ghsa-db` | `4b4ac300` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `github-api-description` | `7bdf5f00` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fortio` | `67ce9fa2` | 1607497 | 1081 | published `pack-fortio-67ce9fa2` |  |
| `garak` | `cd87893b` | 28168713 | 3875 | published `pack-garak-cd87893b` |  |
| `gatekeeper-library` | `f8e3b4e9` | 9922385 | 73 | published `pack-gatekeeper-library-f8e3b4e9` |  |
| `gateway-api` | `0da5a10e` | 41434412 | 4873 | published `pack-gateway-api-0da5a10e` |  |
| `gatling` | `d552826b` | 7444636 | 9511 | published `pack-gatling-d552826b` |  |
| `gdpr-commonaccord` | `cd623e0d` | 474748 | 22 | published `pack-gdpr-commonaccord-cd623e0d` |  |
| `gemini-cookbook` | `dd5b15da` | 13713934 | 351 | published `pack-gemini-cookbook-dd5b15da` |  |
| `getrandom` | `eeb6a3d4` | 895286 | 212 | published `pack-getrandom-eeb6a3d4` |  |
| `giskard` | `48814003` | 12699289 | 3874 | published `pack-giskard-48814003` |  |
| `github-opensource-guide` | `e7ac98e9` | 7874160 | 120 | published `pack-github-opensource-guide-e7ac98e9` |  |
| `github-site-policy` | `b9578b54` | 3347414 | 0 | published `pack-github-site-policy-b9578b54` |  |
| `gitlab-runbooks` | `caae4426` | 208752683 | 1066 | published `pack-gitlab-runbooks-caae4426` |  |
| `glasskube` | `b0d877db` | 6396077 | 1731 | published `pack-glasskube-b0d877db` |  |
| `gnark` | `3ac4de3e` | 15705661 | 10714 | published `pack-gnark-3ac4de3e` |  |
| `gnostic` | `baf14b96` | 8328820 | 1272 | published `pack-gnostic-baf14b96` |  |
| `go-jose` | `13b7118f` | 1988372 | 1485 | published `pack-go-jose-13b7118f` |  |

### Run [39203376748](https://github.com/xerj-org/xerj/actions/runs/39203376748), 2026-10-19: 20 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792387923-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792387927-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792387924-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792387970-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792388004-defs/_bulk -> HTTP 429: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792388004-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792388113-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":19,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792388328-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dagger` | `96a18ac7` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-dagger-b1792388383-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dnscrypt-proxy` | `ade0c1d6` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-dnscrypt-proxy-b1792388498-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term" |
| `duckdb` | `2866f801` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-duckdb-b1792388532-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `envoy` | `eb32f1f9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-envoy-b1792388878-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `exploit-pocs-2023` | `af9e194d` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2024` | `7855e58f` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2025` | `374cf8f1` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploit-pocs-2026` | `160f470e` |  |  | failed at `build` | xerj hub build-pack: `corpus add` failed (exit 1) |
| `exploitdb-src` | `12879ca7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `fluent-bit` | `3b6ce5cf` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `etherparse` | `70f72bee` | 9012116 | 3793 | published `pack-etherparse-70f72bee` |  |
| `eurlex-core` | `f407c938` | 9135342 | 16 | published `pack-eurlex-core-f407c938` |  |
| `external-secrets` | `4aaa3159` | 40778181 | 10129 | published `pack-external-secrets-4aaa3159` |  |
| `eyecite` | `b70a146d` | 2605064 | 331 | published `pack-eyecite-b70a146d` |  |
| `fabric` | `177b3a9d` | 32243057 | 2237 | published `pack-fabric-177b3a9d` |  |
| `faiss` | `0db2f308` | 32757782 | 12391 | published `pack-faiss-0db2f308` |  |
| `falco-rules` | `21ef6d5f` | 1870384 | 149 | published `pack-falco-rules-21ef6d5f` |  |
| `fastchat` | `587d5cfa` | 5045861 | 1626 | published `pack-fastchat-587d5cfa` |  |
| `fastcrypto` | `f8b24b80` | 4474089 | 3073 | published `pack-fastcrypto-f8b24b80` |  |
| `fastmcp` | `e2fcc412` | 64270304 | 18367 | published `pack-fastmcp-e2fcc412` |  |
| `fastrag` | `ab3d19d5` | 2237003 | 536 | published `pack-fastrag-ab3d19d5` |  |
| `fda-food-code` | `30a28444` | 14416437 | 0 | published `pack-fda-food-code-30a28444` |  |
| `fhir-test-cases` | `4e3b2480` | 109828456 | 228 | published `pack-fhir-test-cases-4e3b2480` |  |
| `flagembedding` | `fd1a2bdf` | 92227919 | 3794 | published `pack-flagembedding-fd1a2bdf` |  |
| `flatbuffers` | `b8431fbc` | 38266071 | 33166 | published `pack-flatbuffers-b8431fbc` |  |
| `flexrag` | `2b6e32c3` | 67185455 | 1234 | published `pack-flexrag-2b6e32c3` |  |
| `fluentd` | `358c92bb` | 12708171 | 6310 | published `pack-fluentd-358c92bb` |  |
| `flux2` | `a0c663ab` | 8323477 | 2024 | published `pack-flux2-a0c663ab` |  |
| `fluxcd-website` | `8aa27f0f` | 9054653 | 59 | published `pack-fluxcd-website-8aa27f0f` |  |
| `foodon` | `cd735402` | 210218794 | 376 | published `pack-foodon-cd735402` |  |

### Run [39195023220](https://github.com/xerj-org/xerj/actions/runs/39195023220), 2026-10-19: 26 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792381421-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":10,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792381425-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792381420-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792381465-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792381497-defs/_bulk -> HTTP 429: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792381497-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":16,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792381586-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792382522-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dagger` | `96a18ac7` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-dagger-b1792382577-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `dnscrypt-proxy` | `ade0c1d6` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-dnscrypt-proxy-b1792382853-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term" |
| `duckdb` | `2866f801` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-duckdb-b1792383053-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `envoy` | `eb32f1f9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-envoy-b1792384081-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `density` | `58938943` | 51514224 | 199 | published `pack-density-58938943` |  |
| `dgraph` | `317411f2` | 24678726 | 11589 | published `pack-dgraph-317411f2` |  |
| `digitalocean-openapi` | `0dd84700` | 12975301 | 0 | published `pack-digitalocean-openapi-0dd84700` |  |
| `diskann` | `177c03be` | 64486239 | 17298 | published `pack-diskann-177c03be` |  |
| `dispatch` | `dd2837e8` | 23213156 | 6630 | published `pack-dispatch-dd2837e8` |  |
| `distilabel` | `313fac85` | 4219980 | 2554 | published `pack-distilabel-313fac85` |  |
| `docassemble` | `fea293b7` | 145235243 | 26347 | published `pack-docassemble-fea293b7` |  |
| `docker-docs` | `db384a4b` | 97278436 | 157 | published `pack-docker-docs-db384a4b` |  |
| `dragonboat` | `076c7f64` | 8889944 | 4966 | published `pack-dragonboat-076c7f64` |  |
| `drpc` | `92689768` | 3020519 | 1839 | published `pack-drpc-92689768` |  |
| `dspy` | `099982b3` | 41227146 | 8779 | published `pack-dspy-099982b3` |  |
| `dwyl-english-words` | `20f5cc9b` | 45640664 | 4 | published `pack-dwyl-english-words-20f5cc9b` |  |
| `ecfr-title-12` | `4dc0246c` | 123249158 | 0 | published `pack-ecfr-title-12-4dc0246c` |  |
| `ecfr-title-14` | `0520b312` | 57580240 | 0 | published `pack-ecfr-title-14-0520b312` |  |
| `ecfr-title-26` | `3f3e58c7` | 308683055 | 0 | published `pack-ecfr-title-26-3f3e58c7` |  |
| `ecfr-title-29` | `bf0c9b74` | 118993217 | 0 | published `pack-ecfr-title-29-bf0c9b74` |  |
| `ecma262` | `a51d83bd` | 13580002 | 0 | published `pack-ecma262-a51d83bd` |  |
| `edliz-zimbabwe-treatment-guidelines` | `73b89bdc` | 1879537 | 165 | published `pack-edliz-zimbabwe-treatment-guidelines-73b89bdc` |  |
| `ell` | `9d129846` | 10349317 | 1451 | published `pack-ell-9d129846` |  |
| `emp-tool` | `bd036a9e` | 6229978 | 2160 | published `pack-emp-tool-bd036a9e` |  |
| `enet` | `5a9c537f` | 1356452 | 419 | published `pack-enet-5a9c537f` |  |
| `errata-ai-readability-vale` | `20b367bc` | 112854 | 1 | published `pack-errata-ai-readability-vale-20b367bc` |  |
| `esbuild` | `f6058f83` | 28395479 | 7879 | published `pack-esbuild-f6058f83` |  |
| `eset-malware-ioc` | `705ead5a` | 10001388 | 23 | published `pack-eset-malware-ioc-705ead5a` |  |
| `etcd-docs` | `d20f8ae3` | 20847879 | 2 | published `pack-etcd-docs-d20f8ae3` |  |
| `etcd-src` | `64f26db4` | 37690733 | 12546 | published `pack-etcd-src-64f26db4` |  |

### Run [39184928518](https://github.com/xerj-org/xerj/actions/runs/39184928518), 2026-10-19: 29 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792373835-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792374096-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792374254-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `cassandra` | `ec2c276b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-cassandra-b1792374869-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"s |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792375472-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792375651-defs/_bulk -> HTTP 429: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792375651-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":14,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792376376-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":19,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792376461-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `dagger` | `96a18ac7` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-dagger-b1792376964-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `adyen-openapi` | `45520ee6` | 105094075 | 0 | published `pack-adyen-openapi-45520ee6-r17` |  |
| `aeron` | `d3499bb1` | 52527532 | 28139 | published `pack-aeron-d3499bb1-r17` |  |
| `anthropic-cookbook` | `9c1e493d` | 85014750 | 901 | published `pack-anthropic-cookbook-9c1e493d-r17` |  |
| `argo-cd` | `03a10c74` | 89567425 | 25467 | published `pack-argo-cd-03a10c74-r17` |  |
| `argo-workflows` | `8f57ec73` | 91713431 | 14769 | published `pack-argo-workflows-8f57ec73-r17` |  |
| `arrow-rs` | `73595d58` | 56262718 | 24047 | published `pack-arrow-rs-73595d58-r17` |  |
| `awesome-llm-apps` | `4bf51ab7` | 56873452 | 7418 | published `pack-awesome-llm-apps-4bf51ab7-r16` |  |
| `bookkeeper` | `773a988d` | 68205408 | 29420 | published `pack-bookkeeper-773a988d-r14` |  |
| `botan` | `9fc28d5b` | 158366827 | 18513 | published `pack-botan-9fc28d5b-r14` |  |
| `calcite` | `8f27e24a` | 108937810 | 49987 | published `pack-calcite-8f27e24a-r14` |  |
| `camel` | `e5e55256` | 265285745 | 10924 | published `pack-camel-e5e55256-r13` |  |
| `cdk8s` | `ed80049b` | 105184382 | 127 | published `pack-cdk8s-ed80049b-r13` |  |
| `cell-ontology` | `b7cb31d4` | 197950867 | 67 | published `pack-cell-ontology-b7cb31d4-r13` |  |
| `checkov` | `456c7e53` | 87257813 | 19706 | published `pack-checkov-456c7e53-r12` |  |
| `chroma` | `5f30478c` | 73037625 | 22136 | published `pack-chroma-5f30478c-r12` |  |
| `cloud-custodian` | `a75de191` | 109667276 | 18800 | published `pack-cloud-custodian-a75de191-r12` |  |
| `cloudnative-pg` | `f2a7162e` | 52739148 | 6086 | published `pack-cloudnative-pg-f2a7162e-r12` |  |
| `cluster-api` | `557ad73a` | 74192551 | 16324 | published `pack-cluster-api-557ad73a-r12` |  |
| `complianceascode-content` | `f0f3396f` | 104701056 | 3975 | published `pack-complianceascode-content-f0f3396f-r11` |  |
| `ctakes` | `80e0d8e9` | 53144902 | 17779 | published `pack-ctakes-80e0d8e9-r8` |  |
| `cue` | `71c80626` | 55211693 | 12842 | published `pack-cue-71c80626-r7` |  |
| `curl-src` | `39ad9b07` | 66682937 | 26667 | published `pack-curl-src-39ad9b07` |  |
| `curve25519-dalek` | `97a020d1` | 6889084 | 1693 | published `pack-curve25519-dalek-97a020d1` |  |
| `cyclonedx-spec` | `ad83dae2` | 14097467 | 110 | published `pack-cyclonedx-spec-ad83dae2` |  |
| `dailymed` | `21a87b3c` | 89890311 | 16 | published `pack-dailymed-21a87b3c` |  |
| `datafusion` | `45de64cf` | 62418252 | 48976 | published `pack-datafusion-45de64cf` |  |
| `datasketches-cpp` | `9a7226e3` | 7364616 | 1274 | published `pack-datasketches-cpp-9a7226e3` |  |
| `deepeval` | `016946f6` | 32359954 | 15473 | published `pack-deepeval-016946f6` |  |
| `delta-rs` | `ad4964d5` | 21063694 | 7998 | published `pack-delta-rs-ad4964d5` |  |

### Run [39168442906](https://github.com/xerj-org/xerj/actions/runs/39168442906), 2026-10-18: 9 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127055457 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 54757451 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792365740-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84941896 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `argo-cd` | `03a10c74` | 96689311 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91732115 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56304083 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56855143 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":10,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792366000-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `bookkeeper` | `773a988d` | 68443056 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 208227199 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792366098-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `calcite` | `8f27e24a` | 108947530 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `camel` | `e5e55256` | 265268553 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105405971 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196722547 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792366831-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` | 87363417 | 59 | failed at `e2e` | xerj hub pull chatdoctor failed (exit 1) |
| `checkov` | `456c7e53` | 86927005 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73054583 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `cloud-custodian` | `a75de191` | 109540764 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52711424 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74183375 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `complianceascode-content` | `f0f3396f` | 108474501 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792368608-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1," |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792369493-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `ctakes` | `80e0d8e9` | 53132719 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `cue` | `71c80626` | 55198417 | 12842 | failed at `e2e` | xerj hub pull cue failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660906 | 0 | published `pack-api-blueprint-86fc3a12-r16` |  |
| `awesome-mcp-servers` | `cc7f1f42` | 2809562 | 0 | published `pack-awesome-mcp-servers-cc7f1f42-r15` |  |
| `awesome-sre` | `e91ef69a` | 116118 | 0 | published `pack-awesome-sre-e91ef69a-r14` |  |
| `brex-prompt-engineering` | `50871be1` | 475057 | 0 | published `pack-brex-prompt-engineering-50871be1-r13` |  |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8244443 | 0 | published `pack-caliber-ehr-phenotypes-f4f614fc-r13` |  |
| `clipboard-apis` | `6201fd15` | 825705 | 0 | published `pack-clipboard-apis-6201fd15-r11` |  |
| `commonpaper-contracts` | `3efceb01` | 252233 | 0 | published `pack-commonpaper-contracts-3efceb01-r11` |  |
| `compose-spec` | `914ec15d` | 1938903 | 0 | published `pack-compose-spec-914ec15d-r10` |  |
| `csp` | `b1bad631` | 5846707 | 0 | published `pack-csp-b1bad631-r8` |  |

### Run [39164946551](https://github.com/xerj-org/xerj/actions/runs/39164946551), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 105269353 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 54876769 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792358109-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84942163 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 659879 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 89553846 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91716623 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56292975 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56880189 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809384 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115129 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792358673-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68215418 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 159684879 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792358670-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `brex-prompt-engineering` | `50871be1` | 473226 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108821303 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8256024 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265295558 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105227049 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 198335660 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792359194-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792359231-defs/_bulk -> HTTP 429: {"took":0,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792359231-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `checkov` | `456c7e53` | 87138705 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73047214 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 912998 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 101559426 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52658022 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74186246 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 252064 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 104566261 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1939140 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":14,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792360873-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792360951-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5847969 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `ctakes` | `80e0d8e9` | 53119604 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `cue` | `71c80626` | 57875860 | 12842 | failed at `e2e` | xerj hub pull cue failed (exit 1) |

### Run [39157103974](https://github.com/xerj-org/xerj/actions/runs/39157103974), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127207904 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52537248 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792350675-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84952816 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660832 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 96851057 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91713349 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56245451 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56883875 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2808842 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115874 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":11,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792350896-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `bookkeeper` | `773a988d` | 68708520 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 215320845 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":13,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792351014-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 474949 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 89948953 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8248796 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 264864991 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 89841978 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 195687394 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792351453-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792351483-defs/_bulk -> HTTP 429: {"took":0,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792351483-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `checkov` | `456c7e53` | 86908249 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73046715 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 824945 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 107883402 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52716760 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74189388 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 252087 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 112228323 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1938623 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":16,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792352958-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":11,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792353209-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5843167 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `ctakes` | `80e0d8e9` | 53153909 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `cue` | `71c80626` | 55207687 | 12842 | failed at `e2e` | xerj hub pull cue failed (exit 1) |

### Run [39150321799](https://github.com/xerj-org/xerj/actions/runs/39150321799), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 104723591 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52511685 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792345249-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84904524 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660921 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 116027323 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91709371 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56255231 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56847250 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809470 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115417 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792345539-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68497492 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 143908066 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792345639-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `brex-prompt-engineering` | `50871be1` | 474997 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 89951644 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8242442 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265401173 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105262073 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 195543842 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792346076-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` | 87202425 | 59 | failed at `e2e` | xerj hub pull chatdoctor failed (exit 1) |
| `checkov` | `456c7e53` | 86958980 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73043116 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 824953 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 108003562 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52715897 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74184120 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 252111 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 116066402 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1939287 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":16,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792347277-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":10,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792349136-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5843592 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `ctakes` | `80e0d8e9` | 53175104 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `cue` | `71c80626` | 57938408 | 12842 | failed at `e2e` | xerj hub pull cue failed (exit 1) |

### Run [39143681844](https://github.com/xerj-org/xerj/actions/runs/39143681844), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127679259 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 54786342 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792340009-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84955555 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660538 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 116074509 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91700136 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56269130 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56884946 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2806182 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115166 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792340239-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68216839 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 215159331 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792340390-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 473217 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108905266 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8253700 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265382655 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105241671 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 195665265 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792340751-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` | 87196294 | 59 | failed at `e2e` | xerj hub pull chatdoctor failed (exit 1) |
| `checkov` | `456c7e53` | 86879962 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73055880 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 914434 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 108252597 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52774499 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74182439 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 252020 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 116079040 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1939086 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792341864-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1," |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792341923-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5844813 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `ctakes` | `80e0d8e9` | 53151760 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `cue` | `71c80626` | 57900563 | 12842 | failed at `e2e` | xerj hub pull cue failed (exit 1) |

### Run [39134588401](https://github.com/xerj-org/xerj/actions/runs/39134588401), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 126960436 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52533050 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792332835-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84952353 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660357 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 89675248 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91723997 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56224466 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56874481 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809167 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115143 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792333542-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 66617715 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 136280066 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792333798-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 473218 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 151971261 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8252746 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265562482 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105197459 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 195507029 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792334669-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792334771-defs/_bulk -> HTTP 429: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792334771-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `checkov` | `456c7e53` | 87209289 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73053867 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 914173 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 109620666 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52699766 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74342965 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 251803 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 108496603 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1939597 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":15,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792336006-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":19,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792338447-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5844155 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `ctakes` | `80e0d8e9` | 53128565 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `cue` | `71c80626` | 55207467 | 12842 | failed at `e2e` | xerj hub pull cue failed (exit 1) |

### Run [39128294372](https://github.com/xerj-org/xerj/actions/runs/39128294372), 2026-10-18: 1 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127754427 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52538358 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792328145-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84932498 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660551 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 116107182 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91726225 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56275737 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56865501 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809818 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115470 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":10,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792328708-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `bookkeeper` | `773a988d` | 66876332 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 143946617 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":12,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792328766-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 475043 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108974500 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8245403 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265294230 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105235963 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196634883 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792330206-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792330247-defs/_bulk -> HTTP 429: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792330247-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `checkov` | `456c7e53` | 86690974 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73053753 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 825790 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 101645034 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52721392 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74354486 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 252276 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 116075483 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1939488 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":15,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792331335-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792331566-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5846101 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `ctakes` | `80e0d8e9` | 53151499 | 17779 | failed at `e2e` | xerj hub pull ctakes failed (exit 1) |
| `css-tree` | `f8980154` | 4997128 | 838 | published `pack-css-tree-f8980154` |  |

### Run [39119460093](https://github.com/xerj-org/xerj/actions/runs/39119460093), 2026-10-18: 2 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127054883 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52506084 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":3,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792320651-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84962539 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660923 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 89521495 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91727819 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56250419 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56877968 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809535 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115096 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792321617-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68718373 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 158444121 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792321800-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `brex-prompt-engineering` | `50871be1` | 474892 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108808579 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8243213 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265118482 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105180400 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196618176 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792322326-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792322362-defs/_bulk -> HTTP 429: {"took":0,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792322362-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `checkov` | `456c7e53` | 86945623 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73062623 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 826180 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 108441864 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52720123 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74189685 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 251941 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 112239089 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1939944 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":14,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792324091-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cri-o` | `617265f3` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":18,"errors":true,"items":[{"index":{"_index":"xc-cri-o-b1792325993-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `csp` | `b1bad631` | 5847308 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cozo` | `481af058` | 17628613 | 2693 | published `pack-cozo-481af058` |  |
| `crossplane` | `b8d0c527` | 22712892 | 5262 | published `pack-crossplane-b8d0c527` |  |

### Run [39110555987](https://github.com/xerj-org/xerj/actions/runs/39110555987), 2026-10-18: 4 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 103934863 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52533393 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792312168-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84913104 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660624 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 96873122 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91735621 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56274112 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56850708 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809291 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115317 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792312959-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68198403 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 212346337 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":11,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792313265-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 474932 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108935687 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8248980 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265221736 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105235764 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196425036 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792314215-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` |  |  | failed at `build` | xerj hub build-pack: bulk /xc-chatdoctor-b1792314746-defs/_bulk -> HTTP 429: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-chatdoctor-b1792314746-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no |
| `checkov` | `456c7e53` | 87968361 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73064046 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 825009 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 108657974 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52691468 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74299660 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 251818 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 108476332 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1940382 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":15,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792315845-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1, |
| `counsel-chat` | `e202e19c` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `coredns` | `235e7947` | 16757807 | 4940 | published `pack-coredns-235e7947` |  |
| `coroot` | `56d1b0bd` | 20929290 | 3102 | published `pack-coroot-56d1b0bd` |  |
| `courts-db` | `c22604df` | 1400281 | 47 | published `pack-courts-db-c22604df` |  |
| `covid-qa` | `fb2eb5e2` | 1479894 | 393 | published `pack-covid-qa-fb2eb5e2` |  |

### Run [39102556680](https://github.com/xerj-org/xerj/actions/runs/39102556680), 2026-10-18: 5 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127473272 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52535449 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792305456-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84910421 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660776 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 116071397 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91721550 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56270735 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56954142 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2806255 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115572 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792306926-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68732165 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 160625486 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":14,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792307383-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 474868 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 89942198 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8249845 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265406839 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105186818 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196662321 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792308071-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` | 87316066 | 59 | failed at `e2e` | xerj hub pull chatdoctor failed (exit 1) |
| `checkov` | `456c7e53` | 94365294 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73034093 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 826688 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 101363850 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52791301 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74195491 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 251786 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `complianceascode-content` | `f0f3396f` | 112320807 | 3975 | failed at `e2e` | xerj hub pull complianceascode-content failed (exit 1) |
| `compose-spec` | `914ec15d` | 1938414 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `concourse` | `6a706fb8` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `containerd` | `92f321c8` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-containerd-b1792309436-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1," |
| `congress-legislators` | `d5af3d2d` | 47962068 | 207 | published `pack-congress-legislators-d5af3d2d` |  |
| `connect-go` | `2b80b7c5` | 6714111 | 2930 | published `pack-connect-go-2b80b7c5` |  |
| `connect-protocol` | `088d64e1` | 4260285 | 24 | published `pack-connect-protocol-088d64e1` |  |
| `controlflow` | `a7ffb12f` | 6980613 | 1175 | published `pack-controlflow-a7ffb12f` |  |
| `conventional-commits` | `7d293dc5` | 4315265 | 7 | published `pack-conventional-commits-7d293dc5` |  |

### Run [39097676164](https://github.com/xerj-org/xerj/actions/runs/39097676164), 2026-10-18: 9 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127102628 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52542937 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":8,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792298953-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84915000 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660422 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 95531268 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91730897 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56263452 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56876604 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809708 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115271 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792299176-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68709201 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 143693735 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792299275-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"sta |
| `brex-prompt-engineering` | `50871be1` | 475042 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108973137 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8257970 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265338153 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `cdk8s` | `ed80049b` | 105094728 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196618322 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":9,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792299668-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `chatdoctor` | `e77bd7eb` | 87216295 | 59 | failed at `e2e` | xerj hub pull chatdoctor failed (exit 1) |
| `checkov` | `456c7e53` | 86965098 | 19706 | failed at `e2e` | xerj hub pull checkov failed (exit 1) |
| `chroma` | `5f30478c` | 73048727 | 22136 | failed at `e2e` | xerj hub pull chroma failed (exit 1) |
| `clipboard-apis` | `6201fd15` | 825895 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `cloud-custodian` | `a75de191` | 103832133 | 18800 | failed at `e2e` | xerj hub pull cloud-custodian failed (exit 1) |
| `cloudnative-pg` | `f2a7162e` | 52713985 | 6086 | failed at `e2e` | xerj hub pull cloudnative-pg failed (exit 1) |
| `cluster-api` | `557ad73a` | 74178316 | 16324 | failed at `e2e` | xerj hub pull cluster-api failed (exit 1) |
| `commonpaper-contracts` | `3efceb01` | 252256 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `chdb` | `a2d579a2` | 35658820 | 18998 | published `pack-chdb-a2d579a2` |  |
| `choosealicense-data` | `f717b235` | 3825117 | 49 | published `pack-choosealicense-data-f717b235` |  |
| `chronicle-detection-rules` | `234e9223` | 8921576 | 430 | published `pack-chronicle-detection-rules-234e9223` |  |
| `cicero-contract-templates` | `89d82efc` | 15737811 | 2387 | published `pack-cicero-contract-templates-89d82efc` |  |
| `circl` | `91db3e78` | 46752405 | 6698 | published `pack-circl-91db3e78` |  |
| `cisa` | `edfa3694` | 15305050 | 35 | published `pack-cisa-edfa3694` |  |
| `cloudevents-spec` | `2ed3806b` | 3613226 | 79 | published `pack-cloudevents-spec-2ed3806b` |  |
| `cognita` | `8bcaae79` | 3143935 | 721 | published `pack-cognita-8bcaae79` |  |
| `commonaccord-legal-docs` | `c2e864fd` | 16110759 | 37 | published `pack-commonaccord-legal-docs-c2e864fd` |  |

### Run [39091961868](https://github.com/xerj-org/xerj/actions/runs/39091961868), 2026-10-18: 17 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 128802151 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 54753628 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":6,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792295323-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84900043 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660828 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 96831097 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91719093 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56234430 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56867640 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2808990 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115178 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":7,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792295599-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `bookkeeper` | `773a988d` | 68217531 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 143697339 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":11,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792295713-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 473150 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108815484 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8251949 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `camel` | `e5e55256` | 265425403 | 10924 | failed at `e2e` | xerj hub pull camel failed (exit 1) |
| `cassandra` | `ec2c276b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":10,"errors":true,"items":[{"index":{"_index":"xc-cassandra-b1792296135-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1," |
| `cdk8s` | `ed80049b` | 105196359 | 127 | failed at `e2e` | xerj hub pull cdk8s failed (exit 1) |
| `cell-ontology` | `b7cb31d4` | 196856121 | 67 | failed at `e2e` | xerj hub pull cell-ontology failed (exit 1) |
| `cfssl` | `ab47f0d9` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":5,"errors":true,"items":[{"index":{"_index":"xc-cfssl-b1792296480-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"statu |
| `candle` | `c68b2499` | 31473651 | 12753 | published `pack-candle-c68b2499` |  |
| `caniuse` | `e951c6cd` | 6980429 | 3 | published `pack-caniuse-e951c6cd` |  |
| `cap-examples` | `9ef3b4d0` | 1003421 | 54 | published `pack-cap-examples-9ef3b4d0` |  |
| `capa-rules` | `805f9eac` | 2229324 | 12 | published `pack-capa-rules-805f9eac` |  |
| `capnproto` | `e866bdba` | 38671480 | 4892 | published `pack-capnproto-e866bdba` |  |
| `carbon` | `c9b1f791` | 46085538 | 11349 | published `pack-carbon-c9b1f791` |  |
| `catala-law-lang` | `5a41577e` | 14937095 | 6199 | published `pack-catala-law-lang-5a41577e` |  |
| `cdc-clinical` | `3bd1ba62` | 5920750 | 12 | published `pack-cdc-clinical-3bd1ba62` |  |
| `cel-spec` | `40a3c900` | 4524562 | 1663 | published `pack-cel-spec-40a3c900` |  |
| `cereal` | `22a1b369` | 5998963 | 3132 | published `pack-cereal-22a1b369` |  |
| `cert-manager` | `ef9ac738` | 35355153 | 8129 | published `pack-cert-manager-ef9ac738` |  |
| `certmagic` | `cfd20205` | 2154445 | 631 | published `pack-certmagic-cfd20205` |  |
| `certstrap` | `f392bdae` | 906673 | 293 | published `pack-certstrap-f392bdae` |  |
| `cfn-nag` | `8b5f03da` | 4550174 | 1323 | published `pack-cfn-nag-8b5f03da` |  |
| `chamber` | `5f93f5f3` | 1126572 | 471 | published `pack-chamber-5f93f5f3` |  |
| `chaos-mesh` | `780cb80c` | 23381806 | 7320 | published `pack-chaos-mesh-780cb80c` |  |
| `chaostoolkit-docs` | `d113ae82` | 1497001 | 10 | published `pack-chaostoolkit-docs-d113ae82` |  |

### Run [39091860082](https://github.com/xerj-org/xerj/actions/runs/39091860082), 2026-10-18: 22 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `adyen-openapi` | `45520ee6` | 127033718 | 0 | failed at `e2e` | xerj hub pull adyen-openapi failed (exit 1) |
| `aeron` | `d3499bb1` | 52531523 | 28139 | failed at `e2e` | xerj hub pull aeron failed (exit 1) |
| `agno` | `1f2aac51` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":4,"errors":true,"items":[{"index":{"_index":"xc-agno-b1792292503-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"status |
| `anthropic-cookbook` | `9c1e493d` | 84939283 | 901 | failed at `e2e` | xerj hub pull anthropic-cookbook failed (exit 1) |
| `api-blueprint` | `86fc3a12` | 660629 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `argo-cd` | `03a10c74` | 116015334 | 25467 | failed at `e2e` | xerj hub pull argo-cd failed (exit 1) |
| `argo-workflows` | `8f57ec73` | 91730049 | 14769 | failed at `e2e` | xerj hub pull argo-workflows failed (exit 1) |
| `arrow-rs` | `73595d58` | 56254615 | 24047 | failed at `e2e` | xerj hub pull arrow-rs failed (exit 1) |
| `awesome-llm-apps` | `4bf51ab7` | 56861150 | 7418 | failed at `e2e` | xerj hub pull awesome-llm-apps failed (exit 1) |
| `awesome-mcp-servers` | `cc7f1f42` | 2809865 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `awesome-sre` | `e91ef69a` | 115543 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `biome` | `39d7595b` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":10,"errors":true,"items":[{"index":{"_index":"xc-biome-b1792292746-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"stat |
| `bookkeeper` | `773a988d` | 68757576 | 29420 | failed at `e2e` | xerj hub pull bookkeeper failed (exit 1) |
| `botan` | `9fc28d5b` | 164011813 | 18513 | failed at `e2e` | xerj hub pull botan failed (exit 1) |
| `boulder` | `f877538d` |  |  | failed at `build` | xerj hub build-pack: defs bulk index reported errors: {"took":11,"errors":true,"items":[{"index":{"_index":"xc-boulder-b1792292898-defs","_id":"","_version":1,"_shards":{"total":1,"successful":1,"failed":0},"_seq_no":0,"_primary_term":1,"st |
| `brex-prompt-engineering` | `50871be1` | 474953 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `calcite` | `8f27e24a` | 108806227 | 49987 | failed at `e2e` | xerj hub pull calcite failed (exit 1) |
| `caliber-ehr-phenotypes` | `f4f614fc` | 8256247 | 0 | failed at `e2e` | defs.jsonl has no identifier to look up |
| `badger` | `5688f140` | 4850849 | 1800 | published `pack-badger-5688f140` |  |
| `bandit-plugins` | `68ebe11e` | 2271033 | 1067 | published `pack-bandit-plugins-68ebe11e` |  |
| `bebop` | `03a2a761` | 3694767 | 3075 | published `pack-bebop-03a2a761` |  |
| `blake2` | `ed1974ea` | 2859278 | 675 | published `pack-blake2-ed1974ea` |  |
| `blake3` | `f55849f8` | 4448805 | 1122 | published `pack-blake3-f55849f8` |  |
| `bleve` | `6abe3730` | 42683669 | 5910 | published `pack-bleve-6abe3730` |  |
| `blst` | `19b285a4` | 4992652 | 2824 | published `pack-blst-19b285a4` |  |
| `bluge` | `57414197` | 6564295 | 2485 | published `pack-bluge-57414197` |  |
| `boringtun` | `6dcc889a` | 1185429 | 518 | published `pack-boringtun-6dcc889a` |  |
| `borsh-rs` | `c9a2ed48` | 2849421 | 1260 | published `pack-borsh-rs-c9a2ed48` |  |
| `box-openapi` | `38fa64be` | 3398743 | 18 | published `pack-box-openapi-38fa64be` |  |
| `brotli` | `42a2ed43` | 40961895 | 3157 | published `pack-brotli-42a2ed43` |  |
| `brpc` | `7a2c1d5e` | 46898776 | 17281 | published `pack-brpc-7a2c1d5e` |  |
| `bson-rust` | `81dcdcae` | 3892655 | 2270 | published `pack-bson-rust-81dcdcae` |  |
| `btrblocks` | `c950c3c0` | 4440890 | 655 | published `pack-btrblocks-c950c3c0` |  |
| `buf` | `91f127d3` | 37057776 | 19130 | published `pack-buf-91f127d3` |  |
| `buildpacks-pack` | `99894bcb` | 9899243 | 4292 | published `pack-buildpacks-pack-99894bcb` |  |
| `bulletproofs` | `be67b6d5` | 2161647 | 339 | published `pack-bulletproofs-be67b6d5` |  |
| `c-ares` | `f4156c12` | 10811210 | 4077 | published `pack-c-ares-f4156c12` |  |
| `caddy` | `ac834b5d` | 14497818 | 4633 | published `pack-caddy-ac834b5d` |  |
| `caldera` | `cb8d6a81` | 11038475 | 2989 | published `pack-caldera-cb8d6a81` |  |
| `caldera-stockpile` | `996ec41c` | 2479556 | 743 | published `pack-caldera-stockpile-996ec41c` |  |

### Run [39088277520](https://github.com/xerj-org/xerj/actions/runs/39088277520), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `12-factor-agents` | `d20c7283` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `18f-content-guide` | `1b1723d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `a11y-project-content` | `27de2268` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `accord-legal-models` | `a23b30a1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ada-2010` | `d2be9eb3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `adalflow` | `810de99d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `adyen-openapi` | `45520ee6` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aeron` | `d3499bb1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agentbench` | `d1e4a10d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agentscope` | `812eaecd` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agno` | `1f2aac51` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aider` | `5dc9490b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aioquic` | `974e9349` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `alex-inclusive-language` | `9a57595d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ann-benchmarks` | `2e081ad3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `annoy` | `379f7446` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `anthropic-cookbook` | `9c1e493d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `anything-llm` | `3e4f9411` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-blueprint` | `86fc3a12` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-common-protos` | `3332dec5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-linter` | `836098d5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `arazzo-spec` | `79ddb24a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `argo-cd` | `03a10c74` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `argo-workflows` | `8f57ec73` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aria-practices` | `0f765e4d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `arrow-rs` | `73595d58` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `astro-docs` | `f29faa41` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-bindings` | `1f418fe7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-spec` | `1dd65fd2` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-spec-json-schemas` | `00af2905` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-studio` | `f3aa74ec` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `atomic-agents` | `4d443474` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-flow` | `8b6ca28b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autogen` | `027ecf0a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-ai-system-prompts` | `74964625` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-llm-apps` | `4bf51ab7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-mcp-servers` | `cc7f1f42` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-sre` | `e91ef69a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `axe-core` | `7e287a6a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `axolotl` | `e767fe07` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |

### Run [39085957551](https://github.com/xerj-org/xerj/actions/runs/39085957551), 2026-10-18: 0 of 40 published

| slug | pin8 | bytes | defs | result | error |
|---|---|---:|---:|---|---|
| `12-factor-agents` | `d20c7283` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `18f-content-guide` | `1b1723d3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `a11y-project-content` | `27de2268` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `accord-legal-models` | `a23b30a1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ada-2010` | `d2be9eb3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `adalflow` | `810de99d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `adyen-openapi` | `45520ee6` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aeron` | `d3499bb1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agentbench` | `d1e4a10d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agentscope` | `812eaecd` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `agno` | `1f2aac51` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aider` | `5dc9490b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aioquic` | `974e9349` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `alex-inclusive-language` | `9a57595d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `ann-benchmarks` | `2e081ad3` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `annoy` | `379f7446` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `anthropic-cookbook` | `9c1e493d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `anything-llm` | `3e4f9411` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-blueprint` | `86fc3a12` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-common-protos` | `3332dec5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `api-linter` | `836098d5` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `arazzo-spec` | `79ddb24a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `argo-cd` | `03a10c74` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `argo-workflows` | `8f57ec73` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `aria-practices` | `0f765e4d` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `arrow-rs` | `73595d58` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `astro-docs` | `f29faa41` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-bindings` | `1f418fe7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-spec` | `1dd65fd2` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-spec-json-schemas` | `00af2905` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `asyncapi-studio` | `f3aa74ec` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `atomic-agents` | `4d443474` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-flow` | `8b6ca28b` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `attack-mappings-explorer` | `a85e55af` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autogen` | `027ecf0a` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `autorag` | `ccb70c12` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `avro` | `28cb08c1` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-ai-system-prompts` | `74964625` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-llm-apps` | `4bf51ab7` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
| `awesome-mcp-servers` | `cc7f1f42` |  |  | no result | the pack job recorded no result (pack jobs: failure, build xerj: success) |
